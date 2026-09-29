//! Finance audit trail for the tenant Admin Desk.
//!
//! One read-only timeline over the tables that already record money moving:
//! the store wallet ledger (top-ups, online top-ups, deductions, refunds,
//! order debits and laundry payments), payment-request payments, Razorpay
//! orders and guardian fee payment links. Nothing new is stored; every row
//! comes from its source table, so the trail cannot drift from the ledgers.
//!
//! Tables that belong to optional features are included only when the tenant
//! database has them.

use std::collections::HashMap;

use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    routing::get,
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    error::ApiResult,
    models::ApiResponse,
    operations::require,
    security_logs::{day_bounds, like_pattern},
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub const READ_PERMISSION: &str = "administration.audit_logs.read";

/// Every kind of entry the trail can hold, in the order the filter lists them.
pub const KINDS: [&str; 10] = [
    "top_up",
    "online_top_up",
    "deduction",
    "refund",
    "order_debit",
    "laundry_payment",
    "payment_request",
    "payment_request_issued",
    "online_payment",
    "fee_payment",
];

pub fn router() -> Router<AppState> {
    Router::new().route("/admin/audit-logs", get(list_audit_logs))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuditQuery {
    /// Comma-separated kinds from [`KINDS`].
    kind: Option<String>,
    /// `credit`, `debit` or `payment`.
    direction: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    /// Target user: name, number, email, description or reference.
    q: Option<String>,
    /// Actor: name or email, or `system` for entries with no actor.
    actor: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// Keeps the known kinds from a comma-separated filter. None means "all".
fn parse_kinds(value: Option<&str>) -> Option<Vec<String>> {
    let kinds: Vec<String> = value?
        .split(',')
        .map(str::trim)
        .filter(|kind| KINDS.contains(kind))
        .map(str::to_owned)
        .collect();
    (!kinds.is_empty()).then_some(kinds)
}

#[derive(Clone, Copy, Debug, Default)]
struct Sources {
    laundry: bool,
    payment_requests: bool,
    razorpay_orders: bool,
    fee_links: bool,
}

/// The unified timeline. `$1` is the tenant id; the optional sources are
/// appended as UNION ALL branches with identical columns.
fn timeline_sql(sources: Sources) -> String {
    let laundry_join = if sources.laundry {
        "LEFT JOIN campus_ops.laundry_charges lc ON lc.tenant_id=t.tenant_id AND lc.wallet_transaction_id=t.id"
    } else {
        "LEFT JOIN (SELECT NULL::uuid AS id, NULL::text AS name) lc ON false"
    };
    let mut sql = format!(
        r#"SELECT 'wallet'::text AS source, t.id::text AS id,
                  CASE WHEN lc.id IS NOT NULL THEN 'laundry_payment'
                       WHEN t.transaction_type='manual_top_up' THEN 'top_up'
                       WHEN t.transaction_type='online_top_up' THEN 'online_top_up'
                       WHEN t.transaction_type='manual_debit' THEN 'deduction'
                       ELSE t.transaction_type END AS kind,
                  CASE WHEN t.amount > 0 THEN 'credit' ELSE 'debit' END AS direction,
                  t.user_id, t.actor_user_id, t.amount::float8 AS amount,
                  t.shop_key, shop.name AS shop_name, t.description,
                  t.reference_id AS reference, 'completed'::text AS status,
                  (w.balance - COALESCE(sum(t.amount) OVER (
                      PARTITION BY t.user_id, t.shop_key
                      ORDER BY t.created_at DESC, t.id DESC
                      ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING), 0))::float8 AS balance_after,
                  t.created_at,
                  jsonb_strip_nulls(jsonb_build_object(
                      'transactionType', t.transaction_type,
                      'laundryChargeId', lc.id, 'laundryItem', lc.name)) AS details
           FROM campus_ops.canteen_wallet_transactions t
           LEFT JOIN campus_ops.canteen_wallets w
             ON w.tenant_id=t.tenant_id AND w.user_id=t.user_id AND w.shop_key=t.shop_key
           LEFT JOIN campus_ops.shops shop
             ON shop.tenant_id=t.tenant_id AND shop.shop_key=t.shop_key
           {laundry_join}
           WHERE t.tenant_id=$1"#
    );
    if sources.payment_requests {
        sql.push_str(
            r#"
           UNION ALL
           SELECT 'payment_request', p.id::text, 'payment_request', 'payment',
                  p.user_id, p.marked_by, p.amount::float8, NULL, NULL, r.title,
                  COALESCE(p.payment_reference, p.razorpay_payment_id), p.status, NULL::float8,
                  COALESCE(p.paid_at, p.updated_at),
                  jsonb_strip_nulls(jsonb_build_object(
                      'requestId', r.id, 'purpose', r.purpose, 'method', p.payment_method,
                      'note', p.note, 'razorpayOrderId', p.razorpay_order_id,
                      'razorpayPaymentId', p.razorpay_payment_id))
           FROM campus_ops.payment_request_payers p
           JOIN campus_ops.payment_requests r ON r.id=p.request_id
           WHERE p.tenant_id=$1 AND p.status='paid'
           UNION ALL
           SELECT 'payment_request_issued', r.id::text, 'payment_request_issued', 'payment',
                  NULL, r.created_by, r.amount::float8, NULL, NULL, r.title, NULL, r.status,
                  NULL::float8, r.created_at,
                  jsonb_strip_nulls(jsonb_build_object(
                      'purpose', r.purpose, 'description', r.description, 'dueDate', r.due_date,
                      'targetMode', r.target_mode, 'cancelledBy', r.cancelled_by,
                      'cancelReason', r.cancel_reason, 'cancelledAt', r.cancelled_at))
           FROM campus_ops.payment_requests r
           WHERE r.tenant_id=$1"#,
        );
    }
    if sources.razorpay_orders {
        sql.push_str(
            r#"
           UNION ALL
           SELECT 'razorpay', o.order_id, 'online_payment', 'payment',
                  o.user_id, NULL, (o.amount_paise / 100.0)::float8, o.shop_key, NULL,
                  initcap(replace(o.purpose, '_', ' ')), COALESCE(o.payment_id, o.order_id),
                  o.status, NULL::float8, COALESCE(o.captured_at, o.created_at),
                  jsonb_strip_nulls(jsonb_build_object(
                      'orderId', o.order_id, 'paymentId', o.payment_id, 'method', o.payment_method,
                      'purpose', o.purpose, 'fulfilled', o.fulfilled, 'recovered', o.recovered,
                      'errorCode', o.error_code, 'errorDescription', o.error_description,
                      'userName', o.user_name, 'receipt', o.receipt))
           FROM campus_ops.razorpay_orders o
           WHERE o.tenant_id=$1"#,
        );
    }
    if sources.fee_links {
        sql.push_str(
            r#"
           UNION ALL
           SELECT 'fee_payment', g.id::text, 'fee_payment', 'payment',
                  g.student_user_id, NULL, (g.amount_paise / 100.0)::float8, NULL, NULL,
                  'Fee payment link', COALESCE(g.payment_id, g.provider_link_id), g.status,
                  NULL::float8, COALESCE(g.paid_at, g.created_at),
                  jsonb_strip_nulls(jsonb_build_object(
                      'feeRecordId', g.fee_record_id, 'guardianName', g.guardian_name,
                      'paymentId', g.payment_id))
           FROM campus_ops.guardian_fee_payment_links g
           WHERE g.tenant_id=$1"#,
        );
    }
    sql
}

fn audit_sql(sources: Sources) -> String {
    format!(
        r#"WITH timeline AS ({timeline}),
           enriched AS (
             SELECT e.*,
                    COALESCE(NULLIF(student.full_name, ''), NULLIF(target.display_name, ''), target.email) AS target_name,
                    NULLIF(student.student_number, '') AS target_number,
                    target.email AS target_email,
                    COALESCE(NULLIF(actor.display_name, ''), actor.email) AS actor_name
             FROM timeline e
             LEFT JOIN core.students student
               ON student.tenant_id=$1 AND student.user_account_id::text=e.user_id
             LEFT JOIN identity.users target ON target.id::text=e.user_id
             LEFT JOIN identity.users actor ON actor.id::text=e.actor_user_id
           ),
           filtered AS (
             SELECT * FROM enriched
             WHERE ($2::timestamptz IS NULL OR created_at >= $2)
               AND ($3::timestamptz IS NULL OR created_at < $3)
               AND ($4::text[] IS NULL OR kind = ANY($4))
               AND ($5::text IS NULL OR direction = $5)
               AND ($6::text IS NULL
                    OR COALESCE(target_name, '') ILIKE $6
                    OR COALESCE(target_number, '') ILIKE $6
                    OR COALESCE(target_email, '') ILIKE $6
                    OR COALESCE(description, '') ILIKE $6
                    OR COALESCE(reference, '') ILIKE $6
                    OR id ILIKE $6
                    OR user_id = ANY($7))
               AND ($8::text IS NULL
                    OR COALESCE(actor_name, '') ILIKE $8
                    OR actor_user_id = ANY($9))
               AND (NOT $10 OR actor_user_id IS NULL)
           )
           SELECT
             (SELECT COALESCE(jsonb_agg(jsonb_build_object(
                 'source', p.source, 'id', p.id, 'kind', p.kind, 'direction', p.direction,
                 'userId', p.user_id, 'targetName', p.target_name, 'targetNumber', p.target_number,
                 'targetEmail', p.target_email, 'actorUserId', p.actor_user_id,
                 'actorName', p.actor_name, 'amount', p.amount, 'shopKey', p.shop_key,
                 'shopName', p.shop_name, 'description', p.description, 'reference', p.reference,
                 'status', p.status, 'balanceAfter', p.balance_after,
                 'balanceBefore', p.balance_after - p.amount,
                 'createdAt', p.created_at, 'details', p.details
               ) ORDER BY p.created_at DESC, p.id), '[]'::jsonb)
              FROM (SELECT * FROM filtered ORDER BY created_at DESC, id LIMIT $11 OFFSET $12) p) AS entries,
             (SELECT count(*) FROM filtered) AS total,
             (SELECT COALESCE(sum(amount) FILTER (WHERE direction='credit'), 0)::float8 FROM filtered) AS credits,
             (SELECT COALESCE(-sum(amount) FILTER (WHERE direction='debit'), 0)::float8 FROM filtered) AS debits,
             (SELECT COALESCE(sum(amount) FILTER (
                 WHERE direction='payment' AND kind <> 'payment_request_issued'
                   AND status IN ('paid', 'captured')), 0)::float8 FROM filtered) AS payments,
             (SELECT COALESCE(jsonb_object_agg(kind, n), '{{}}'::jsonb)
              FROM (SELECT kind, count(*) AS n FROM filtered GROUP BY kind) k) AS by_kind"#,
        timeline = timeline_sql(sources)
    )
}

async fn available_sources(pool: &sqlx::PgPool) -> ApiResult<Sources> {
    let row = sqlx::query(
        r#"SELECT to_regclass('campus_ops.laundry_charges') IS NOT NULL AS laundry,
                  to_regclass('campus_ops.payment_request_payers') IS NOT NULL
                    AND to_regclass('campus_ops.payment_requests') IS NOT NULL AS payment_requests,
                  to_regclass('campus_ops.razorpay_orders') IS NOT NULL AS razorpay_orders,
                  to_regclass('campus_ops.guardian_fee_payment_links') IS NOT NULL AS fee_links"#,
    )
    .fetch_one(pool)
    .await?;
    Ok(Sources {
        laundry: row.try_get("laundry")?,
        payment_requests: row.try_get("payment_requests")?,
        razorpay_orders: row.try_get("razorpay_orders")?,
        fee_links: row.try_get("fee_links")?,
    })
}

/// Control-plane accounts in this tenant whose name or email matches.
async fn matching_account_ids(
    control: &sqlx::PgPool,
    tenant_slug: &str,
    pattern: &str,
) -> ApiResult<Vec<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        r#"SELECT u.id::text
           FROM identity.users u
           JOIN identity.tenant_memberships m ON m.user_id=u.id
           JOIN platform.tenants t ON t.id=m.tenant_id
           WHERE t.slug=$1 AND (u.display_name ILIKE $2 OR u.email ILIKE $2)
           LIMIT 500"#,
    )
    .bind(tenant_slug)
    .bind(pattern)
    .fetch_all(control)
    .await?)
}

async fn list_audit_logs(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<AuditQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, READ_PERMISSION)?;
    let tenant_slug = principal.student.tenant_id.clone();
    let db = state.tenant_database(&tenant_slug).await?;
    let control = state.database().unwrap_or_else(|| db.clone());
    let tenant = crate::operations::tenant_id(db.pool(), &tenant_slug).await?;
    let (from, to) = day_bounds(query.from, query.to)?;
    let kinds = parse_kinds(query.kind.as_deref());
    let direction = query
        .direction
        .as_deref()
        .map(str::trim)
        .filter(|value| matches!(*value, "credit" | "debit" | "payment"))
        .map(str::to_owned);
    let search = like_pattern(query.q.as_deref());
    let search_ids = match search.as_deref() {
        Some(pattern) => matching_account_ids(control.pool(), &tenant_slug, pattern).await?,
        None => Vec::new(),
    };
    let actor_text = query
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let system_only = actor_text.is_some_and(|value| value.eq_ignore_ascii_case("system"));
    let actor = if system_only {
        None
    } else {
        like_pattern(actor_text)
    };
    let actor_ids = match actor.as_deref() {
        Some(pattern) => matching_account_ids(control.pool(), &tenant_slug, pattern).await?,
        None => Vec::new(),
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0).clamp(0, 1_000_000);

    let sources = available_sources(db.pool()).await?;
    let run = |sources: Sources| {
        let sql = audit_sql(sources);
        let kinds = kinds.clone();
        let direction = direction.clone();
        let search = search.clone();
        let search_ids = search_ids.clone();
        let actor = actor.clone();
        let actor_ids = actor_ids.clone();
        let pool = db.pool().clone();
        async move {
            sqlx::query(&sql)
                .bind(tenant)
                .bind(from)
                .bind(to)
                .bind(kinds)
                .bind(direction)
                .bind(search)
                .bind(search_ids)
                .bind(actor)
                .bind(actor_ids)
                .bind(system_only)
                .bind(limit)
                .bind(offset)
                .fetch_one(&pool)
                .await
        }
    };
    // The optional feature tables are owned by other modules. If one has a
    // shape this query does not expect, fall back to the wallet ledger alone
    // rather than hiding the whole trail.
    let row = match run(sources).await {
        Ok(row) => row,
        Err(error) if sources.payment_requests || sources.razorpay_orders || sources.fee_links => {
            tracing::warn!(%error, "finance audit fell back to the wallet ledger");
            run(Sources {
                laundry: sources.laundry,
                ..Sources::default()
            })
            .await?
        }
        Err(error) => return Err(error.into()),
    };

    let mut entries: Value = row.try_get("entries")?;
    fill_control_names(&mut entries, control.pool(), &tenant_slug).await?;
    Ok(Json(ApiResponse::new(json!({
        "entries": entries,
        "total": row.try_get::<i64, _>("total")?,
        "summary": {
            "credits": row.try_get::<f64, _>("credits")?,
            "debits": row.try_get::<f64, _>("debits")?,
            "payments": row.try_get::<f64, _>("payments")?,
            "byKind": row.try_get::<Value, _>("by_kind")?,
        },
        "kinds": KINDS,
        "limit": limit,
        "offset": offset,
    }))))
}

/// The tenant copy of identity.users can lag behind the control plane, so
/// names and emails missing from it are filled from the control plane.
async fn fill_control_names(
    entries: &mut Value,
    control: &sqlx::PgPool,
    tenant_slug: &str,
) -> ApiResult<()> {
    let Some(items) = entries.as_array_mut() else {
        return Ok(());
    };
    let ids: Vec<Uuid> = items
        .iter()
        .flat_map(|item| {
            ["userId", "actorUserId"]
                .into_iter()
                .filter_map(|key| item.get(key).and_then(Value::as_str))
                .filter_map(|id| Uuid::parse_str(id).ok())
                .collect::<Vec<_>>()
        })
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let accounts: HashMap<String, (String, String)> =
        sqlx::query_as::<_, (String, String, String)>(
            r#"SELECT u.id::text, u.display_name, u.email
               FROM identity.users u
               JOIN identity.tenant_memberships m ON m.user_id=u.id
               JOIN platform.tenants t ON t.id=m.tenant_id AND t.slug=$2
               WHERE u.id = ANY($1)"#,
        )
        .bind(&ids)
        .bind(tenant_slug)
        .fetch_all(control)
        .await?
        .into_iter()
        .map(|(id, name, email)| (id.to_lowercase(), (name, email)))
        .collect();
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        for (id_key, name_key, email_key) in [
            ("userId", "targetName", Some("targetEmail")),
            ("actorUserId", "actorName", None),
        ] {
            let Some(account) = object
                .get(id_key)
                .and_then(Value::as_str)
                .and_then(|id| accounts.get(&id.to_lowercase()))
                .cloned()
            else {
                continue;
            };
            let missing = |value: Option<&Value>| {
                value
                    .and_then(Value::as_str)
                    .is_none_or(|value| value.trim().is_empty())
            };
            if missing(object.get(name_key)) {
                let name = if account.0.trim().is_empty() {
                    account.1.clone()
                } else {
                    account.0.clone()
                };
                object.insert(name_key.into(), json!(name));
            }
            if let Some(email_key) = email_key
                && missing(object.get(email_key))
            {
                object.insert(email_key.into(), json!(account.1));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{KINDS, Sources, audit_sql, parse_kinds, timeline_sql};

    #[test]
    fn kind_filter_keeps_known_kinds_only() {
        assert_eq!(parse_kinds(None), None);
        assert_eq!(parse_kinds(Some(" , bogus")), None);
        assert_eq!(
            parse_kinds(Some("refund, laundry_payment,drop table")),
            Some(vec!["refund".to_owned(), "laundry_payment".to_owned()])
        );
        assert!(KINDS.contains(&"online_payment"));
    }

    #[test]
    fn optional_sources_are_added_only_when_present() {
        let wallet_only = timeline_sql(Sources::default());
        assert!(wallet_only.contains("canteen_wallet_transactions"));
        assert!(!wallet_only.contains("laundry_charges"));
        assert!(!wallet_only.contains("UNION ALL"));

        let everything = timeline_sql(Sources {
            laundry: true,
            payment_requests: true,
            razorpay_orders: true,
            fee_links: true,
        });
        assert!(everything.contains("campus_ops.laundry_charges"));
        assert!(everything.contains("campus_ops.payment_request_payers"));
        assert!(everything.contains("campus_ops.razorpay_orders"));
        assert!(everything.contains("campus_ops.guardian_fee_payment_links"));
        assert_eq!(everything.matches("UNION ALL").count(), 4);
    }

    #[test]
    fn audit_query_binds_every_filter() {
        let sql = audit_sql(Sources::default());
        for placeholder in [
            "$1", "$2", "$3", "$4", "$5", "$6", "$7", "$8", "$9", "$10", "$11", "$12",
        ] {
            assert!(sql.contains(placeholder), "{placeholder} is not used");
        }
        assert!(sql.contains("'{}'::jsonb"));
    }
}

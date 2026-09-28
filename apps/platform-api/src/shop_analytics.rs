//! One shop's sales, cost, profit and counter-staff performance over an
//! explicit date range — the owner workspace's "Sales & Profit" tab.
//!
//! `GET /canteen/shop-analytics?shop=<key>&from=YYYY-MM-DD&to=YYYY-MM-DD`
//!
//! * `from`/`to` are inclusive calendar days in the tenant's local time
//!   (Asia/Kolkata). Both default to today; one alone is a single day.
//! * Revenue is completed orders only, exactly as the campus sales dashboard
//!   counts it: rejected orders are refunded and cancelled ones never charged.
//! * Cost is each line's quantity times the menu item's configured cost price
//!   (`actual_price`, which itself defaults to the selling price). A line whose
//!   item has since been deleted is costed at its selling price, so it never
//!   reports profit that cannot be traced to a configured cost.
//! * Staff performance is attributed by `canteen_orders.handled_by` — the
//!   account that last moved the order. Every active captain assigned to the
//!   shop is listed, including those with no orders in the range; anyone else
//!   who handled an order (an owner, or a captain since unassigned) follows.

use std::collections::HashMap;

use axum::{
    Extension, Json,
    extract::{Query, State},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::{require_any, require_assigned_shop, tenant_id},
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

/// Grants that may read a shop's analytics; the shop assignment is checked too.
const ANALYTICS_GRANTS: &[&str] = &[
    "vendor_management.vendors.read",
    "canteen.analytics.read",
    "canteen.orders.manage",
];

/// Grants that oversee every shop on campus, so need no assignment.
const OVERSIGHT_GRANTS: &[&str] = &[
    "vendor_management.vendors.read",
    "vendor_management.vendors.update",
];

const TENANT_TIMEZONE: &str = "Asia/Kolkata";

/// The longest range one request may cover (two years, leap day included).
const MAX_RANGE_DAYS: i64 = 731;

/// Most recent orders listed under each staff member.
const RECENT_PER_STAFF: usize = 10;

#[derive(Deserialize)]
pub(crate) struct ShopAnalyticsQuery {
    shop: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

pub(crate) async fn shop_analytics(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<ShopAnalyticsQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(&access, ANALYTICS_GRANTS)?;
    let shop_key = query
        .shop
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| ApiError::BadRequest("Choose a shop".into()))?
        .to_ascii_lowercase();
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let pool = db.pool();
    let tenant = tenant_id(pool, &principal.student.tenant_id).await?;
    if !OVERSIGHT_GRANTS.iter().any(|grant| access.allows(grant)) {
        require_assigned_shop(
            pool,
            tenant,
            &principal.student.id,
            &principal.student.email,
            &shop_key,
            &access,
        )
        .await?;
    }

    let today = sqlx::query_scalar::<_, NaiveDate>("SELECT (now() AT TIME ZONE $1)::date")
        .bind(TENANT_TIMEZONE)
        .fetch_one(pool)
        .await?;
    let range = DateRange::parse(query.from.as_deref(), query.to.as_deref(), today)?;

    let shop = sqlx::query_as::<_, (String, String, String)>(
        "SELECT shop_key, name, category FROM campus_ops.shops WHERE tenant_id=$1 AND shop_key=$2",
    )
    .bind(tenant)
    .bind(&shop_key)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Shop not found".into()))?;

    let orders = shop_orders(pool, tenant, &shop_key, range).await?;
    let staff = shop_staff(pool, tenant, &shop_key).await?;
    let unknown: Vec<String> = orders
        .iter()
        .filter_map(|order| order.handled_by.clone())
        .filter(|id| !staff.iter().any(|member| member.matches(id)))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let others = if unknown.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, (String, String, Option<String>)>(
            "SELECT id::text, display_name, email FROM identity.users WHERE id::text = ANY($1)",
        )
        .bind(&unknown)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(id, name, email)| (id, (name, email)))
        .collect()
    };

    let mut body = build_report(&orders, &staff, &others);
    body["shop"] = json!({ "shopKey": shop.0, "name": shop.1, "category": shop.2 });
    body["range"] = json!({
        "from": range.from.to_string(),
        "to": range.to.to_string(),
        "days": range.days(),
        "timezone": TENANT_TIMEZONE,
    });
    Ok(Json(ApiResponse::new(body)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DateRange {
    from: NaiveDate,
    to: NaiveDate,
}

impl DateRange {
    fn parse(from: Option<&str>, to: Option<&str>, today: NaiveDate) -> ApiResult<Self> {
        let day = |raw: Option<&str>, field: &str| -> ApiResult<Option<NaiveDate>> {
            match raw.map(str::trim).filter(|value| !value.is_empty()) {
                None => Ok(None),
                Some(value) => NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .map(Some)
                    .map_err(|_| {
                        ApiError::BadRequest(format!("`{field}` must be a date like 2026-01-31"))
                    }),
            }
        };
        let (from, to) = match (day(from, "from")?, day(to, "to")?) {
            (None, None) => (today, today),
            (Some(from), None) => (from, from),
            (None, Some(to)) => (to, to),
            (Some(from), Some(to)) => (from, to),
        };
        if from > to {
            return Err(ApiError::BadRequest(
                "The start date must be on or before the end date".into(),
            ));
        }
        let range = Self { from, to };
        if range.days() > MAX_RANGE_DAYS {
            return Err(ApiError::BadRequest(format!(
                "Choose a range of at most {MAX_RANGE_DAYS} days"
            )));
        }
        Ok(range)
    }

    fn days(self) -> i64 {
        (self.to - self.from).num_days() + 1
    }
}

/// One order of the shop inside the range, costed.
#[derive(Debug, Clone, PartialEq)]
struct OrderFact {
    id: Uuid,
    order_number: i64,
    customer_name: String,
    status: String,
    handled_by: Option<String>,
    total: f64,
    cost: f64,
    items: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl OrderFact {
    fn is_completed(&self) -> bool {
        self.status == "completed"
    }
    fn is_active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "pending" | "accepted" | "preparing" | "ready"
        )
    }
    fn is_rejected(&self) -> bool {
        self.status == "rejected"
    }
    fn is_cancelled(&self) -> bool {
        self.status == "cancelled"
    }
    /// Placement to hand-over, for completed orders.
    fn handling_minutes(&self) -> Option<f64> {
        self.is_completed().then(|| {
            ((self.updated_at - self.created_at).num_seconds().max(0) as f64) / 60.0
        })
    }
}

/// An active operator assignment of the shop.
#[derive(Debug, Clone, PartialEq)]
struct StaffMember {
    /// `shop_user_assignments.user_id` as stored.
    assignment_user_id: String,
    /// The matching `identity.users` id, when the account exists.
    identity_id: Option<String>,
    role: String,
    name: String,
    email: Option<String>,
}

impl StaffMember {
    fn matches(&self, handler: &str) -> bool {
        self.assignment_user_id == handler || self.identity_id.as_deref() == Some(handler)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
struct Totals {
    orders: i64,
    completed: i64,
    active: i64,
    rejected: i64,
    cancelled: i64,
    items: i64,
    revenue: f64,
    cost: f64,
    active_value: f64,
    refunded: f64,
    handling_minutes: f64,
    handled_completed: i64,
}

impl Totals {
    fn add(&mut self, order: &OrderFact) {
        self.orders += 1;
        if order.is_completed() {
            self.completed += 1;
            self.items += order.items;
            self.revenue += order.total;
            self.cost += order.cost;
        } else if order.is_active() {
            self.active += 1;
            self.active_value += order.total;
        } else if order.is_rejected() {
            self.rejected += 1;
            self.refunded += order.total;
        } else if order.is_cancelled() {
            self.cancelled += 1;
        }
        if let Some(minutes) = order.handling_minutes() {
            self.handling_minutes += minutes;
            self.handled_completed += 1;
        }
    }

    fn profit(&self) -> f64 {
        self.revenue - self.cost
    }

    fn json(&self, total_revenue: f64) -> Value {
        let revenue = round_money(self.revenue);
        let cost = round_money(self.cost);
        json!({
            "orders": self.orders,
            "completedOrders": self.completed,
            "activeOrders": self.active,
            "rejectedOrders": self.rejected,
            "cancelledOrders": self.cancelled,
            "itemsSold": self.items,
            "revenue": revenue,
            "cost": cost,
            "profit": round_money(self.profit()),
            "marginPercent": if self.revenue > 0.0 {
                round_one(self.profit() / self.revenue * 100.0)
            } else { 0.0 },
            "averageOrderValue": if self.completed > 0 {
                round_money(self.revenue / self.completed as f64)
            } else { 0.0 },
            "activeValue": round_money(self.active_value),
            "refunded": round_money(self.refunded),
            "revenueShare": if total_revenue > 0.0 {
                round_one(self.revenue / total_revenue * 100.0)
            } else { 0.0 },
            "averageHandlingMinutes": if self.handled_completed > 0 {
                Value::from(round_one(self.handling_minutes / self.handled_completed as f64))
            } else { Value::Null },
        })
    }
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round_one(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn order_json(order: &OrderFact) -> Value {
    json!({
        "id": order.id,
        "orderNumber": order.order_number,
        "customerName": order.customer_name,
        "status": order.status,
        "total": round_money(order.total),
        "cost": round_money(order.cost),
        "profit": round_money(order.total - order.cost),
        "itemCount": order.items,
        "createdAt": order.created_at,
        "updatedAt": order.updated_at,
    })
}

/// Rolls the range's orders up for the shop and per staff member. `orders`
/// are newest first; `others` names handlers who are not assigned staff.
fn build_report(
    orders: &[OrderFact],
    staff: &[StaffMember],
    others: &HashMap<String, (String, Option<String>)>,
) -> Value {
    let mut summary = Totals::default();
    for order in orders {
        summary.add(order);
    }
    let unattributed = orders.iter().filter(|o| o.handled_by.is_none()).count();

    struct Row<'a> {
        user_id: String,
        name: String,
        email: Option<String>,
        role: Option<String>,
        assigned: bool,
        totals: Totals,
        recent: Vec<&'a OrderFact>,
        last_handled: Option<DateTime<Utc>>,
    }
    let mut rows: Vec<Row> = staff
        .iter()
        .map(|member| Row {
            user_id: member
                .identity_id
                .clone()
                .unwrap_or_else(|| member.assignment_user_id.clone()),
            name: member.name.clone(),
            email: member.email.clone(),
            role: Some(member.role.clone()),
            assigned: true,
            totals: Totals::default(),
            recent: Vec::new(),
            last_handled: None,
        })
        .collect();

    for order in orders {
        let Some(handler) = order.handled_by.as_deref() else {
            continue;
        };
        let index = match staff.iter().position(|member| member.matches(handler)) {
            Some(index) => index,
            None => match rows.iter().position(|row| !row.assigned && row.user_id == handler) {
                Some(index) => index,
                None => {
                    let (name, email) = others
                        .get(handler)
                        .cloned()
                        .unwrap_or_else(|| ("Former staff".into(), None));
                    rows.push(Row {
                        user_id: handler.to_owned(),
                        name,
                        email,
                        role: None,
                        assigned: false,
                        totals: Totals::default(),
                        recent: Vec::new(),
                        last_handled: None,
                    });
                    rows.len() - 1
                }
            },
        };
        let row = &mut rows[index];
        row.totals.add(order);
        if row.recent.len() < RECENT_PER_STAFF {
            row.recent.push(order);
        }
        row.last_handled = row.last_handled.max(Some(order.updated_at));
    }

    // Captains first — every one of them, even idle — then owners and anyone
    // else who handled orders; busiest first within each group.
    let rank = |row: &Row| match row.role.as_deref() {
        Some("captain") => 0,
        Some(_) => 1,
        None => 2,
    };
    rows.retain(|row| row.role.as_deref() == Some("captain") || row.totals.orders > 0);
    rows.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then(b.totals.revenue.total_cmp(&a.totals.revenue))
            .then(b.totals.orders.cmp(&a.totals.orders))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let captains: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut value = row.totals.json(summary.revenue);
            value["userId"] = json!(row.user_id);
            value["name"] = json!(row.name);
            value["email"] = json!(row.email);
            value["role"] = json!(row.role);
            value["assigned"] = json!(row.assigned);
            value["lastHandledAt"] = json!(row.last_handled);
            value["recentOrders"] = Value::Array(row.recent.iter().map(|o| order_json(o)).collect());
            value
        })
        .collect();

    let mut summary_json = summary.json(summary.revenue);
    summary_json["unattributedOrders"] = json!(unattributed);
    json!({ "summary": summary_json, "captains": captains })
}

async fn shop_orders(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    shop_key: &str,
    range: DateRange,
) -> ApiResult<Vec<OrderFact>> {
    // Orders whose `store` predates the shop register resolve to the shop of
    // the matching category, exactly as ordering and the sales dashboard do.
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            i64,
            String,
            String,
            Option<String>,
            f64,
            f64,
            i64,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        r#"WITH scoped AS (
          SELECT o.id, o.order_number, o.customer_name, o.status, o.handled_by,
                 o.total::float8 AS total,
                 CASE WHEN jsonb_typeof(o.lines)='array' THEN o.lines ELSE '[]'::jsonb END AS lines,
                 o.created_at, o.updated_at
          FROM campus_ops.canteen_orders o
          LEFT JOIN campus_ops.shops exact ON exact.tenant_id=o.tenant_id AND exact.shop_key=o.store
          LEFT JOIN LATERAL (
            SELECT s.shop_key FROM campus_ops.shops s
            WHERE exact.shop_key IS NULL AND s.tenant_id=o.tenant_id AND lower(s.category)=CASE
              WHEN lower(o.store) LIKE '%laundry%' THEN 'laundry'
              WHEN lower(o.store) LIKE '%station%' THEN 'stationery'
              ELSE 'canteen' END
            ORDER BY s.created_at, s.shop_key LIMIT 1
          ) fallback ON true
          WHERE o.tenant_id=$1
            AND COALESCE(exact.shop_key, fallback.shop_key, o.store)=$2
            AND o.created_at >= ($4::date::timestamp AT TIME ZONE $3)
            AND o.created_at < (($5::date + 1)::timestamp AT TIME ZONE $3)
        )
        SELECT x.id, x.order_number, x.customer_name, x.status, x.handled_by, x.total,
          COALESCE((SELECT sum(
              COALESCE(NULLIF(l.value->>'quantity','')::float8, 1)
              * COALESCE(mi.actual_price::float8, mi.price::float8,
                         NULLIF(l.value->>'price','')::float8, 0))
            FROM jsonb_array_elements(x.lines) l
            LEFT JOIN campus_ops.canteen_menu_items mi
              ON mi.tenant_id=$1 AND mi.id::text = l.value->>'itemId'), 0)::float8 AS cost,
          COALESCE((SELECT sum(COALESCE(NULLIF(l.value->>'quantity','')::float8, 1))
            FROM jsonb_array_elements(x.lines) l), 0)::int8 AS items,
          x.created_at, x.updated_at
        FROM scoped x
        ORDER BY x.created_at DESC"#,
    )
    .bind(tenant)
    .bind(shop_key)
    .bind(TENANT_TIMEZONE)
    .bind(range.from)
    .bind(range.to)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| OrderFact {
            id: r.0,
            order_number: r.1,
            customer_name: r.2,
            status: r.3,
            handled_by: r.4.filter(|id| !id.trim().is_empty()),
            total: r.5,
            cost: r.6,
            items: r.7,
            created_at: r.8,
            updated_at: r.9,
        })
        .collect())
}

async fn shop_staff(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    shop_key: &str,
) -> ApiResult<Vec<StaffMember>> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, String, Option<String>)>(
        r#"SELECT a.user_id, a.assignment_role, u.id::text,
             COALESCE(NULLIF(trim(u.display_name), ''), u.email, a.user_id), u.email
           FROM campus_ops.shop_user_assignments a
           JOIN campus_ops.shops s ON s.tenant_id=a.tenant_id AND s.id=a.shop_id
           LEFT JOIN identity.users u ON u.id::text=a.user_id
           WHERE a.tenant_id=$1 AND s.shop_key=$2 AND a.is_active
           ORDER BY a.assignment_role, 4"#,
    )
    .bind(tenant)
    .bind(shop_key)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| StaffMember {
            assignment_user_id: r.0,
            role: r.1,
            identity_id: r.2,
            name: r.3,
            email: r.4,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn day(raw: &str) -> NaiveDate {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").unwrap()
    }

    fn order(handler: Option<&str>, status: &str, total: f64, cost: f64, minutes: i64) -> OrderFact {
        let created = Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap();
        OrderFact {
            id: Uuid::new_v4(),
            order_number: 1,
            customer_name: "Student".into(),
            status: status.into(),
            handled_by: handler.map(str::to_owned),
            total,
            cost,
            items: 2,
            created_at: created,
            updated_at: created + chrono::Duration::minutes(minutes),
        }
    }

    fn member(id: &str, role: &str, name: &str) -> StaffMember {
        StaffMember {
            assignment_user_id: id.into(),
            identity_id: Some(id.into()),
            role: role.into(),
            name: name.into(),
            email: None,
        }
    }

    #[test]
    fn ranges_default_to_today_and_reject_bad_input() {
        let today = day("2026-09-28");
        assert_eq!(
            DateRange::parse(None, None, today).unwrap(),
            DateRange { from: today, to: today }
        );
        let range = DateRange::parse(Some("2026-09-01"), Some("2026-09-30"), today).unwrap();
        assert_eq!(range.days(), 30);
        assert_eq!(
            DateRange::parse(Some("2026-09-05"), None, today).unwrap().days(),
            1
        );
        assert!(DateRange::parse(Some("2026-09-30"), Some("2026-09-01"), today).is_err());
        assert!(DateRange::parse(Some("30/09/2026"), None, today).is_err());
        assert!(DateRange::parse(Some("2020-01-01"), Some("2026-01-01"), today).is_err());
    }

    #[test]
    fn every_captain_is_listed_and_credited_with_their_own_orders() {
        let staff = vec![
            member("cap-a", "captain", "Anu"),
            member("cap-b", "captain", "Bala"),
            member("own", "owner", "Owner"),
        ];
        let orders = vec![
            order(Some("cap-a"), "completed", 100.0, 60.0, 10),
            order(Some("cap-a"), "completed", 50.0, 30.0, 20),
            order(Some("cap-a"), "rejected", 40.0, 20.0, 1),
            order(Some("gone"), "completed", 30.0, 10.0, 5),
            order(None, "pending", 25.0, 10.0, 0),
        ];
        let mut others = HashMap::new();
        others.insert("gone".to_string(), ("Chitra".to_string(), None));
        let report = build_report(&orders, &staff, &others);

        let summary = &report["summary"];
        assert_eq!(summary["orders"], 5);
        assert_eq!(summary["completedOrders"], 3);
        assert_eq!(summary["revenue"], 180.0);
        assert_eq!(summary["cost"], 100.0);
        assert_eq!(summary["profit"], 80.0);
        assert_eq!(summary["refunded"], 40.0);
        assert_eq!(summary["activeValue"], 25.0);
        assert_eq!(summary["unattributedOrders"], 1);

        let captains = report["captains"].as_array().unwrap();
        let names: Vec<&str> = captains.iter().map(|c| c["name"].as_str().unwrap()).collect();
        // Idle Bala is still listed; the idle owner is not; the unassigned
        // handler follows the captains.
        assert_eq!(names, ["Anu", "Bala", "Chitra"]);
        let anu = &captains[0];
        assert_eq!(anu["orders"], 3);
        assert_eq!(anu["completedOrders"], 2);
        assert_eq!(anu["rejectedOrders"], 1);
        assert_eq!(anu["itemsSold"], 4);
        assert_eq!(anu["revenue"], 150.0);
        assert_eq!(anu["profit"], 60.0);
        assert_eq!(anu["averageHandlingMinutes"], 15.0);
        assert_eq!(anu["revenueShare"], 83.3);
        assert_eq!(anu["recentOrders"].as_array().unwrap().len(), 3);
        let bala = &captains[1];
        assert_eq!(bala["orders"], 0);
        assert_eq!(bala["revenue"], 0.0);
        assert!(bala["averageHandlingMinutes"].is_null());
        assert_eq!(captains[2]["assigned"], false);
    }

    #[test]
    fn handler_ids_match_either_the_assignment_or_the_identity_id() {
        let staff = vec![StaffMember {
            assignment_user_id: "legacy-id".into(),
            identity_id: Some("uuid-1".into()),
            role: "captain".into(),
            name: "Dev".into(),
            email: None,
        }];
        let orders = vec![
            order(Some("uuid-1"), "completed", 10.0, 5.0, 3),
            order(Some("legacy-id"), "completed", 20.0, 5.0, 3),
        ];
        let report = build_report(&orders, &staff, &HashMap::new());
        let captains = report["captains"].as_array().unwrap();
        assert_eq!(captains.len(), 1);
        assert_eq!(captains[0]["orders"], 2);
        assert_eq!(captains[0]["userId"], "uuid-1");
    }
}

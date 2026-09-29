//! One shop's sales, cost, profit and counter-staff performance over an
//! explicit date range — the owner workspace's "Sales" destination.
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
//!   account that last moved the order. Every active account the admin has
//!   assigned to the shop as a captain (Vendors & shops → Counter staff) is
//!   listed, including those with no orders in the range; anyone else who
//!   handled an order (an owner, or a captain since unassigned) follows.
//!   Deactivated and deleted accounts are never listed; the orders they moved
//!   still count in the shop's totals.
//!
//! Adding `captain=<userId>` (with optional `page` and `pageSize`) returns
//! that person's detail for the same range under `captainDetail`: their
//! figures, a per-day series and every order they handled, paginated.

use std::collections::{BTreeSet, HashMap, HashSet};

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

/// Most recent orders listed under each staff member in the overview.
const RECENT_PER_STAFF: usize = 10;

/// Orders per page of a captain's detail, by default and at most.
const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_PAGE_SIZE: usize = 100;

/// An account that is switched off or was deleted (see `user_deletion`).
/// Deletion tombstones the row: inactive, with a `deleted+…@deleted.invalid`
/// placeholder address and `profile.deleted = true`.
const LIVE_ACCOUNT_SQL: &str = "u.active \
     AND lower(COALESCE(u.email, '')) NOT LIKE 'deleted+%@deleted.invalid' \
     AND COALESCE(u.profile->>'deleted', 'false') <> 'true'";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShopAnalyticsQuery {
    shop: Option<String>,
    from: Option<String>,
    to: Option<String>,
    /// Adds this staff member's detail (`captainDetail`) to the report.
    captain: Option<String>,
    page: Option<usize>,
    #[serde(alias = "page_size")]
    page_size: Option<usize>,
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
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut hidden = HashSet::new();
    let mut others = HashMap::new();
    if !unknown.is_empty() {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, bool, Option<DateTime<Utc>>)>(
            &format!(
                "SELECT u.id::text, COALESCE(NULLIF(trim(u.display_name), ''), u.email, u.id::text), \
                        u.email, ({LIVE_ACCOUNT_SQL}) AS live, u.last_login_at \
                 FROM identity.users u WHERE u.id::text = ANY($1)"
            ),
        )
        .bind(&unknown)
        .fetch_all(pool)
        .await?;
        for (id, name, email, live, last_login) in rows {
            if live {
                others.insert(
                    id,
                    OtherHandler {
                        name,
                        email,
                        last_login,
                    },
                );
            } else {
                // Deactivated or deleted: not listed, still in the totals.
                hidden.insert(id);
            }
        }
    }

    let rows = staff_rows(&orders, &staff, &others, &hidden);
    let mut body = build_report(&orders, &rows);
    if let Some(captain) = query
        .captain
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        let row = rows
            .iter()
            .find(|row| row.user_id == captain || row.aliases.iter().any(|alias| alias == captain))
            .ok_or_else(|| {
                ApiError::NotFound("That staff member has no record at this shop".into())
            })?;
        body["captainDetail"] = captain_detail(
            row,
            range,
            total_revenue(&orders),
            query.page.unwrap_or(1),
            query.page_size.unwrap_or(DEFAULT_PAGE_SIZE),
            &shop.0,
        );
    }
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
    /// The calendar day it was placed, in the tenant's time zone.
    day: NaiveDate,
    fulfilment_mode: String,
    token_number: Option<i32>,
    lines: Value,
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
        self.is_completed()
            .then(|| ((self.updated_at - self.created_at).num_seconds().max(0) as f64) / 60.0)
    }
}

/// A live account actively assigned to the shop.
#[derive(Debug, Clone, PartialEq)]
struct StaffMember {
    /// `shop_user_assignments.user_id` as stored (an id, or a legacy email).
    assignment_user_id: String,
    /// The account's `identity.users` id.
    identity_id: String,
    role: String,
    name: String,
    email: Option<String>,
    last_login: Option<DateTime<Utc>>,
}

impl StaffMember {
    fn matches(&self, handler: &str) -> bool {
        self.assignment_user_id == handler || self.identity_id == handler
    }
}

/// Someone who handled an order without being assigned to the shop.
#[derive(Debug, Clone, PartialEq)]
struct OtherHandler {
    name: String,
    email: Option<String>,
    last_login: Option<DateTime<Utc>>,
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

fn total_revenue(orders: &[OrderFact]) -> f64 {
    orders
        .iter()
        .filter(|order| order.is_completed())
        .map(|order| order.total)
        .sum()
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

/// Everything the order detail page shows, in the shape the canteen order
/// endpoints use, plus the analytics figures.
fn detailed_order_json(order: &OrderFact, handler_name: &str, shop_key: &str) -> Value {
    let mut value = order_json(order);
    value["lines"] = order.lines.clone();
    value["fulfilmentMode"] = json!(order.fulfilment_mode);
    value["tokenNumber"] = json!(order.token_number);
    value["captainName"] = json!(handler_name);
    value["shopKey"] = json!(shop_key);
    value
}

/// One person's share of the range.
struct StaffRow<'a> {
    user_id: String,
    /// Other ids the same person's orders may carry (a legacy assignment id).
    aliases: Vec<String>,
    name: String,
    email: Option<String>,
    role: Option<String>,
    assigned: bool,
    last_login: Option<DateTime<Utc>>,
    totals: Totals,
    /// Newest first.
    orders: Vec<&'a OrderFact>,
    last_handled: Option<DateTime<Utc>>,
}

/// Credits the range's orders (newest first) to the shop's staff. Every
/// assigned captain is kept, idle or not; anyone else only when they handled
/// an order. Hidden (deactivated or deleted) handlers are left out.
fn staff_rows<'a>(
    orders: &'a [OrderFact],
    staff: &[StaffMember],
    others: &HashMap<String, OtherHandler>,
    hidden: &HashSet<String>,
) -> Vec<StaffRow<'a>> {
    let mut rows: Vec<StaffRow> = staff
        .iter()
        .map(|member| StaffRow {
            user_id: member.identity_id.clone(),
            aliases: if member.assignment_user_id == member.identity_id {
                Vec::new()
            } else {
                vec![member.assignment_user_id.clone()]
            },
            name: member.name.clone(),
            email: member.email.clone(),
            role: Some(member.role.clone()),
            assigned: true,
            last_login: member.last_login,
            totals: Totals::default(),
            orders: Vec::new(),
            last_handled: None,
        })
        .collect();

    for order in orders {
        let Some(handler) = order.handled_by.as_deref() else {
            continue;
        };
        if hidden.contains(handler) {
            continue;
        }
        let index = match staff.iter().position(|member| member.matches(handler)) {
            Some(index) => index,
            None => match rows
                .iter()
                .position(|row| !row.assigned && row.user_id == handler)
            {
                Some(index) => index,
                None => {
                    let other = others.get(handler);
                    rows.push(StaffRow {
                        user_id: handler.to_owned(),
                        aliases: Vec::new(),
                        name: other
                            .map(|o| o.name.clone())
                            .unwrap_or_else(|| "Former staff".into()),
                        email: other.and_then(|o| o.email.clone()),
                        role: None,
                        assigned: false,
                        last_login: other.and_then(|o| o.last_login),
                        totals: Totals::default(),
                        orders: Vec::new(),
                        last_handled: None,
                    });
                    rows.len() - 1
                }
            },
        };
        let row = &mut rows[index];
        row.totals.add(order);
        row.orders.push(order);
        row.last_handled = row.last_handled.max(Some(order.updated_at));
    }

    // Captains first — every one of them, even idle — then owners and anyone
    // else who handled orders; busiest first within each group.
    let rank = |row: &StaffRow| match row.role.as_deref() {
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
    rows
}

fn row_json(row: &StaffRow, total_revenue: f64) -> Value {
    let mut value = row.totals.json(total_revenue);
    value["userId"] = json!(row.user_id);
    value["name"] = json!(row.name);
    value["email"] = json!(row.email);
    value["role"] = json!(row.role);
    value["assigned"] = json!(row.assigned);
    value["lastHandledAt"] = json!(row.last_handled);
    value["lastSeenAt"] = json!(row.last_login);
    value["recentOrders"] = Value::Array(
        row.orders
            .iter()
            .take(RECENT_PER_STAFF)
            .map(|o| order_json(o))
            .collect(),
    );
    value
}

/// Rolls the range's orders up for the shop and per staff member.
fn build_report(orders: &[OrderFact], rows: &[StaffRow]) -> Value {
    let mut summary = Totals::default();
    for order in orders {
        summary.add(order);
    }
    let unattributed = orders.iter().filter(|o| o.handled_by.is_none()).count();
    let captains: Vec<Value> = rows
        .iter()
        .map(|row| row_json(row, summary.revenue))
        .collect();
    let mut summary_json = summary.json(summary.revenue);
    summary_json["unattributedOrders"] = json!(unattributed);
    json!({ "summary": summary_json, "captains": captains })
}

/// One person's figures, a day-by-day series across the whole range (idle
/// days included) and one page of every order they handled, newest first.
fn captain_detail(
    row: &StaffRow,
    range: DateRange,
    total_revenue: f64,
    page: usize,
    page_size: usize,
    shop_key: &str,
) -> Value {
    let mut days: Vec<(NaiveDate, Totals)> = range
        .from
        .iter_days()
        .take_while(|day| *day <= range.to)
        .map(|day| (day, Totals::default()))
        .collect();
    for order in &row.orders {
        if let Ok(index) = days.binary_search_by(|(day, _)| day.cmp(&order.day)) {
            days[index].1.add(order);
        }
    }
    let daily: Vec<Value> = days
        .iter()
        .map(|(day, totals)| {
            json!({
                "date": day.to_string(),
                "orders": totals.orders,
                "completedOrders": totals.completed,
                "revenue": round_money(totals.revenue),
                "profit": round_money(totals.profit()),
            })
        })
        .collect();

    let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
    let total = row.orders.len();
    let total_pages = total.div_ceil(page_size).max(1);
    let page = page.clamp(1, total_pages);
    let orders: Vec<Value> = row
        .orders
        .iter()
        .skip((page - 1) * page_size)
        .take(page_size)
        .map(|order| detailed_order_json(order, &row.name, shop_key))
        .collect();

    let mut value = row_json(row, total_revenue);
    value["daily"] = Value::Array(daily);
    value["orders"] = Value::Array(orders);
    value["page"] = json!(page);
    value["pageSize"] = json!(page_size);
    value["totalOrders"] = json!(total);
    value["totalPages"] = json!(total_pages);
    value
}

async fn shop_orders(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    shop_key: &str,
    range: DateRange,
) -> ApiResult<Vec<OrderFact>> {
    // Orders whose `store` predates the shop register resolve to the shop of
    // the matching category, exactly as ordering and the sales dashboard do.
    #[allow(clippy::type_complexity)]
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
            NaiveDate,
            String,
            Option<i32>,
            Value,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        r#"WITH scoped AS (
          SELECT o.id, o.order_number, o.customer_name, o.status, o.handled_by,
                 o.total::float8 AS total,
                 CASE WHEN jsonb_typeof(o.lines)='array' THEN o.lines ELSE '[]'::jsonb END AS lines,
                 o.fulfilment_mode, o.token_number,
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
          (x.created_at AT TIME ZONE $3)::date AS day,
          x.fulfilment_mode, x.token_number, x.lines,
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
            day: r.8,
            fulfilment_mode: r.9,
            token_number: r.10,
            lines: r.11,
            created_at: r.12,
            updated_at: r.13,
        })
        .collect())
}

/// Exactly the live accounts the admin has actively assigned to the shop.
/// An assignment names its account by id or, on older rows, by email.
async fn shop_staff(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    shop_key: &str,
) -> ApiResult<Vec<StaffMember>> {
    #[allow(clippy::type_complexity)]
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<DateTime<Utc>>,
        ),
    >(&format!(
        r#"SELECT DISTINCT ON (u.id)
             a.user_id, a.assignment_role, u.id::text,
             COALESCE(NULLIF(trim(u.display_name), ''), u.email, a.user_id), u.email,
             u.last_login_at
           FROM campus_ops.shop_user_assignments a
           JOIN campus_ops.shops s ON s.tenant_id=a.tenant_id AND s.id=a.shop_id
           JOIN identity.users u
             ON u.id::text=a.user_id OR lower(u.email)=lower(a.user_id)
           WHERE a.tenant_id=$1 AND s.shop_key=$2 AND a.is_active
             AND {LIVE_ACCOUNT_SQL}
           ORDER BY u.id, CASE a.assignment_role WHEN 'captain' THEN 0 ELSE 1 END"#
    ))
    .bind(tenant)
    .bind(shop_key)
    .fetch_all(pool)
    .await?;
    let mut staff: Vec<StaffMember> = rows
        .into_iter()
        .map(|r| StaffMember {
            assignment_user_id: r.0,
            role: r.1,
            identity_id: r.2,
            name: r.3,
            email: r.4,
            last_login: r.5,
        })
        .collect();
    staff.sort_by(|a, b| {
        a.role
            .cmp(&b.role)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(staff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn day(raw: &str) -> NaiveDate {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").unwrap()
    }

    fn order(
        handler: Option<&str>,
        status: &str,
        total: f64,
        cost: f64,
        minutes: i64,
    ) -> OrderFact {
        order_on("2026-09-01", handler, status, total, cost, minutes)
    }

    fn order_on(
        on: &str,
        handler: Option<&str>,
        status: &str,
        total: f64,
        cost: f64,
        minutes: i64,
    ) -> OrderFact {
        let placed = day(on);
        let created = Utc.from_utc_datetime(&placed.and_hms_opt(5, 0, 0).unwrap());
        OrderFact {
            id: Uuid::new_v4(),
            order_number: 1,
            customer_name: "Student".into(),
            status: status.into(),
            handled_by: handler.map(str::to_owned),
            total,
            cost,
            items: 2,
            day: placed,
            fulfilment_mode: "pickup".into(),
            token_number: None,
            lines: json!([{ "itemId": "dosa", "name": "Dosa", "price": total, "quantity": 2 }]),
            created_at: created,
            updated_at: created + chrono::Duration::minutes(minutes),
        }
    }

    fn member(id: &str, role: &str, name: &str) -> StaffMember {
        StaffMember {
            assignment_user_id: id.into(),
            identity_id: id.into(),
            role: role.into(),
            name: name.into(),
            email: Some(format!("{}@mec.local", name.to_lowercase())),
            last_login: None,
        }
    }

    fn report(
        orders: &[OrderFact],
        staff: &[StaffMember],
        others: &HashMap<String, OtherHandler>,
        hidden: &HashSet<String>,
    ) -> Value {
        build_report(orders, &staff_rows(orders, staff, others, hidden))
    }

    #[test]
    fn ranges_default_to_today_and_reject_bad_input() {
        let today = day("2026-09-28");
        assert_eq!(
            DateRange::parse(None, None, today).unwrap(),
            DateRange {
                from: today,
                to: today
            }
        );
        let range = DateRange::parse(Some("2026-09-01"), Some("2026-09-30"), today).unwrap();
        assert_eq!(range.days(), 30);
        assert_eq!(
            DateRange::parse(Some("2026-09-05"), None, today)
                .unwrap()
                .days(),
            1
        );
        assert!(DateRange::parse(Some("2026-09-30"), Some("2026-09-01"), today).is_err());
        assert!(DateRange::parse(Some("30/09/2026"), None, today).is_err());
        assert!(DateRange::parse(Some("2020-01-01"), Some("2026-01-01"), today).is_err());
    }

    #[test]
    fn assigned_mec_local_captains_are_listed_like_anyone_else() {
        // The admin's Counter staff list is the membership: an address on the
        // campus's own `.local` domain is a real captain, not a demo.
        let staff = vec![
            member("kesava", "captain", "Kesava"),
            member("purusoth", "captain", "Purusoth"),
            member("shashi", "captain", "Shashi"),
            member("yuvaraj", "captain", "Yuvaraj"),
        ];
        let orders = vec![order(Some("shashi"), "completed", 80.0, 50.0, 12)];
        let report = report(&orders, &staff, &HashMap::new(), &HashSet::new());
        let captains = report["captains"].as_array().unwrap();
        let names: Vec<&str> = captains
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Shashi", "Kesava", "Purusoth", "Yuvaraj"]);
        assert_eq!(captains[0]["email"], "shashi@mec.local");
        assert!(captains.iter().all(|c| c["assigned"] == true));
    }

    #[test]
    fn deactivated_handlers_are_not_ranked_but_stay_in_the_totals() {
        let staff = vec![member("cap-a", "captain", "Anu")];
        let orders = vec![
            order(Some("cap-a"), "completed", 100.0, 60.0, 10),
            order(Some("deleted-one"), "completed", 40.0, 20.0, 10),
        ];
        let hidden: HashSet<String> = ["deleted-one".to_string()].into();
        let report = report(&orders, &staff, &HashMap::new(), &hidden);
        assert_eq!(report["summary"]["revenue"], 140.0);
        let captains = report["captains"].as_array().unwrap();
        assert_eq!(captains.len(), 1);
        assert_eq!(captains[0]["name"], "Anu");
    }

    #[test]
    fn no_assignments_means_no_captains() {
        let report = report(&[], &[], &HashMap::new(), &HashSet::new());
        assert!(report["captains"].as_array().unwrap().is_empty());
        assert_eq!(report["summary"]["orders"], 0);
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
        others.insert(
            "gone".to_string(),
            OtherHandler {
                name: "Chitra".into(),
                email: None,
                last_login: None,
            },
        );
        let report = report(&orders, &staff, &others, &HashSet::new());

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
        let names: Vec<&str> = captains
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
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
            assignment_user_id: "dev@mec.local".into(),
            identity_id: "uuid-1".into(),
            role: "captain".into(),
            name: "Dev".into(),
            email: Some("dev@mec.local".into()),
            last_login: None,
        }];
        let orders = vec![
            order(Some("uuid-1"), "completed", 10.0, 5.0, 3),
            order(Some("dev@mec.local"), "completed", 20.0, 5.0, 3),
        ];
        let report = report(&orders, &staff, &HashMap::new(), &HashSet::new());
        let captains = report["captains"].as_array().unwrap();
        assert_eq!(captains.len(), 1);
        assert_eq!(captains[0]["orders"], 2);
        assert_eq!(captains[0]["userId"], "uuid-1");
    }

    #[test]
    fn captain_detail_has_a_daily_series_and_paginated_orders() {
        let staff = vec![member("cap-a", "captain", "Anu")];
        let mut orders = vec![
            order_on("2026-09-03", Some("cap-a"), "completed", 100.0, 60.0, 10),
            order_on("2026-09-03", Some("cap-a"), "rejected", 30.0, 10.0, 2),
            order_on("2026-09-01", Some("cap-a"), "completed", 50.0, 20.0, 10),
            order_on("2026-09-01", Some("other"), "completed", 50.0, 20.0, 10),
        ];
        // Newest first, as the query returns them.
        orders.sort_by(|a, b| b.day.cmp(&a.day));
        let rows = staff_rows(&orders, &staff, &HashMap::new(), &HashSet::new());
        let anu = rows.iter().find(|row| row.user_id == "cap-a").unwrap();
        let range = DateRange {
            from: day("2026-09-01"),
            to: day("2026-09-04"),
        };

        let detail = captain_detail(anu, range, total_revenue(&orders), 1, 2, "mec-canteen");
        let daily = detail["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 4);
        assert_eq!(daily[0]["date"], "2026-09-01");
        assert_eq!(daily[0]["orders"], 1);
        assert_eq!(daily[0]["revenue"], 50.0);
        assert_eq!(daily[1]["orders"], 0);
        assert_eq!(daily[2]["orders"], 2);
        assert_eq!(daily[2]["completedOrders"], 1);
        assert_eq!(daily[2]["revenue"], 100.0);
        assert_eq!(daily[2]["profit"], 40.0);
        assert_eq!(detail["totalOrders"], 3);
        assert_eq!(detail["totalPages"], 2);
        assert_eq!(detail["revenueShare"], 75.0);
        let first = detail["orders"].as_array().unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0]["captainName"], "Anu");
        assert_eq!(first[0]["shopKey"], "mec-canteen");
        assert_eq!(first[0]["fulfilmentMode"], "pickup");
        assert_eq!(first[0]["lines"][0]["name"], "Dosa");

        let second = captain_detail(anu, range, total_revenue(&orders), 2, 2, "mec-canteen");
        assert_eq!(second["page"], 2);
        assert_eq!(second["orders"].as_array().unwrap().len(), 1);
        // Out-of-range pages clamp to the last one.
        let clamped = captain_detail(anu, range, total_revenue(&orders), 9, 2, "mec-canteen");
        assert_eq!(clamped["page"], 2);
    }
}

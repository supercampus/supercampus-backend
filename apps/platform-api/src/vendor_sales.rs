//! Campus shop sales: the dashboard and order ledger behind the app's
//! "Vendors & Orders" workspace.
//!
//! Every figure comes from two sale sources: canteen/stationery orders
//! (`campus_ops.canteen_orders`) and laundry charges
//! (`campus_ops.laundry_charges`). Revenue is completed sales only (completed
//! orders, paid laundry charges); rejected orders are refunded and cancelled
//! ones never charged, so neither counts. "Today", "this week" and "this
//! month" are the tenant's local calendar (Asia/Kolkata), never the database
//! server's clock.

use std::collections::BTreeMap;

use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::{require_any, tenant_id},
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/canteen/sales-dashboard", get(sales_dashboard))
        .route("/canteen/sales-dashboard/orders", get(sales_orders))
        // One shop over an explicit date range, with per-captain performance.
        .route(
            "/canteen/shop-analytics",
            get(crate::shop_analytics::shop_analytics),
        )
        // Finance reports (PDF / CSV in the app) over the same sales and ledger.
        .merge(crate::reports::router())
}

/// Grants that may read shop sales.
const SALES_GRANTS: &[&str] = &[
    "vendor_management.vendors.read",
    "canteen.analytics.read",
    "canteen.orders.manage",
];

/// The campus calendar every period boundary is drawn in.
const TENANT_TIMEZONE: &str = "Asia/Kolkata";

/// Every sale on campus as one row set. `$1` is the tenant, `$2` the time
/// zone. Orders whose `store` predates the shop register (e.g. `classic`)
/// resolve to the shop of the matching category, exactly as ordering does.
pub(crate) const SALES_CTE: &str = r#"
WITH shop_list AS (
  SELECT shop_key, name, category, created_at FROM campus_ops.shops WHERE tenant_id=$1
),
bounds AS (
  SELECT (now() AT TIME ZONE $2) AS now_local,
         date_trunc('day', now() AT TIME ZONE $2) AS today_start,
         date_trunc('week', now() AT TIME ZONE $2) AS week_start,
         date_trunc('month', now() AT TIME ZONE $2) AS month_start
),
sales AS (
  SELECT o.id, 'order'::text AS kind, o.order_number AS number,
    o.customer_name, COALESCE(exact.shop_key, fallback.shop_key, o.store) AS shop_key,
    o.total::float8 AS total, o.status,
    CASE WHEN o.status='completed' THEN 'completed'
         WHEN o.status IN ('cancelled','rejected') THEN 'cancelled'
         ELSE 'active' END AS bucket,
    o.fulfilment_mode, o.lines, o.token_number, o.rejection_reason,
    o.created_at, o.updated_at, (o.created_at AT TIME ZONE $2) AS local_at
  FROM campus_ops.canteen_orders o
  LEFT JOIN shop_list exact ON exact.shop_key=o.store
  LEFT JOIN LATERAL (
    SELECT s.shop_key FROM shop_list s
    WHERE exact.shop_key IS NULL AND lower(s.category)=CASE
      WHEN lower(o.store) LIKE '%laundry%' THEN 'laundry'
      WHEN lower(o.store) LIKE '%station%' THEN 'stationery'
      ELSE 'canteen' END
    ORDER BY s.created_at, s.shop_key LIMIT 1
  ) fallback ON true
  WHERE o.tenant_id=$1
  UNION ALL
  SELECT c.id, 'laundry'::text, NULL::bigint,
    COALESCE(u.display_name, 'Not yet claimed'), c.shop_key,
    c.total::float8, c.status,
    CASE c.status WHEN 'paid' THEN 'completed' WHEN 'cancelled' THEN 'cancelled' ELSE 'active' END,
    NULL::text,
    jsonb_build_array(jsonb_build_object('name', c.name, 'quantity', c.quantity::float8,
      'unitLabel', c.unit_label, 'price', c.unit_price::float8, 'lineTotal', c.total::float8)),
    NULL::int, NULL::text, c.created_at, c.updated_at, (c.created_at AT TIME ZONE $2)
  FROM campus_ops.laundry_charges c
  LEFT JOIN identity.users u ON u.id::text=c.claimed_by
  WHERE c.tenant_id=$1
)"#;

/// SQL predicate: the sale falls inside period `$3`.
const IN_PERIOD: &str = "(CASE $3 WHEN 'today' THEN x.local_at >= b.today_start \
     WHEN 'week' THEN x.local_at >= b.week_start \
     WHEN 'month' THEN x.local_at >= b.month_start ELSE true END)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SalesPeriod {
    Today,
    Week,
    Month,
    All,
}

impl SalesPeriod {
    fn parse(raw: Option<&str>) -> ApiResult<Self> {
        match raw
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("today") => Ok(Self::Today),
            Some("week") | Some("this_week") => Ok(Self::Week),
            Some("month") | Some("this_month") => Ok(Self::Month),
            Some("all") | Some("all_time") => Ok(Self::All),
            Some(_) => Err(ApiError::BadRequest(
                "Period must be today, week, month or all".into(),
            )),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Week => "week",
            Self::Month => "month",
            Self::All => "all",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Week => "This week",
            Self::Month => "This month",
            Self::All => "All time",
        }
    }

    /// The width of one bar in the revenue-over-time chart.
    fn bucket_unit(self) -> &'static str {
        match self {
            Self::Today => "hour",
            Self::Week | Self::Month => "day",
            Self::All => "month",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusBucket {
    Completed,
    Active,
    Cancelled,
}

impl StatusBucket {
    fn parse(raw: &str) -> Self {
        match raw {
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Active,
        }
    }

    fn filter(raw: Option<&str>) -> ApiResult<Option<Self>> {
        match raw
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("all") => Ok(None),
            Some("completed") => Ok(Some(Self::Completed)),
            Some("active") | Some("in_progress") => Ok(Some(Self::Active)),
            Some("cancelled") => Ok(Some(Self::Cancelled)),
            Some(_) => Err(ApiError::BadRequest(
                "Status must be all, active, completed or cancelled".into(),
            )),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Active => "active",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Tally {
    count: i64,
    amount: f64,
}

/// Sales of one shop in one status bucket, measured over several windows.
#[derive(Debug, Clone, PartialEq)]
struct SalesRow {
    shop_key: String,
    bucket: StatusBucket,
    all: Tally,
    period: Tally,
    today: Tally,
    week_amount: f64,
    month_amount: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct SalesSummary {
    completed: i64,
    active: i64,
    cancelled: i64,
    /// Completed sales only.
    revenue: f64,
}

impl SalesSummary {
    fn from_rows<'a>(
        rows: impl IntoIterator<Item = &'a SalesRow>,
        window: impl Fn(&SalesRow) -> Tally,
    ) -> Self {
        let mut summary = Self::default();
        for row in rows {
            let tally = window(row);
            match row.bucket {
                StatusBucket::Completed => {
                    summary.completed += tally.count;
                    summary.revenue += tally.amount;
                }
                StatusBucket::Active => summary.active += tally.count,
                StatusBucket::Cancelled => summary.cancelled += tally.count,
            }
        }
        summary.revenue = round_money(summary.revenue);
        summary
    }

    fn orders(&self) -> i64 {
        self.completed + self.active + self.cancelled
    }

    /// Revenue per completed sale; 0 before the first one.
    fn average_order_value(&self) -> f64 {
        if self.completed <= 0 {
            0.0
        } else {
            round_money(self.revenue / self.completed as f64)
        }
    }

    fn percent(&self, count: i64) -> f64 {
        share_percent(count as f64, self.orders() as f64)
    }
}

/// Whole-number share; 0 when there is nothing to share.
fn share_percent(part: f64, whole: f64) -> f64 {
    if whole <= 0.0 || part <= 0.0 {
        0.0
    } else {
        ((part / whole) * 100.0).round()
    }
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// "canteen" → "Canteen", "mec-stationery" → "Mec Stationery".
fn title_case(raw: &str) -> String {
    raw.split(['_', '-', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq)]
struct ShopInfo {
    id: Option<String>,
    shop_key: String,
    name: String,
    category: String,
    is_active: bool,
    is_open: bool,
    operators: Value,
}

#[derive(Debug, Clone, PartialEq)]
struct StoreSales {
    shop: ShopInfo,
    period: SalesSummary,
    today: SalesSummary,
    all: SalesSummary,
}

/// One entry per registered shop, plus any shop key sales still name that the
/// register no longer holds, busiest first.
fn store_sales(shops: &[ShopInfo], rows: &[SalesRow]) -> Vec<StoreSales> {
    let mut by_key: BTreeMap<&str, Vec<&SalesRow>> = BTreeMap::new();
    for row in rows {
        by_key.entry(row.shop_key.as_str()).or_default().push(row);
    }
    let mut stores: Vec<StoreSales> = shops
        .iter()
        .map(|shop| {
            let own = by_key.remove(shop.shop_key.as_str()).unwrap_or_default();
            StoreSales {
                shop: shop.clone(),
                period: SalesSummary::from_rows(own.iter().copied(), |r| r.period),
                today: SalesSummary::from_rows(own.iter().copied(), |r| r.today),
                all: SalesSummary::from_rows(own.iter().copied(), |r| r.all),
            }
        })
        .collect();
    for (key, own) in by_key {
        stores.push(StoreSales {
            shop: ShopInfo {
                id: None,
                shop_key: key.to_owned(),
                name: title_case(key),
                category: "Other".into(),
                is_active: false,
                is_open: false,
                operators: json!([]),
            },
            period: SalesSummary::from_rows(own.iter().copied(), |r| r.period),
            today: SalesSummary::from_rows(own.iter().copied(), |r| r.today),
            all: SalesSummary::from_rows(own.iter().copied(), |r| r.all),
        });
    }
    stores.sort_by(|a, b| {
        b.period
            .revenue
            .total_cmp(&a.period.revenue)
            .then(b.period.orders().cmp(&a.period.orders()))
            .then(a.shop.name.cmp(&b.shop.name))
    });
    stores
}

#[derive(Deserialize)]
struct DashboardQuery {
    period: Option<String>,
}

async fn sales_dashboard(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<DashboardQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(&access, SALES_GRANTS)?;
    let period = SalesPeriod::parse(query.period.as_deref())?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let pool = db.pool();
    let tenant = tenant_id(pool, &principal.student.tenant_id).await?;

    let rows = sales_rows(pool, tenant, period).await?;
    let shops = shop_register(pool, tenant).await?;
    let trend = sales_trend(pool, tenant, period).await?;
    let top_items = top_items(pool, tenant, period).await?;
    let range = sqlx::query_as::<_, (String, String)>(
        r#"SELECT to_char(CASE $1 WHEN 'today' THEN date_trunc('day', now() AT TIME ZONE $2)
                 WHEN 'week' THEN date_trunc('week', now() AT TIME ZONE $2)
                 WHEN 'month' THEN date_trunc('month', now() AT TIME ZONE $2)
                 ELSE COALESCE((SELECT min(created_at AT TIME ZONE $2) FROM campus_ops.canteen_orders WHERE tenant_id=$3),
                               date_trunc('day', now() AT TIME ZONE $2)) END, 'YYYY-MM-DD"T"HH24:MI:SS'),
             to_char(now() AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS')"#,
    )
    .bind(period.key())
    .bind(TENANT_TIMEZONE)
    .bind(tenant)
    .fetch_one(pool)
    .await?;
    // The newest sales regardless of period, for app builds that still read
    // `recentOrders` from here instead of the orders ledger.
    let (_, recent) = orders_page(pool, tenant, SalesPeriod::All, None, None, 25).await?;

    let mut body = dashboard_json(period, &rows, &shops, trend, top_items, range);
    body["recentOrders"] = recent;
    Ok(Json(ApiResponse::new(body)))
}

fn dashboard_json(
    period: SalesPeriod,
    rows: &[SalesRow],
    shops: &[ShopInfo],
    trend: Value,
    top_items: Value,
    range: (String, String),
) -> Value {
    let summary = SalesSummary::from_rows(rows, |r| r.period);
    let today = SalesSummary::from_rows(rows, |r| r.today);
    let all = SalesSummary::from_rows(rows, |r| r.all);
    let week_revenue = round_money(
        rows.iter()
            .filter(|r| r.bucket == StatusBucket::Completed)
            .map(|r| r.week_amount)
            .sum(),
    );
    let month_revenue = round_money(
        rows.iter()
            .filter(|r| r.bucket == StatusBucket::Completed)
            .map(|r| r.month_amount)
            .sum(),
    );
    let stores = store_sales(shops, rows);
    let store_json: Vec<Value> = stores
        .iter()
        .map(|store| {
            json!({
                "id": store.shop.id,
                "shopKey": store.shop.shop_key,
                "name": store.shop.name,
                "category": title_case(&store.shop.category),
                "isActive": store.shop.is_active,
                "isOpen": store.shop.is_open,
                "operators": store.shop.operators,
                "orders": store.period.orders(),
                "completedOrders": store.period.completed,
                "cancelledOrders": store.period.cancelled,
                "revenue": store.period.revenue,
                "revenueShare": share_percent(store.period.revenue, summary.revenue),
                "averageOrderValue": store.period.average_order_value(),
                "ordersToday": store.today.orders(),
                "revenueToday": store.today.revenue,
                "activeNow": store.all.active,
                // Kept for app builds that read the earlier shape.
                "totalOrders": store.all.orders(),
                "totalRevenue": store.all.revenue,
                "activeOrders": store.all.active,
            })
        })
        .collect();
    let food_revenue: f64 = stores
        .iter()
        .filter(|s| s.shop.category.eq_ignore_ascii_case("canteen"))
        .map(|s| s.all.revenue)
        .sum();
    let other_revenue = round_money(all.revenue - food_revenue);

    json!({
        "period": period.key(),
        "periodLabel": period.label(),
        "timezone": TENANT_TIMEZONE,
        "rangeStart": range.0,
        "generatedAt": range.1,
        "summary": {
            "orders": summary.orders(),
            "completedOrders": summary.completed,
            "activeOrders": summary.active,
            "cancelledOrders": summary.cancelled,
            "revenue": summary.revenue,
            "averageOrderValue": summary.average_order_value(),
            "pendingNow": all.active,
        },
        "statusBreakdown": {
            "completed": summary.completed,
            "active": summary.active,
            "cancelled": summary.cancelled,
        },
        "stores": store_json,
        "trend": trend,
        "topItems": top_items,

        // The earlier all-time shape, for app builds that still read it.
        "platformOrders": all.orders(),
        "ordersToday": today.orders(),
        "ordersTodayTrend": format!("{} new today", today.orders()),
        "revenue": all.revenue,
        "revenueToday": today.revenue,
        "revenueTodayTrend": format!("₹{:.0} today", today.revenue),
        "monthlyRevenue": month_revenue,
        "weeklyRevenue": week_revenue,
        "weeklyRevenueTrend": format!("₹{:.0} this week", week_revenue),
        "pendingActions": all.active,
        "pendingRequests": all.active,
        "pendingApprovals": 0,
        "pendingActionsTrend": format!("{} in the queue", all.active),
        "orderStatusDistribution": {
            "completed": all.percent(all.completed),
            "cancelled": all.percent(all.cancelled),
            "pending": all.percent(all.active),
            "completedCount": all.completed,
            "cancelledCount": all.cancelled,
            "pendingCount": all.active,
            "totalCount": all.orders(),
        },
        "paymentSplit": {
            "ordersPercentage": share_percent(food_revenue, all.revenue),
            "adhocPercentage": share_percent(other_revenue, all.revenue),
            "ordersRevenue": round_money(food_revenue),
            "adhocRevenue": other_revenue,
        },
        "shops": store_json,
    })
}

async fn sales_rows(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    period: SalesPeriod,
) -> ApiResult<Vec<SalesRow>> {
    let sql = format!(
        r#"{SALES_CTE}
        SELECT x.shop_key, x.bucket,
          count(*)::int8, COALESCE(sum(x.total), 0)::float8,
          count(*) FILTER (WHERE {IN_PERIOD})::int8,
          COALESCE(sum(x.total) FILTER (WHERE {IN_PERIOD}), 0)::float8,
          count(*) FILTER (WHERE x.local_at >= b.today_start)::int8,
          COALESCE(sum(x.total) FILTER (WHERE x.local_at >= b.today_start), 0)::float8,
          COALESCE(sum(x.total) FILTER (WHERE x.local_at >= b.week_start), 0)::float8,
          COALESCE(sum(x.total) FILTER (WHERE x.local_at >= b.month_start), 0)::float8
        FROM sales x CROSS JOIN bounds b
        GROUP BY x.shop_key, x.bucket"#
    );
    let rows = sqlx::query_as::<_, (String, String, i64, f64, i64, f64, i64, f64, f64, f64)>(&sql)
        .bind(tenant)
        .bind(TENANT_TIMEZONE)
        .bind(period.key())
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| SalesRow {
            shop_key: r.0,
            bucket: StatusBucket::parse(&r.1),
            all: Tally {
                count: r.2,
                amount: r.3,
            },
            period: Tally {
                count: r.4,
                amount: r.5,
            },
            today: Tally {
                count: r.6,
                amount: r.7,
            },
            week_amount: r.8,
            month_amount: r.9,
        })
        .collect())
}

async fn shop_register(pool: &sqlx::PgPool, tenant: Uuid) -> ApiResult<Vec<ShopInfo>> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, bool, bool, Value)>(
        r#"SELECT s.id, s.shop_key, s.name, s.category, s.is_active, s.shop_open,
             COALESCE((SELECT jsonb_agg(jsonb_build_object(
                 'userId', a.user_id,
                 'name', COALESCE(u.display_name, a.user_id),
                 'role', a.assignment_role)
               ORDER BY (a.assignment_role='owner') DESC, u.display_name)
               FROM campus_ops.shop_user_assignments a
               LEFT JOIN identity.users u ON u.id::text=a.user_id
               WHERE a.tenant_id=s.tenant_id AND a.shop_id=s.id AND a.is_active), '[]'::jsonb)
           FROM campus_ops.shops s WHERE s.tenant_id=$1 ORDER BY s.name"#,
    )
    .bind(tenant)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ShopInfo {
            id: Some(r.0.to_string()),
            shop_key: r.1,
            name: r.2,
            category: r.3,
            is_active: r.4,
            is_open: r.5,
            operators: r.6,
        })
        .collect())
}

/// Completed revenue and order counts per bar, zero-filled across the whole
/// period so gaps show as empty days rather than disappearing.
async fn sales_trend(pool: &sqlx::PgPool, tenant: Uuid, period: SalesPeriod) -> ApiResult<Value> {
    let sql = format!(
        r#"{SALES_CTE},
        series AS (
          SELECT generate_series(
            CASE $3 WHEN 'today' THEN b.today_start
                    WHEN 'week' THEN b.week_start
                    WHEN 'month' THEN b.month_start
                    ELSE GREATEST(b.month_start - interval '11 months',
                      COALESCE((SELECT date_trunc('month', min(local_at)) FROM sales), b.month_start)) END,
            CASE $3 WHEN 'today' THEN b.today_start + interval '23 hours'
                    WHEN 'week' THEN b.week_start + interval '6 days'
                    WHEN 'month' THEN b.month_start + interval '1 month' - interval '1 day'
                    ELSE b.month_start END,
            CASE $3 WHEN 'today' THEN interval '1 hour'
                    WHEN 'all' THEN interval '1 month'
                    ELSE interval '1 day' END) AS slot
          FROM bounds b
        )
        SELECT COALESCE(jsonb_agg(jsonb_build_object(
            'start', to_char(t.slot, 'YYYY-MM-DD"T"HH24:MI:SS'),
            'orders', t.orders,
            'completedOrders', t.completed,
            'revenue', t.revenue,
            'isFuture', t.slot > (SELECT now_local FROM bounds)
          ) ORDER BY t.slot), '[]'::jsonb)
        FROM (
          SELECT s.slot,
            count(x.id) AS orders,
            count(x.id) FILTER (WHERE x.bucket='completed') AS completed,
            COALESCE(sum(x.total) FILTER (WHERE x.bucket='completed'), 0)::float8 AS revenue
          FROM series s
          LEFT JOIN sales x ON date_trunc($4, x.local_at)=s.slot
          GROUP BY s.slot
        ) t"#
    );
    let points = sqlx::query_scalar::<_, Value>(&sql)
        .bind(tenant)
        .bind(TENANT_TIMEZONE)
        .bind(period.key())
        .bind(period.bucket_unit())
        .fetch_one(pool)
        .await?;
    Ok(json!({ "unit": period.bucket_unit(), "points": points }))
}

/// Best sellers by quantity from completed order lines in the period.
async fn top_items(pool: &sqlx::PgPool, tenant: Uuid, period: SalesPeriod) -> ApiResult<Value> {
    let sql = format!(
        r#"{SALES_CTE}
        SELECT COALESCE(jsonb_agg(item ORDER BY (item->>'quantity')::float8 DESC,
                                   (item->>'revenue')::float8 DESC), '[]'::jsonb)
        FROM (
          SELECT jsonb_build_object(
              'name', line->>'name',
              'shopKey', x.shop_key,
              'quantity', sum(COALESCE((line->>'quantity')::numeric, 0))::float8,
              'revenue', round(sum(COALESCE((line->>'price')::numeric, 0)
                                  * COALESCE((line->>'quantity')::numeric, 0)), 2)::float8) AS item
          FROM sales x CROSS JOIN bounds b
          CROSS JOIN LATERAL jsonb_array_elements(x.lines) line
          WHERE x.kind='order' AND x.bucket='completed' AND {IN_PERIOD}
          GROUP BY line->>'name', x.shop_key
          ORDER BY sum(COALESCE((line->>'quantity')::numeric, 0)) DESC
          LIMIT 5
        ) ranked"#
    );
    Ok(sqlx::query_scalar::<_, Value>(&sql)
        .bind(tenant)
        .bind(TENANT_TIMEZONE)
        .bind(period.key())
        .fetch_one(pool)
        .await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrdersQuery {
    period: Option<String>,
    store: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
}

/// Orders and laundry charges across every shop, newest first.
async fn sales_orders(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<OrdersQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(&access, SALES_GRANTS)?;
    let period = SalesPeriod::parse(query.period.as_deref().or(Some("all")))?;
    let status = StatusBucket::filter(query.status.as_deref())?;
    let store = query
        .store
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all"))
        .map(str::to_owned);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let pool = db.pool();
    let tenant = tenant_id(pool, &principal.student.tenant_id).await?;
    let (total, orders) = orders_page(pool, tenant, period, store, status, limit).await?;
    Ok(Json(ApiResponse::new(json!({
        "period": period.key(),
        "total": total,
        "orders": orders,
    }))))
}

async fn orders_page(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    period: SalesPeriod,
    store: Option<String>,
    status: Option<StatusBucket>,
    limit: i64,
) -> ApiResult<(i64, Value)> {
    let sql = format!(
        r#"{SALES_CTE},
        matching AS (
          SELECT x.* FROM sales x CROSS JOIN bounds b
          WHERE {IN_PERIOD}
            AND ($4::text IS NULL OR x.shop_key=$4)
            AND ($5::text IS NULL OR x.bucket=$5)
        )
        SELECT (SELECT count(*) FROM matching)::int8,
          COALESCE((SELECT jsonb_agg(jsonb_build_object(
            'id', m.id,
            'kind', m.kind,
            'orderNumber', m.number,
            'customerName', m.customer_name,
            'shopKey', m.shop_key,
            'store', m.shop_key,
            'storeName', COALESCE(s.name, m.shop_key),
            'category', COALESCE(s.category, 'Other'),
            'total', m.total,
            'status', m.status,
            'statusBucket', m.bucket,
            'fulfilmentMode', m.fulfilment_mode,
            'tokenNumber', m.token_number,
            'rejectionReason', m.rejection_reason,
            'lines', m.lines,
            'createdAt', m.created_at,
            'updatedAt', m.updated_at
          ) ORDER BY m.created_at DESC)
          FROM (SELECT * FROM matching ORDER BY created_at DESC LIMIT $6) m
          LEFT JOIN shop_list s ON s.shop_key=m.shop_key), '[]'::jsonb)"#
    );
    Ok(sqlx::query_as::<_, (i64, Value)>(&sql)
        .bind(tenant)
        .bind(TENANT_TIMEZONE)
        .bind(period.key())
        .bind(store)
        .bind(status.map(StatusBucket::key))
        .bind(limit)
        .fetch_one(pool)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(shop: &str, bucket: StatusBucket, period: (i64, f64), all: (i64, f64)) -> SalesRow {
        SalesRow {
            shop_key: shop.into(),
            bucket,
            all: Tally {
                count: all.0,
                amount: all.1,
            },
            period: Tally {
                count: period.0,
                amount: period.1,
            },
            today: Tally::default(),
            week_amount: 0.0,
            month_amount: 0.0,
        }
    }

    fn shop(key: &str, name: &str, category: &str) -> ShopInfo {
        ShopInfo {
            id: Some(format!("id-{key}")),
            shop_key: key.into(),
            name: name.into(),
            category: category.into(),
            is_active: true,
            is_open: true,
            operators: json!([]),
        }
    }

    #[test]
    fn periods_parse_with_today_as_default() {
        assert_eq!(SalesPeriod::parse(None).unwrap(), SalesPeriod::Today);
        assert_eq!(SalesPeriod::parse(Some("Week")).unwrap(), SalesPeriod::Week);
        assert_eq!(
            SalesPeriod::parse(Some("this_month")).unwrap(),
            SalesPeriod::Month
        );
        assert_eq!(SalesPeriod::parse(Some("all")).unwrap(), SalesPeriod::All);
        assert!(SalesPeriod::parse(Some("yesterday")).is_err());
        assert_eq!(SalesPeriod::Today.bucket_unit(), "hour");
        assert_eq!(SalesPeriod::Month.bucket_unit(), "day");
        assert_eq!(SalesPeriod::All.bucket_unit(), "month");
    }

    #[test]
    fn status_filter_accepts_the_three_buckets() {
        assert_eq!(StatusBucket::filter(None).unwrap(), None);
        assert_eq!(StatusBucket::filter(Some("all")).unwrap(), None);
        assert_eq!(
            StatusBucket::filter(Some("active")).unwrap(),
            Some(StatusBucket::Active)
        );
        assert!(StatusBucket::filter(Some("rejected")).is_err());
        assert_eq!(StatusBucket::parse("cancelled"), StatusBucket::Cancelled);
        assert_eq!(StatusBucket::parse("pending"), StatusBucket::Active);
    }

    #[test]
    fn revenue_counts_completed_sales_only() {
        let rows = vec![
            row("canteen", StatusBucket::Completed, (3, 300.0), (10, 1000.0)),
            row("canteen", StatusBucket::Active, (2, 150.0), (2, 150.0)),
            row("canteen", StatusBucket::Cancelled, (1, 80.0), (4, 320.0)),
        ];
        let period = SalesSummary::from_rows(&rows, |r| r.period);
        assert_eq!(period.orders(), 6);
        assert_eq!(period.revenue, 300.0);
        assert_eq!(period.average_order_value(), 100.0);
        assert_eq!(period.percent(period.completed), 50.0);
        let all = SalesSummary::from_rows(&rows, |r| r.all);
        assert_eq!(all.orders(), 16);
        assert_eq!(all.revenue, 1000.0);
    }

    #[test]
    fn nothing_sold_means_zeros_not_invented_shares() {
        let empty = SalesSummary::from_rows(&[], |r| r.period);
        assert_eq!(empty.orders(), 0);
        assert_eq!(empty.average_order_value(), 0.0);
        assert_eq!(empty.percent(0), 0.0);
        assert_eq!(share_percent(0.0, 0.0), 0.0);
        assert_eq!(share_percent(25.0, 100.0), 25.0);
    }

    #[test]
    fn every_shop_appears_and_orphan_sales_are_kept() {
        let shops = vec![
            shop("mec-canteen", "Canteen", "canteen"),
            shop("mec-laundry", "Campus Laundry", "laundry"),
        ];
        let rows = vec![
            row("mec-canteen", StatusBucket::Completed, (1, 50.0), (1, 50.0)),
            row("bites", StatusBucket::Completed, (2, 120.0), (2, 120.0)),
        ];
        let stores = store_sales(&shops, &rows);
        assert_eq!(stores.len(), 3);
        // Busiest first by period revenue.
        assert_eq!(stores[0].shop.name, "Bites");
        assert_eq!(stores[0].shop.category, "Other");
        assert_eq!(stores[1].shop.shop_key, "mec-canteen");
        assert_eq!(stores[1].period.revenue, 50.0);
        assert_eq!(stores[2].shop.shop_key, "mec-laundry");
        assert_eq!(stores[2].period.orders(), 0);
    }

    #[test]
    fn dashboard_totals_match_the_store_breakdown() {
        let shops = vec![
            shop("mec-canteen", "Canteen", "canteen"),
            shop("stationery", "Stationery Store", "Stationery"),
        ];
        let rows = vec![
            row(
                "mec-canteen",
                StatusBucket::Completed,
                (2, 90.0),
                (5, 400.0),
            ),
            row("stationery", StatusBucket::Completed, (1, 30.0), (1, 30.0)),
            row("stationery", StatusBucket::Active, (1, 65.0), (2, 315.0)),
        ];
        let value = dashboard_json(
            SalesPeriod::Week,
            &rows,
            &shops,
            json!({}),
            json!([]),
            ("a".into(), "b".into()),
        );
        assert_eq!(value["summary"]["orders"], 4);
        assert_eq!(value["summary"]["revenue"], 120.0);
        assert_eq!(value["summary"]["averageOrderValue"], 40.0);
        assert_eq!(value["summary"]["pendingNow"], 2);
        let stores = value["stores"].as_array().unwrap();
        let store_revenue: f64 = stores.iter().map(|s| s["revenue"].as_f64().unwrap()).sum();
        assert_eq!(store_revenue, 120.0);
        assert_eq!(stores[0]["revenueShare"], 75.0);
        assert_eq!(stores[1]["category"], "Stationery");
        assert_eq!(value["paymentSplit"]["ordersRevenue"], 400.0);
        assert_eq!(value["paymentSplit"]["adhocRevenue"], 30.0);
    }

    #[test]
    fn title_case_names_unknown_shops() {
        assert_eq!(title_case("canteen"), "Canteen");
        assert_eq!(title_case("mec-stationery"), "Mec Stationery");
        assert_eq!(title_case("quick_bites"), "Quick Bites");
    }
}

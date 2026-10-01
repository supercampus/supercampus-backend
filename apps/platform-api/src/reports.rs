//! Finance reports for the accountant and administrators: the app's
//! "Reports" page asks for one kind over a period, then renders the tables it
//! gets back as a PDF or CSV on the device — and can email those files.
//!
//! `GET /reports/options` — the shops and menu items the parameter pickers offer.
//! `GET /reports/{kind}?from=YYYY-MM-DD&to=YYYY-MM-DD&month=YYYY-MM&shop=<key>&items=<id,id>`
//! `POST /reports/{kind}/email` — the same parameters plus `recipients` and the
//! PDF/CSV the app rendered (base64); see [`email_report`].
//!
//! * Every figure is read from the tenant's own records: the wallet ledger
//!   (`campus_ops.canteen_wallet_transactions`), wallets, orders
//!   (`campus_ops.canteen_orders`), laundry charges, payment requests and
//!   Razorpay orders. Days are the tenant's local calendar (Asia/Kolkata).
//! * Only kinds this platform actually records are offered; nothing is
//!   invented to fill a table.
//! * Reading tenant-wide balances is finance work: both the sales analytics
//!   grant and the wallet-ledger grant are required, and the fee kinds also
//!   need their own read grant.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    routing::{get, post},
};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::{require, tenant_id},
    state::{AppState, AuthPrincipal, EffectiveAccess},
    vendor_sales::SALES_CTE,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports/options", get(report_options))
        .route("/reports/{kind}", get(generate_report))
        .route(
            "/reports/{kind}/email",
            post(email_report).layer(DefaultBodyLimit::max(EMAIL_BODY_LIMIT)),
        )
}

/// Both are needed: sales figures, and every member's wallet ledger.
const REPORT_GRANTS: &[&str] = &["canteen.analytics.read", "canteen.wallet.top_up"];

const TENANT_TIMEZONE: &str = "Asia/Kolkata";

/// The longest range one report may cover (two years, leap day included).
const MAX_RANGE_DAYS: i64 = 731;

/// Day-by-day balances list every student for every day, so they stay short.
const MAX_EOD_DAYS: i64 = 31;

/// Ledger rows one report returns at most; a longer ledger says it was cut.
const MAX_LEDGER_ROWS: usize = 20_000;

/// Students listed by name in the spending report.
const TOP_SPENDERS: i64 = 25;

/// The shop a ledger row (aliased `t`) belongs to for a shop's figures: a
/// purchase or refund belongs to the counter the order was placed at, even
/// when the canteen's general credit paid for it; every other row (top-ups,
/// deductions) belongs to the wallet bucket it changed.
const TXN_SHOP_SQL: &str = "(CASE WHEN t.transaction_type IN ('order_debit','refund')      THEN COALESCE(t.counter_shop_key, t.shop_key) ELSE t.shop_key END)";

/// Whether a wallet bucket key (an SQL expression) is shop `$5` or one of
/// its counters: a canteen's wallet holds its general credit and the credit
/// restricted to each of its counters.
macro_rules! bucket_of_shop_sql {
    ($key:literal) => {
        concat!(
            "($5::text IS NULL OR ", $key, "=$5 OR EXISTS(SELECT 1 FROM campus_ops.shops counter_of",
            " WHERE counter_of.tenant_id=$1 AND counter_of.shop_key=", $key,
            " AND counter_of.parent_shop_key=$5))"
        )
    };
}

fn require_report_access(access: &EffectiveAccess) -> ApiResult<()> {
    for grant in REPORT_GRANTS {
        require(access, grant)?;
    }
    Ok(())
}

/// The report grants, plus the kind's own read grant where it has one.
fn require_kind_access(access: &EffectiveAccess, kind: ReportKind) -> ApiResult<()> {
    require_report_access(access)?;
    if let Some(grant) = kind.extra_grant() {
        require(access, grant)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportKind {
    Master,
    DailySales,
    ItemSales,
    CaptainPerformance,
    HourlySales,
    Credits,
    Debits,
    Refunds,
    TopUps,
    Deductions,
    WalletBalances,
    VendorPayable,
    ShopTransactions,
    AccountantCredits,
    ParameterSales,
    CancelledOrders,
    LaundryCharges,
    PaymentRequests,
    OnlinePayments,
    StudentSpending,
    StudentEodWallet,
}

impl ReportKind {
    pub(crate) const ALL: [Self; 21] = [
        Self::Master,
        Self::DailySales,
        Self::ItemSales,
        Self::CaptainPerformance,
        Self::HourlySales,
        Self::Credits,
        Self::Debits,
        Self::Refunds,
        Self::TopUps,
        Self::Deductions,
        Self::WalletBalances,
        Self::VendorPayable,
        Self::ShopTransactions,
        Self::AccountantCredits,
        Self::ParameterSales,
        Self::CancelledOrders,
        Self::LaundryCharges,
        Self::PaymentRequests,
        Self::OnlinePayments,
        Self::StudentSpending,
        Self::StudentEodWallet,
    ];

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let key = raw.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    fn key(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::DailySales => "daily_sales",
            Self::ItemSales => "item_sales",
            Self::CaptainPerformance => "captain_performance",
            Self::HourlySales => "hourly_sales",
            Self::Credits => "credits",
            Self::Debits => "debits",
            Self::Refunds => "refunds",
            Self::TopUps => "top_ups",
            Self::Deductions => "deductions",
            Self::WalletBalances => "wallet_balances",
            Self::VendorPayable => "vendor_payable",
            Self::ShopTransactions => "shop_transactions",
            Self::AccountantCredits => "accountant_credits",
            Self::ParameterSales => "parameter_sales",
            Self::CancelledOrders => "cancelled_orders",
            Self::LaundryCharges => "laundry_charges",
            Self::PaymentRequests => "payment_requests",
            Self::OnlinePayments => "online_payments",
            Self::StudentSpending => "student_spending",
            Self::StudentEodWallet => "student_eod_wallet",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Master => "Master Report",
            Self::DailySales => "Daily Sales Summary",
            Self::ItemSales => "Item-wise Sales",
            Self::CaptainPerformance => "Captain Performance",
            Self::HourlySales => "Hourly Sales",
            Self::Credits => "Credits",
            Self::Debits => "Debits",
            Self::Refunds => "Refunds",
            Self::TopUps => "Wallet Top-ups",
            Self::Deductions => "Accountant Deductions",
            Self::WalletBalances => "Wallet Balances",
            Self::VendorPayable => "Vendor Payable",
            Self::ShopTransactions => "Shop-wise Transactions",
            Self::AccountantCredits => "Accountant Credits",
            Self::ParameterSales => "Parameter Sales",
            Self::CancelledOrders => "Rejected & Cancelled Orders",
            Self::LaundryCharges => "Laundry Charges",
            Self::PaymentRequests => "Payment Requests",
            Self::OnlinePayments => "Online Payments",
            Self::StudentSpending => "Student Spending",
            Self::StudentEodWallet => "Student EOD Wallet",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Master => "Full operations: sales, vendors, ledger",
            Self::DailySales => "Completed sales, orders and cancellations by day and shop",
            Self::ItemSales => "Top items with quantity, revenue, cost and profit",
            Self::CaptainPerformance => {
                "Orders handled, revenue and handling time per staff member"
            }
            Self::HourlySales => "Sales by hour of the day and day of the week",
            Self::Credits => "Wallet top-ups and credit transactions",
            Self::Debits => "Purchases and wallet debit transactions",
            Self::Refunds => "Order refunds and reversal transactions",
            Self::TopUps => "Top-ups by source, payment method and day",
            Self::Deductions => "Manual wallet deductions with the reason and who made them",
            Self::WalletBalances => "Current balance of every wallet, overdrawn first",
            Self::VendorPayable => "Monthly vendor settlement statement",
            Self::ShopTransactions => "Monthly shop ledger with payable calculations",
            Self::AccountantCredits => "Credits manually added by the accountant",
            Self::ParameterSales => {
                "Sales and count for selected menu items in the selected period"
            }
            Self::CancelledOrders => "Rejected and cancelled orders and charges, with reasons",
            Self::LaundryCharges => "Laundry charges paid, pending and cancelled",
            Self::PaymentRequests => "Requests issued, paid, pending and overdue by purpose",
            Self::OnlinePayments => "Razorpay payments captured, settled and pending",
            Self::StudentSpending => "Wallet spending by department and batch",
            Self::StudentEodWallet => "Day-wise end-of-day wallet balance for every student",
        }
    }

    /// Settlements run by calendar month rather than a free range.
    fn is_monthly(self) -> bool {
        matches!(self, Self::VendorPayable | Self::ShopTransactions)
    }

    /// A point-in-time picture: no period, "as of" when it was generated.
    fn is_snapshot(self) -> bool {
        matches!(self, Self::WalletBalances)
    }

    /// Kinds about something other than a shop ignore the shop filter.
    fn uses_shop(self) -> bool {
        !matches!(self, Self::PaymentRequests | Self::OnlinePayments)
    }

    /// Fee records have their own read grant on top of the report grants.
    fn extra_grant(self) -> Option<&'static str> {
        match self {
            Self::PaymentRequests => Some("fees.payment_requests.read"),
            Self::OnlinePayments => Some("fees.online_payments.read"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DateRange {
    from: NaiveDate,
    to: NaiveDate,
}

impl DateRange {
    fn days(self) -> i64 {
        (self.to - self.from).num_days() + 1
    }

    fn label(self) -> String {
        if self.from == self.to {
            self.from.format("%-d %b %Y").to_string()
        } else {
            format!(
                "{} to {}",
                self.from.format("%-d %b %Y"),
                self.to.format("%-d %b %Y")
            )
        }
    }
}

fn parse_day(raw: Option<&str>, field: &str) -> ApiResult<Option<NaiveDate>> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| ApiError::BadRequest(format!("`{field}` must be a date like 2026-01-31"))),
    }
}

/// `2026-09` → 1 Sep 2026 to 30 Sep 2026.
fn parse_month(raw: &str) -> ApiResult<DateRange> {
    let invalid = || ApiError::BadRequest("`month` must look like 2026-09".into());
    let (year, month) = raw.trim().split_once('-').ok_or_else(invalid)?;
    let year: i32 = year.parse().map_err(|_| invalid())?;
    let month: u32 = month.parse().map_err(|_| invalid())?;
    let from = NaiveDate::from_ymd_opt(year, month, 1).ok_or_else(invalid)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .ok_or_else(invalid)?;
    Ok(DateRange {
        from,
        to: next.pred_opt().ok_or_else(invalid)?,
    })
}

/// The period a report covers. Monthly kinds take `month` (default: this
/// month); snapshots are today; every other kind takes `from`/`to`
/// (default: today).
fn resolve_range(kind: ReportKind, query: &ReportQuery, today: NaiveDate) -> ApiResult<DateRange> {
    if kind.is_snapshot() {
        return Ok(DateRange {
            from: today,
            to: today,
        });
    }
    if kind.is_monthly() {
        return match query
            .month
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            Some(month) => parse_month(month),
            None => parse_month(&format!("{}-{:02}", today.year(), today.month())),
        };
    }
    let (from, to) = match (
        parse_day(query.from.as_deref(), "from")?,
        parse_day(query.to.as_deref(), "to")?,
    ) {
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
    let range = DateRange { from, to };
    let limit = if kind == ReportKind::StudentEodWallet {
        MAX_EOD_DAYS
    } else {
        MAX_RANGE_DAYS
    };
    if range.days() > limit {
        return Err(ApiError::BadRequest(format!(
            "Choose a range of at most {limit} days for this report"
        )));
    }
    Ok(range)
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round_one(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// "manual_top_up" → "Manual top-up"; unknown kinds are shown as written.
fn transaction_type_label(raw: &str) -> String {
    match raw {
        "manual_top_up" => "Manual top-up".into(),
        "online_top_up" => "Online top-up".into(),
        "order_debit" => "Purchase".into(),
        "manual_debit" => "Deduction".into(),
        "refund" => "Refund".into(),
        other => words(other),
    }
}

/// "bank_transfer" → "Bank transfer".
fn words(raw: &str) -> String {
    let spaced = raw.trim().replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

// ---------------------------------------------------------------------------
// The report document
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Column {
    key: &'static str,
    label: &'static str,
    /// text | money | number | datetime | date
    format: &'static str,
}

const fn col(key: &'static str, label: &'static str, format: &'static str) -> Column {
    Column { key, label, format }
}

/// Row key the app reads to tint a row (e.g. an overdrawn wallet); it is not
/// a column, so the CSV never shows it.
const ROW_TONE: &str = "_tone";

#[derive(Debug, Clone)]
struct Table {
    title: String,
    columns: Vec<Column>,
    rows: Vec<Value>,
    totals: Option<Value>,
}

impl Table {
    fn new(title: impl Into<String>, columns: Vec<Column>) -> Self {
        Self {
            title: title.into(),
            columns,
            rows: Vec::new(),
            totals: None,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "columns": self.columns.iter().map(|c| json!({
                "key": c.key, "label": c.label, "format": c.format,
            })).collect::<Vec<_>>(),
            "rows": self.rows,
            "totals": self.totals,
        })
    }
}

#[derive(Debug, Clone, Default)]
struct Report {
    notes: Vec<String>,
    summary: Vec<Value>,
    tables: Vec<Table>,
}

fn stat(label: &str, value: f64, format: &str) -> Value {
    json!({ "label": label, "value": value, "format": format })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default, Debug, Clone)]
pub(crate) struct ReportQuery {
    from: Option<String>,
    to: Option<String>,
    month: Option<String>,
    shop: Option<String>,
    items: Option<String>,
}

async fn report_options(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_report_access(&access)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let pool = db.pool();
    let tenant = tenant_id(pool, &principal.student.tenant_id).await?;
    // The shop picker offers a shop's own staff their own shops only.
    let scope = crate::operations::caller_shop_scope(pool, tenant, &principal, &access).await?;
    let shops = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'shopKey', shop_key, 'name', name, 'category', lower(category),
             'isActive', is_active) ORDER BY sort_order NULLS LAST, name, shop_key), '[]'::jsonb)
           FROM campus_ops.shops WHERE tenant_id=$1
             AND ($2::text[] IS NULL OR shop_key = ANY($2))"#,
    )
    .bind(tenant)
    .bind(scope.as_deref())
    .fetch_one(pool)
    .await?;
    let items = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'id', item.id, 'name', item.name, 'category', item.category,
             'shopKey', item.store, 'shopName', COALESCE(shop.name, item.store),
             'price', item.price::float8) ORDER BY COALESCE(shop.name, item.store), item.name), '[]'::jsonb)
           FROM campus_ops.canteen_menu_items item
           LEFT JOIN campus_ops.shops shop ON shop.tenant_id=item.tenant_id AND shop.shop_key=item.store
           WHERE item.tenant_id=$1 AND ($2::text[] IS NULL OR item.store = ANY($2))"#,
    )
    .bind(tenant)
    .bind(scope.as_deref())
    .fetch_one(pool)
    .await?;
    let today = local_today(pool).await?;
    Ok(Json(ApiResponse::new(json!({
        "shops": shops,
        "menuItems": items,
        "today": today.format("%Y-%m-%d").to_string(),
        "timezone": TENANT_TIMEZONE,
        "maxRangeDays": MAX_RANGE_DAYS,
        "maxEodDays": MAX_EOD_DAYS,
        "kinds": ReportKind::ALL.iter()
            .filter(|kind| kind.extra_grant().is_none_or(|grant| access.allows(grant)))
            .map(|kind| kind.key())
            .collect::<Vec<_>>(),
    }))))
}

/// The shop a report covers for a caller limited to `scope` (their assigned
/// shops; `None` is the whole campus). A scoped caller must name one of their
/// shops, or has it chosen for them when they run exactly one; reports about
/// something other than a shop are not theirs to read.
fn scoped_report_shop<'a>(
    kind: ReportKind,
    scope: Option<&'a [String]>,
    requested: Option<&'a str>,
) -> ApiResult<Option<&'a str>> {
    let Some(keys) = scope else {
        return Ok(requested);
    };
    if !kind.uses_shop() {
        return Err(ApiError::ForbiddenWithMessage(
            "This report covers the whole campus. Ask the accounts office for it.".into(),
        ));
    }
    match requested {
        Some(key) => {
            crate::operations::require_in_scope(Some(keys), key)?;
            Ok(Some(key))
        }
        None => match keys {
            [only] => Ok(Some(only.as_str())),
            [] => Err(ApiError::ForbiddenWithMessage(
                crate::operations::NOT_ASSIGNED_TO_SHOP_MESSAGE.into(),
            )),
            _ => Err(ApiError::BadRequest(
                "Choose one of your shops for this report".into(),
            )),
        },
    }
}

async fn local_today(pool: &sqlx::PgPool) -> ApiResult<NaiveDate> {
    Ok(
        sqlx::query_scalar::<_, NaiveDate>("SELECT (now() AT TIME ZONE $1)::date")
            .bind(TENANT_TIMEZONE)
            .fetch_one(pool)
            .await?,
    )
}

async fn generate_report(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(kind): Path<String>,
    Query(query): Query<ReportQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let kind =
        ReportKind::parse(&kind).ok_or_else(|| ApiError::NotFound("Unknown report kind".into()))?;
    require_kind_access(&access, kind)?;
    let report = build_report(&state, &principal, &access, kind, &query).await?;
    Ok(Json(ApiResponse::new(report)))
}

/// The report document the app renders — also the source of an emailed
/// report's summary, so the email always states the server's own figures.
async fn build_report(
    state: &AppState,
    principal: &AuthPrincipal,
    access: &EffectiveAccess,
    kind: ReportKind,
    query: &ReportQuery,
) -> ApiResult<Value> {
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let pool = db.pool();
    let tenant = tenant_id(pool, &principal.student.tenant_id).await?;
    let today = local_today(pool).await?;
    let range = resolve_range(kind, query, today)?;

    let requested_shop = query
        .shop
        .as_deref()
        .map(str::trim)
        .filter(|s| kind.uses_shop() && !s.is_empty() && !s.eq_ignore_ascii_case("all"));
    // A shop's own staff report on their own shops only; with one shop that
    // is the default, never "every shop".
    let scope = crate::operations::caller_shop_scope(pool, tenant, principal, access).await?;
    let requested_shop = scoped_report_shop(kind, scope.as_deref(), requested_shop)?;
    let shop = match requested_shop {
        None => None,
        Some(key) => Some(
            sqlx::query_as::<_, (String, String)>(
                "SELECT shop_key, name FROM campus_ops.shops WHERE tenant_id=$1 AND shop_key=$2",
            )
            .bind(tenant)
            .bind(key)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| ApiError::NotFound("Shop not found".into()))?,
        ),
    };
    if kind == ReportKind::ShopTransactions && shop.is_none() {
        return Err(ApiError::BadRequest("Choose a shop for this report".into()));
    }
    let shop_key = shop.as_ref().map(|(key, _)| key.clone());
    let items: Vec<String> = query
        .items
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| Uuid::parse_str(id).is_ok())
        .map(str::to_lowercase)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let control = state.database().unwrap_or_else(|| db.clone());
    let ctx = Ctx {
        pool,
        control: control.pool(),
        tenant,
        range,
        today,
        shop_key: shop_key.as_deref(),
    };
    let report = match kind {
        ReportKind::Master => master_report(&ctx).await?,
        ReportKind::DailySales => daily_sales_report(&ctx).await?,
        ReportKind::ItemSales => item_sales_report(&ctx).await?,
        ReportKind::CaptainPerformance => captain_performance_report(&ctx).await?,
        ReportKind::HourlySales => hourly_sales_report(&ctx).await?,
        ReportKind::Credits => credits_report(&ctx).await?,
        ReportKind::Debits => debits_report(&ctx).await?,
        ReportKind::Refunds => refunds_report(&ctx).await?,
        ReportKind::TopUps => top_ups_report(&ctx).await?,
        ReportKind::Deductions => deductions_report(&ctx).await?,
        ReportKind::WalletBalances => wallet_balances_report(&ctx).await?,
        ReportKind::VendorPayable => vendor_payable_report(&ctx).await?,
        ReportKind::ShopTransactions => shop_transactions_report(&ctx).await?,
        ReportKind::AccountantCredits => accountant_credits_report(&ctx).await?,
        ReportKind::ParameterSales => parameter_sales_report(&ctx, &items).await?,
        ReportKind::CancelledOrders => cancelled_orders_report(&ctx).await?,
        ReportKind::LaundryCharges => laundry_charges_report(&ctx).await?,
        ReportKind::PaymentRequests => payment_requests_report(&ctx).await?,
        ReportKind::OnlinePayments => online_payments_report(&ctx).await?,
        ReportKind::StudentSpending => student_spending_report(&ctx).await?,
        ReportKind::StudentEodWallet => student_eod_report(&ctx).await?,
    };

    let (institution, generated_at, generated_label) =
        sqlx::query_as::<_, (Option<String>, String, String)>(
            r#"SELECT (SELECT name FROM platform.tenants WHERE id=$1),
                  to_char(now() AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS'),
                  to_char(now() AT TIME ZONE $2, 'FMDD Mon YYYY, FMHH12:MI AM')"#,
        )
        .bind(tenant)
        .bind(TENANT_TIMEZONE)
        .fetch_one(pool)
        .await?;
    let period_label = if kind.is_snapshot() {
        format!("As of {generated_label}")
    } else if kind.is_monthly() {
        range.from.format("%B %Y").to_string()
    } else {
        range.label()
    };
    Ok(json!({
        "kind": kind.key(),
        "title": kind.title(),
        "description": kind.description(),
        "from": range.from.format("%Y-%m-%d").to_string(),
        "to": range.to.format("%Y-%m-%d").to_string(),
        "periodLabel": period_label,
        "timezone": TENANT_TIMEZONE,
        "shop": shop.map(|(key, name)| json!({ "shopKey": key, "name": name })),
        "institutionName": institution,
        "generatedAt": generated_at,
        "available": true,
        "notes": report.notes,
        "summary": report.summary,
        "tables": report.tables.iter().map(Table::to_json).collect::<Vec<_>>(),
    }))
}

struct Ctx<'a> {
    pool: &'a sqlx::PgPool,
    control: &'a sqlx::PgPool,
    tenant: Uuid,
    range: DateRange,
    today: NaiveDate,
    shop_key: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// The wallet ledger
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct LedgerRow {
    id: Uuid,
    local_at: String,
    user_id: String,
    shop_name: String,
    amount: f64,
    transaction_type: String,
    description: String,
    actor_user_id: Option<String>,
    student_name: Option<String>,
    student_number: Option<String>,
    order_number: Option<i64>,
    rejection_reason: Option<String>,
}

type LedgerTuple = (
    Uuid,
    String,
    String,
    String,
    Option<String>,
    f64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

/// Ledger rows in the range (and shop), oldest first, with the order a
/// purchase or refund belongs to. `filter` is a fixed SQL predicate on `t`.
async fn ledger(ctx: &Ctx<'_>, filter: &str) -> ApiResult<(Vec<LedgerRow>, bool)> {
    let sql = format!(
        r#"SELECT t.id, to_char(t.created_at AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS'),
             t.user_id, t.shop_key,
             -- A purchase or refund names the counter it was at and, at a
             -- counter, which credit paid: the canteen's general credit or
             -- the counter-only credit.
             CASE WHEN t.transaction_type IN ('order_debit','refund') AND spent.name IS NOT NULL
                  THEN spent.name || CASE
                    WHEN spent.shop_key<>t.shop_key THEN ' (' || COALESCE(shop.name, t.shop_key) || ' credit)'
                    WHEN spent.parent_shop_key IS NOT NULL THEN ' (' || spent.name || '-only credit)'
                    ELSE '' END
                  ELSE {bucket_label} END,
             t.amount::float8, t.transaction_type,
             t.description, t.actor_user_id, student.full_name, student.student_number,
             o.order_number, o.rejection_reason
           FROM campus_ops.canteen_wallet_transactions t
           LEFT JOIN campus_ops.shops shop
             ON shop.tenant_id=t.tenant_id AND shop.shop_key=t.shop_key
           LEFT JOIN campus_ops.shops spent
             ON spent.tenant_id=t.tenant_id AND spent.shop_key=t.counter_shop_key
           LEFT JOIN core.students student
             ON student.tenant_id=t.tenant_id AND student.user_account_id::text=t.user_id
           LEFT JOIN campus_ops.canteen_orders o
             ON o.tenant_id=t.tenant_id AND o.id::text=t.reference_id
           WHERE t.tenant_id=$1
             AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
             AND ($5::text IS NULL OR {txn_shop}=$5)
             AND ({filter})
           ORDER BY t.created_at, t.id
           LIMIT $6"#,
        bucket_label = crate::operations::WALLET_BUCKET_LABEL_SQL,
        txn_shop = TXN_SHOP_SQL,
    );
    let rows = sqlx::query_as::<_, LedgerTuple>(&sql)
        .bind(ctx.tenant)
        .bind(TENANT_TIMEZONE)
        .bind(ctx.range.from)
        .bind(ctx.range.to)
        .bind(ctx.shop_key)
        .bind(MAX_LEDGER_ROWS as i64 + 1)
        .fetch_all(ctx.pool)
        .await?;
    let truncated = rows.len() > MAX_LEDGER_ROWS;
    Ok((
        rows.into_iter()
            .take(MAX_LEDGER_ROWS)
            .map(|r| LedgerRow {
                id: r.0,
                local_at: r.1,
                user_id: r.2.to_lowercase(),
                shop_name: r.4.unwrap_or(r.3),
                amount: r.5,
                transaction_type: r.6,
                description: r.7,
                actor_user_id: r.8.map(|id| id.to_lowercase()),
                student_name: r.9,
                student_number: r.10,
                order_number: r.11,
                rejection_reason: r.12,
            })
            .collect(),
        truncated,
    ))
}

/// Account names from the control plane, which holds every member (the tenant
/// copy of `identity.users` can lag behind it).
async fn account_names(
    ctx: &Ctx<'_>,
    ids: impl IntoIterator<Item = String>,
) -> ApiResult<HashMap<String, String>> {
    let ids: Vec<Uuid> = ids
        .into_iter()
        .filter_map(|id| Uuid::parse_str(&id).ok())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT id::text, display_name FROM identity.users WHERE id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(ctx.control)
    .await?;
    let mut names: HashMap<String, String> = rows
        .into_iter()
        .map(|(id, name)| (id.to_lowercase(), name))
        .collect();
    // Anyone the control plane does not know may still be in the tenant copy.
    let missing: Vec<Uuid> = ids
        .into_iter()
        .filter(|id| !names.contains_key(&id.to_string()))
        .collect();
    if !missing.is_empty() {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT id::text, display_name FROM identity.users WHERE id = ANY($1)",
        )
        .bind(&missing)
        .fetch_all(ctx.pool)
        .await?;
        names.extend(rows.into_iter().map(|(id, name)| (id.to_lowercase(), name)));
    }
    Ok(names)
}

async fn ledger_with_names(
    ctx: &Ctx<'_>,
    filter: &str,
) -> ApiResult<(Vec<LedgerRow>, HashMap<String, String>, bool)> {
    let (rows, truncated) = ledger(ctx, filter).await?;
    let names = account_names(
        ctx,
        rows.iter()
            .filter(|row| row.student_name.is_none())
            .map(|row| row.user_id.clone())
            .chain(rows.iter().filter_map(|row| row.actor_user_id.clone())),
    )
    .await?;
    Ok((rows, names, truncated))
}

fn member_name(row: &LedgerRow, names: &HashMap<String, String>) -> String {
    row.student_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| names.get(&row.user_id).cloned())
        .unwrap_or_else(|| "Unknown account".into())
}

fn actor_name(row: &LedgerRow, names: &HashMap<String, String>) -> String {
    row.actor_user_id
        .as_ref()
        .and_then(|id| names.get(id).cloned())
        .unwrap_or_else(|| "—".into())
}

fn short_id(id: Uuid) -> String {
    id.simple().to_string()[..8].to_uppercase()
}

fn truncation_note(truncated: bool) -> Option<String> {
    truncated.then(|| {
        format!("Only the first {MAX_LEDGER_ROWS} transactions are listed; choose a shorter period for the rest.")
    })
}

// ---------------------------------------------------------------------------
// Kinds
// ---------------------------------------------------------------------------

async fn credits_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let (rows, names, truncated) =
        ledger_with_names(ctx, "t.amount > 0 AND t.transaction_type <> 'refund'").await?;
    let mut table = Table::new(
        "Credit transactions",
        vec![
            col("date", "Date & time", "datetime"),
            col("reference", "Reference", "text"),
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("shop", "Wallet", "text"),
            col("type", "Type", "text"),
            col("addedBy", "Added by", "text"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut by_type: BTreeMap<String, (i64, f64)> = BTreeMap::new();
    let mut total = 0.0;
    for row in &rows {
        total += row.amount;
        let entry = by_type
            .entry(transaction_type_label(&row.transaction_type))
            .or_default();
        entry.0 += 1;
        entry.1 += row.amount;
        table.rows.push(json!({
            "date": row.local_at,
            "reference": short_id(row.id),
            "name": member_name(row, &names),
            "roll": row.student_number.clone().unwrap_or_default(),
            "shop": row.shop_name,
            "type": transaction_type_label(&row.transaction_type),
            "addedBy": actor_name(row, &names),
            "amount": round_money(row.amount),
        }));
    }
    table.totals = Some(json!({ "date": "Total", "amount": round_money(total) }));
    let mut summary = vec![
        stat("Credits", rows.len() as f64, "number"),
        stat("Total credited", round_money(total), "money"),
    ];
    for (label, (_, amount)) in &by_type {
        summary.push(stat(label, round_money(*amount), "money"));
    }
    Ok(Report {
        notes: truncation_note(truncated).into_iter().collect(),
        summary,
        tables: vec![table],
    })
}

async fn debits_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let (rows, names, truncated) = ledger_with_names(ctx, "t.amount < 0").await?;
    let mut table = Table::new(
        "Debit transactions",
        vec![
            col("date", "Date & time", "datetime"),
            col("reference", "Reference", "text"),
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("shop", "Shop", "text"),
            col("order", "Order no.", "text"),
            col("description", "Description", "text"),
            col("type", "Type", "text"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut total = 0.0;
    for row in &rows {
        let amount = round_money(-row.amount);
        total += amount;
        table.rows.push(json!({
            "date": row.local_at,
            "reference": short_id(row.id),
            "name": member_name(row, &names),
            "roll": row.student_number.clone().unwrap_or_default(),
            "shop": row.shop_name,
            "order": row.order_number.map(|n| format!("#{n}")).unwrap_or_default(),
            "description": row.description,
            "type": transaction_type_label(&row.transaction_type),
            "amount": amount,
        }));
    }
    table.totals = Some(json!({ "date": "Total", "amount": round_money(total) }));
    Ok(Report {
        notes: truncation_note(truncated).into_iter().collect(),
        summary: vec![
            stat("Debits", rows.len() as f64, "number"),
            stat("Total debited", round_money(total), "money"),
        ],
        tables: vec![table],
    })
}

async fn refunds_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let (rows, names, truncated) = ledger_with_names(ctx, "t.transaction_type = 'refund'").await?;
    let mut table = Table::new(
        "Refunds",
        vec![
            col("date", "Date & time", "datetime"),
            col("reference", "Reference", "text"),
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("shop", "Shop", "text"),
            col("order", "Order no.", "text"),
            col("reason", "Reason", "text"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut total = 0.0;
    for row in &rows {
        total += row.amount;
        table.rows.push(json!({
            "date": row.local_at,
            "reference": short_id(row.id),
            "name": member_name(row, &names),
            "roll": row.student_number.clone().unwrap_or_default(),
            "shop": row.shop_name,
            "order": row.order_number.map(|n| format!("#{n}")).unwrap_or_default(),
            "reason": row.rejection_reason.clone().filter(|r| !r.trim().is_empty())
                .unwrap_or_else(|| row.description.clone()),
            "amount": round_money(row.amount),
        }));
    }
    table.totals = Some(json!({ "date": "Total", "amount": round_money(total) }));
    Ok(Report {
        notes: truncation_note(truncated).into_iter().collect(),
        summary: vec![
            stat("Refunds", rows.len() as f64, "number"),
            stat("Total refunded", round_money(total), "money"),
        ],
        tables: vec![table],
    })
}

/// Purchases and refunds per shop in the range: what each vendor is owed.
#[derive(Debug, Clone, Default, PartialEq)]
struct Payable {
    shop_name: String,
    category: String,
    purchases: i64,
    gross: f64,
    refunds: i64,
    refunded: f64,
}

impl Payable {
    fn net(&self) -> f64 {
        round_money(self.gross - self.refunded)
    }
}

async fn payables(ctx: &Ctx<'_>) -> ApiResult<Vec<Payable>> {
    let rows = sqlx::query_as::<_, (String, String, String, i64, f64, i64, f64)>(
        r#"SELECT s.shop_key, s.name, lower(s.category),
             count(t.id) FILTER (WHERE t.transaction_type='order_debit')::int8,
             COALESCE(-sum(t.amount) FILTER (WHERE t.transaction_type='order_debit'), 0)::float8,
             count(t.id) FILTER (WHERE t.transaction_type='refund')::int8,
             COALESCE(sum(t.amount) FILTER (WHERE t.transaction_type='refund'), 0)::float8
           FROM campus_ops.shops s
           LEFT JOIN campus_ops.canteen_wallet_transactions t
             ON t.tenant_id=s.tenant_id
            AND (CASE WHEN t.transaction_type IN ('order_debit','refund')
                      THEN COALESCE(t.counter_shop_key, t.shop_key) ELSE t.shop_key END)=s.shop_key
            AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
           WHERE s.tenant_id=$1 AND ($5::text IS NULL OR s.shop_key=$5)
           GROUP BY s.shop_key, s.name, s.category, s.sort_order
           ORDER BY s.sort_order NULLS LAST, s.name, s.shop_key"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Payable {
            shop_name: r.1,
            category: r.2,
            purchases: r.3,
            gross: round_money(r.4),
            refunds: r.5,
            refunded: round_money(r.6),
        })
        .collect())
}

fn payable_table(title: &str, payables: &[Payable]) -> Table {
    let mut table = Table::new(
        title,
        vec![
            col("shop", "Shop", "text"),
            col("category", "Category", "text"),
            col("purchases", "Purchases", "number"),
            col("gross", "Gross sales", "money"),
            col("refunds", "Refunds", "number"),
            col("refunded", "Refunded", "money"),
            col("payable", "Net payable", "money"),
        ],
    );
    let (mut purchases, mut gross, mut refunds, mut refunded) = (0, 0.0, 0, 0.0);
    for p in payables {
        purchases += p.purchases;
        gross += p.gross;
        refunds += p.refunds;
        refunded += p.refunded;
        table.rows.push(json!({
            "shop": p.shop_name,
            "category": p.category,
            "purchases": p.purchases,
            "gross": p.gross,
            "refunds": p.refunds,
            "refunded": p.refunded,
            "payable": p.net(),
        }));
    }
    table.totals = Some(json!({
        "shop": "Total",
        "purchases": purchases,
        "gross": round_money(gross),
        "refunds": refunds,
        "refunded": round_money(refunded),
        "payable": round_money(gross - refunded),
    }));
    table
}

const PAYABLE_NOTE: &str = "Net payable is wallet purchases less refunds for the period. No commission \
     or margin is configured for campus shops, so none is deducted. Wallet top-ups are money held for \
     members, not vendor income, and are not included.";

async fn vendor_payable_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let payables = payables(ctx).await?;
    let gross: f64 = payables.iter().map(|p| p.gross).sum();
    let refunded: f64 = payables.iter().map(|p| p.refunded).sum();
    Ok(Report {
        notes: vec![PAYABLE_NOTE.into()],
        summary: vec![
            stat("Gross sales", round_money(gross), "money"),
            stat("Refunded", round_money(refunded), "money"),
            stat("Net payable", round_money(gross - refunded), "money"),
        ],
        tables: vec![payable_table("Settlement by shop", &payables)],
    })
}

async fn shop_transactions_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let (rows, names, truncated) = ledger_with_names(ctx, "true").await?;
    let mut table = Table::new(
        "Shop ledger",
        vec![
            col("date", "Date & time", "datetime"),
            col("reference", "Reference", "text"),
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("type", "Type", "text"),
            col("order", "Order no.", "text"),
            col("credit", "Wallet credit", "money"),
            col("sale", "Sale", "money"),
            col("refund", "Refund", "money"),
            col("payable", "Payable to date", "money"),
        ],
    );
    let (mut credits, mut sales, mut refunds, mut other_debits) = (0.0, 0.0, 0.0, 0.0);
    for row in &rows {
        let (mut credit, mut sale, mut refund) = (Value::Null, Value::Null, Value::Null);
        match row.transaction_type.as_str() {
            "order_debit" => {
                sales += -row.amount;
                sale = json!(round_money(-row.amount));
            }
            "refund" => {
                refunds += row.amount;
                refund = json!(round_money(row.amount));
            }
            _ if row.amount > 0.0 => {
                credits += row.amount;
                credit = json!(round_money(row.amount));
            }
            _ => other_debits += -row.amount,
        }
        table.rows.push(json!({
            "date": row.local_at,
            "reference": short_id(row.id),
            "name": member_name(row, &names),
            "roll": row.student_number.clone().unwrap_or_default(),
            "type": transaction_type_label(&row.transaction_type),
            "order": row.order_number.map(|n| format!("#{n}")).unwrap_or_default(),
            "credit": credit,
            "sale": sale,
            "refund": refund,
            "payable": round_money(sales - refunds),
        }));
    }
    table.totals = Some(json!({
        "date": "Total",
        "credit": round_money(credits),
        "sale": round_money(sales),
        "refund": round_money(refunds),
        "payable": round_money(sales - refunds),
    }));
    let mut notes = vec![PAYABLE_NOTE.to_owned()];
    if other_debits > 0.0 {
        notes.push(format!(
            "Other wallet debits of {:.2} (not purchases) are listed but do not change the payable.",
            round_money(other_debits)
        ));
    }
    notes.extend(truncation_note(truncated));
    Ok(Report {
        notes,
        summary: vec![
            stat("Transactions", rows.len() as f64, "number"),
            stat("Sales", round_money(sales), "money"),
            stat("Refunds", round_money(refunds), "money"),
            stat("Net payable", round_money(sales - refunds), "money"),
            stat("Wallet credits", round_money(credits), "money"),
        ],
        tables: vec![table],
    })
}

async fn accountant_credits_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let (rows, names, truncated) =
        ledger_with_names(ctx, "t.transaction_type = 'manual_top_up'").await?;
    let mut table = Table::new(
        "Manual credits",
        vec![
            col("date", "Date & time", "datetime"),
            col("reference", "Reference", "text"),
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("shop", "Wallet", "text"),
            col("addedBy", "Added by", "text"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut by_actor: BTreeMap<String, (i64, f64)> = BTreeMap::new();
    let mut total = 0.0;
    for row in &rows {
        total += row.amount;
        let actor = actor_name(row, &names);
        let entry = by_actor.entry(actor.clone()).or_default();
        entry.0 += 1;
        entry.1 += row.amount;
        table.rows.push(json!({
            "date": row.local_at,
            "reference": short_id(row.id),
            "name": member_name(row, &names),
            "roll": row.student_number.clone().unwrap_or_default(),
            "shop": row.shop_name,
            "addedBy": actor,
            "amount": round_money(row.amount),
        }));
    }
    table.totals = Some(json!({ "date": "Total", "amount": round_money(total) }));
    let mut staff = Table::new(
        "By staff member",
        vec![
            col("addedBy", "Added by", "text"),
            col("count", "Credits", "number"),
            col("amount", "Amount", "money"),
        ],
    );
    for (actor, (count, amount)) in &by_actor {
        staff.rows.push(json!({
            "addedBy": actor, "count": count, "amount": round_money(*amount),
        }));
    }
    staff.totals = Some(json!({
        "addedBy": "Total", "count": rows.len(), "amount": round_money(total),
    }));
    Ok(Report {
        notes: std::iter::once(
            "Manual top-ups recorded at the recharge desk, with the staff account that added each one."
                .to_owned(),
        )
        .chain(truncation_note(truncated))
        .collect(),
        summary: vec![
            stat("Manual credits", rows.len() as f64, "number"),
            stat("Total credited", round_money(total), "money"),
        ],
        tables: vec![table, staff],
    })
}

async fn parameter_sales_report(ctx: &Ctx<'_>, items: &[String]) -> ApiResult<Report> {
    let sql = format!(
        r#"{SALES_CTE}
        SELECT COALESCE(line->>'itemId', ''), COALESCE(line->>'name', 'Item'), x.shop_key,
          COALESCE(s.name, x.shop_key),
          sum(COALESCE((line->>'quantity')::numeric, 0))::float8,
          count(DISTINCT x.id)::int8,
          round(sum(COALESCE((line->>'price')::numeric, 0)
                    * COALESCE((line->>'quantity')::numeric, 0)), 2)::float8
        FROM sales x
        CROSS JOIN LATERAL jsonb_array_elements(x.lines) line
        LEFT JOIN shop_list s ON s.shop_key=x.shop_key
        WHERE x.kind='order' AND x.bucket='completed'
          AND x.local_at::date BETWEEN $3 AND $4
          AND ($5::text IS NULL OR x.shop_key=$5)
          AND (cardinality($6::text[])=0 OR lower(line->>'itemId') = ANY($6))
        GROUP BY 1, 2, 3, 4
        ORDER BY 5 DESC, 2"#
    );
    let rows = sqlx::query_as::<_, (String, String, String, String, f64, i64, f64)>(&sql)
        .bind(ctx.tenant)
        .bind(TENANT_TIMEZONE)
        .bind(ctx.range.from)
        .bind(ctx.range.to)
        .bind(ctx.shop_key)
        .bind(items)
        .fetch_all(ctx.pool)
        .await?;
    let mut table = Table::new(
        "Item sales",
        vec![
            col("item", "Item", "text"),
            col("shop", "Shop", "text"),
            col("quantity", "Quantity sold", "number"),
            col("orders", "Orders", "number"),
            col("amount", "Sales", "money"),
        ],
    );
    let mut seen = HashSet::new();
    let (mut quantity, mut amount) = (0.0, 0.0);
    for r in &rows {
        seen.insert(r.0.to_lowercase());
        quantity += r.4;
        amount += r.6;
        table.rows.push(json!({
            "item": r.1, "shop": r.3, "quantity": r.4, "orders": r.5, "amount": round_money(r.6),
        }));
    }
    // A chosen item that sold nothing is listed at zero rather than dropped.
    let unsold: Vec<Uuid> = items
        .iter()
        .filter(|id| !seen.contains(*id))
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect();
    if !unsold.is_empty() {
        let rows = sqlx::query_as::<_, (String, String)>(
            r#"SELECT item.name, COALESCE(shop.name, item.store)
               FROM campus_ops.canteen_menu_items item
               LEFT JOIN campus_ops.shops shop
                 ON shop.tenant_id=item.tenant_id AND shop.shop_key=item.store
               WHERE item.tenant_id=$1 AND item.id = ANY($2)
                 AND ($3::text IS NULL OR item.store=$3)
               ORDER BY item.name"#,
        )
        .bind(ctx.tenant)
        .bind(&unsold)
        .bind(ctx.shop_key)
        .fetch_all(ctx.pool)
        .await?;
        for (name, shop) in rows {
            table.rows.push(json!({
                "item": name, "shop": shop, "quantity": 0, "orders": 0, "amount": 0.0,
            }));
        }
    }
    let orders: i64 = rows.iter().map(|r| r.5).sum();
    table.totals = Some(json!({
        "item": "Total",
        "quantity": quantity,
        "amount": round_money(amount),
    }));
    let scope = if items.is_empty() {
        "All menu items are included."
    } else {
        "Only the selected menu items are included."
    };
    Ok(Report {
        notes: vec![format!(
            "{scope} Completed orders only; pending, cancelled and rejected orders are not sales."
        )],
        summary: vec![
            stat("Items", table.rows.len() as f64, "number"),
            stat("Quantity sold", quantity, "number"),
            stat("Order lines", orders as f64, "number"),
            stat("Sales", round_money(amount), "money"),
        ],
        tables: vec![table],
    })
}

async fn student_eod_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let today = ctx.today;
    let mut notes = Vec::new();
    let mut range = ctx.range;
    if range.from > today {
        return Err(ApiError::BadRequest(
            "End-of-day balances exist only up to today".into(),
        ));
    }
    if range.to > today {
        range.to = today;
        notes.push("Days after today have no end-of-day balance yet and are left out.".to_owned());
    }
    if range.to == today {
        notes.push("Today's figure is the balance so far; the day has not ended.".to_owned());
    }
    notes.push(
        "Each balance is the student's wallet total at 11:59 PM, worked back from today's balance \
         through every later ledger entry."
            .to_owned(),
    );
    let rows = sqlx::query_as::<_, (NaiveDate, Option<String>, String, f64)>(concat!(
        r#"WITH days AS (
             SELECT generate_series($3::date, $4::date, interval '1 day')::date AS day
           ),
           students AS (
             SELECT lower(s.user_account_id::text) AS user_id, s.full_name, s.student_number
             FROM core.students s WHERE s.tenant_id=$1
           ),
           balances AS (
             SELECT lower(w.user_id) AS user_id, sum(w.balance)::float8 AS balance
             FROM campus_ops.canteen_wallets w
             WHERE w.tenant_id=$1 AND "#, bucket_of_shop_sql!("w.shop_key"), r#"
             GROUP BY lower(w.user_id)
           )
           SELECT d.day, st.student_number, st.full_name,
             round((COALESCE(b.balance, 0) - COALESCE((
               SELECT sum(t.amount)::float8 FROM campus_ops.canteen_wallet_transactions t
               WHERE t.tenant_id=$1 AND lower(t.user_id)=st.user_id
                 AND "#, bucket_of_shop_sql!("t.shop_key"), r#"
                 AND t.created_at >= ((d.day + 1)::timestamp AT TIME ZONE $2)
             ), 0))::numeric, 2)::float8
           FROM days d CROSS JOIN students st
           LEFT JOIN balances b ON b.user_id=st.user_id
           ORDER BY d.day, st.student_number NULLS LAST, st.full_name"#
    ))
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(range.from)
    .bind(range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let mut table = Table::new(
        "End-of-day balances",
        vec![
            col("date", "Date", "date"),
            col("roll", "Roll no.", "text"),
            col("name", "Name", "text"),
            col("balance", "EOD balance", "money"),
        ],
    );
    let mut last_day_total = 0.0;
    let mut students = HashSet::new();
    for (day, roll, name, balance) in &rows {
        if *day == range.to {
            last_day_total += balance;
        }
        students.insert((roll.clone(), name.clone()));
        table.rows.push(json!({
            "date": day.format("%Y-%m-%d").to_string(),
            "roll": roll.clone().unwrap_or_default(),
            "name": name,
            "balance": balance,
        }));
    }
    Ok(Report {
        notes,
        summary: vec![
            stat("Students", students.len() as f64, "number"),
            stat("Days", range.days() as f64, "number"),
            stat("Total on last day", round_money(last_day_total), "money"),
        ],
        tables: vec![table],
    })
}

async fn master_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    // Sales by shop, counted exactly as the campus sales dashboard counts them.
    let sql = format!(
        r#"{SALES_CTE}
        SELECT x.shop_key, COALESCE(s.name, x.shop_key),
          count(*) FILTER (WHERE x.bucket='completed')::int8,
          count(*) FILTER (WHERE x.bucket='active')::int8,
          count(*) FILTER (WHERE x.bucket='cancelled')::int8,
          COALESCE(sum(x.total) FILTER (WHERE x.bucket='completed'), 0)::float8
        FROM sales x
        LEFT JOIN shop_list s ON s.shop_key=x.shop_key
        WHERE x.local_at::date BETWEEN $3 AND $4
          AND ($5::text IS NULL OR x.shop_key=$5)
        GROUP BY 1, 2
        ORDER BY 6 DESC, 2"#
    );
    let sales = sqlx::query_as::<_, (String, String, i64, i64, i64, f64)>(&sql)
        .bind(ctx.tenant)
        .bind(TENANT_TIMEZONE)
        .bind(ctx.range.from)
        .bind(ctx.range.to)
        .bind(ctx.shop_key)
        .fetch_all(ctx.pool)
        .await?;
    let mut sales_table = Table::new(
        "Sales by shop",
        vec![
            col("shop", "Shop", "text"),
            col("completed", "Completed", "number"),
            col("active", "In progress", "number"),
            col("cancelled", "Cancelled", "number"),
            col("revenue", "Revenue", "money"),
        ],
    );
    let (mut completed, mut active, mut cancelled, mut revenue) = (0, 0, 0, 0.0);
    for (_, name, done, open, dropped, amount) in &sales {
        completed += done;
        active += open;
        cancelled += dropped;
        revenue += amount;
        sales_table.rows.push(json!({
            "shop": name, "completed": done, "active": open, "cancelled": dropped,
            "revenue": round_money(*amount),
        }));
    }
    sales_table.totals = Some(json!({
        "shop": "Total", "completed": completed, "active": active,
        "cancelled": cancelled, "revenue": round_money(revenue),
    }));

    let payables = payables(ctx).await?;
    let payable_table = payable_table("Vendor payable", &payables);

    let ledger = sqlx::query_as::<_, (String, i64, f64)>(&format!(
        r#"SELECT t.transaction_type, count(*)::int8, sum(t.amount)::float8
           FROM campus_ops.canteen_wallet_transactions t
           WHERE t.tenant_id=$1 AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
             AND ($5::text IS NULL OR {TXN_SHOP_SQL}=$5)
           GROUP BY 1 ORDER BY 1"#
    ))
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let mut ledger_table = Table::new(
        "Wallet ledger",
        vec![
            col("type", "Transaction type", "text"),
            col("count", "Transactions", "number"),
            col("credit", "Credited", "money"),
            col("debit", "Debited", "money"),
        ],
    );
    let (mut credited, mut debited, mut refunded, mut count) = (0.0, 0.0, 0.0, 0);
    for (kind, n, amount) in &ledger {
        count += n;
        let (credit, debit) = if *amount >= 0.0 {
            (round_money(*amount), 0.0)
        } else {
            (0.0, round_money(-amount))
        };
        if kind == "refund" {
            refunded += credit;
        } else {
            credited += credit;
        }
        debited += debit;
        ledger_table.rows.push(json!({
            "type": transaction_type_label(kind), "count": n, "credit": credit, "debit": debit,
        }));
    }
    ledger_table.totals = Some(json!({
        "type": "Total", "count": count,
        "credit": round_money(credited + refunded), "debit": round_money(debited),
    }));
    let net_payable: f64 = payables.iter().map(Payable::net).sum();
    Ok(Report {
        notes: vec![
            "Revenue is completed orders and paid laundry charges; cancelled and rejected orders are not sales."
                .into(),
            PAYABLE_NOTE.into(),
        ],
        summary: vec![
            stat("Revenue", round_money(revenue), "money"),
            stat("Completed orders", completed as f64, "number"),
            stat("Wallet credits", round_money(credited), "money"),
            stat("Wallet debits", round_money(debited), "money"),
            stat("Refunds", round_money(refunded), "money"),
            stat("Net vendor payable", round_money(net_payable), "money"),
        ],
        tables: vec![sales_table, payable_table, ledger_table],
    })
}

// ---------------------------------------------------------------------------
// Sales kinds (orders and laundry, counted as the sales dashboard counts them)
// ---------------------------------------------------------------------------

const COMPLETED_ONLY_NOTE: &str = "Sales are completed orders and paid laundry charges; \
     pending, cancelled and rejected orders are not sales.";

async fn daily_sales_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let sql = format!(
        r#"{SALES_CTE}
        SELECT x.local_at::date, COALESCE(s.name, x.shop_key),
          count(*) FILTER (WHERE x.bucket='completed')::int8,
          COALESCE(sum(x.total) FILTER (WHERE x.bucket='completed'), 0)::float8,
          count(*) FILTER (WHERE x.bucket='active')::int8,
          count(*) FILTER (WHERE x.bucket='cancelled')::int8,
          COALESCE(sum(x.total) FILTER (WHERE x.bucket='cancelled'), 0)::float8
        FROM sales x
        LEFT JOIN shop_list s ON s.shop_key=x.shop_key
        WHERE x.local_at::date BETWEEN $3 AND $4
          AND ($5::text IS NULL OR x.shop_key=$5)
        GROUP BY 1, 2
        ORDER BY 1, 2"#
    );
    let rows = sqlx::query_as::<_, (NaiveDate, String, i64, f64, i64, i64, f64)>(&sql)
        .bind(ctx.tenant)
        .bind(TENANT_TIMEZONE)
        .bind(ctx.range.from)
        .bind(ctx.range.to)
        .bind(ctx.shop_key)
        .fetch_all(ctx.pool)
        .await?;
    let mut by_shop = Table::new(
        "Sales by day and shop",
        vec![
            col("date", "Date", "date"),
            col("shop", "Shop", "text"),
            col("completed", "Completed", "number"),
            col("revenue", "Revenue", "money"),
            col("average", "Avg. order", "money"),
            col("active", "In progress", "number"),
            col("cancelled", "Cancelled / rejected", "number"),
            col("cancelledValue", "Cancelled value", "money"),
        ],
    );
    let mut days: BTreeMap<NaiveDate, (i64, f64, i64, i64)> = BTreeMap::new();
    let (mut completed, mut revenue, mut active, mut cancelled, mut cancelled_value) =
        (0, 0.0, 0, 0, 0.0);
    for (day, shop, done, amount, open, dropped, dropped_value) in &rows {
        completed += done;
        revenue += amount;
        active += open;
        cancelled += dropped;
        cancelled_value += dropped_value;
        let entry = days.entry(*day).or_default();
        entry.0 += done;
        entry.1 += amount;
        entry.2 += open;
        entry.3 += dropped;
        by_shop.rows.push(json!({
            "date": day.format("%Y-%m-%d").to_string(),
            "shop": shop,
            "completed": done,
            "revenue": round_money(*amount),
            "average": if *done > 0 { round_money(amount / *done as f64) } else { 0.0 },
            "active": open,
            "cancelled": dropped,
            "cancelledValue": round_money(*dropped_value),
        }));
    }
    by_shop.totals = Some(json!({
        "date": "Total",
        "completed": completed,
        "revenue": round_money(revenue),
        "average": if completed > 0 { round_money(revenue / completed as f64) } else { 0.0 },
        "active": active,
        "cancelled": cancelled,
        "cancelledValue": round_money(cancelled_value),
    }));
    let mut by_day = Table::new(
        "Daily totals",
        vec![
            col("date", "Date", "date"),
            col("completed", "Completed", "number"),
            col("revenue", "Revenue", "money"),
            col("active", "In progress", "number"),
            col("cancelled", "Cancelled / rejected", "number"),
        ],
    );
    let mut best: Option<(NaiveDate, f64)> = None;
    for (day, (done, amount, open, dropped)) in &days {
        if best.is_none_or(|(_, top)| *amount > top) {
            best = Some((*day, *amount));
        }
        by_day.rows.push(json!({
            "date": day.format("%Y-%m-%d").to_string(),
            "completed": done,
            "revenue": round_money(*amount),
            "active": open,
            "cancelled": dropped,
        }));
    }
    by_day.totals = Some(json!({
        "date": "Total", "completed": completed, "revenue": round_money(revenue),
        "active": active, "cancelled": cancelled,
    }));
    let mut notes = vec![COMPLETED_ONLY_NOTE.to_owned()];
    if let Some((day, amount)) = best.filter(|(_, amount)| *amount > 0.0) {
        notes.push(format!(
            "Best day: {} with {} in sales.",
            day.format("%-d %b %Y"),
            format_inr(amount)
        ));
    }
    let mut tables = vec![by_shop];
    // One shop's day totals repeat the first table line for line.
    if ctx.shop_key.is_none() {
        tables.push(by_day);
    }
    Ok(Report {
        notes,
        summary: vec![
            stat("Revenue", round_money(revenue), "money"),
            stat("Completed orders", completed as f64, "number"),
            stat(
                "Average per day",
                round_money(revenue / ctx.range.days() as f64),
                "money",
            ),
            stat("Cancelled / rejected", cancelled as f64, "number"),
        ],
        tables,
    })
}

async fn item_sales_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let sql = format!(
        r#"{SALES_CTE},
        lines AS (
          SELECT x.shop_key, x.id, line,
            COALESCE(NULLIF(line->>'quantity','')::float8, 1) AS qty,
            COALESCE(NULLIF(line->>'price','')::float8, 0) AS price
          FROM sales x
          CROSS JOIN LATERAL jsonb_array_elements(
            CASE WHEN jsonb_typeof(x.lines)='array' THEN x.lines ELSE '[]'::jsonb END) line
          WHERE x.bucket='completed'
            AND x.local_at::date BETWEEN $3 AND $4
            AND ($5::text IS NULL OR x.shop_key=$5)
        )
        SELECT COALESCE(l.line->>'name', 'Item'), COALESCE(s.name, l.shop_key),
          sum(l.qty)::float8, count(DISTINCT l.id)::int8,
          sum(l.qty * l.price)::float8,
          sum(l.qty * COALESCE(mi.actual_price::float8, mi.price::float8, l.price))::float8
        FROM lines l
        LEFT JOIN shop_list s ON s.shop_key=l.shop_key
        LEFT JOIN campus_ops.canteen_menu_items mi
          ON mi.tenant_id=$1 AND mi.id::text = l.line->>'itemId'
        GROUP BY 1, 2
        ORDER BY 5 DESC, 3 DESC, 1"#
    );
    let rows = sqlx::query_as::<_, (String, String, f64, i64, f64, f64)>(&sql)
        .bind(ctx.tenant)
        .bind(TENANT_TIMEZONE)
        .bind(ctx.range.from)
        .bind(ctx.range.to)
        .bind(ctx.shop_key)
        .fetch_all(ctx.pool)
        .await?;
    let mut table = Table::new(
        "Items by revenue",
        vec![
            col("rank", "#", "number"),
            col("item", "Item", "text"),
            col("shop", "Shop", "text"),
            col("quantity", "Qty sold", "number"),
            col("orders", "Orders", "number"),
            col("revenue", "Revenue", "money"),
            col("cost", "Cost", "money"),
            col("profit", "Profit", "money"),
            col("margin", "Margin %", "number"),
        ],
    );
    let (mut quantity, mut revenue, mut cost) = (0.0, 0.0, 0.0);
    for (index, (item, shop, qty, orders, amount, spent)) in rows.iter().enumerate() {
        quantity += qty;
        revenue += amount;
        cost += spent;
        table.rows.push(json!({
            "rank": index + 1,
            "item": item,
            "shop": shop,
            "quantity": round_money(*qty),
            "orders": orders,
            "revenue": round_money(*amount),
            "cost": round_money(*spent),
            "profit": round_money(amount - spent),
            "margin": margin(*amount, *spent),
        }));
    }
    table.totals = Some(json!({
        "item": "Total",
        "quantity": round_money(quantity),
        "revenue": round_money(revenue),
        "cost": round_money(cost),
        "profit": round_money(revenue - cost),
        "margin": margin(revenue, cost),
    }));
    Ok(Report {
        notes: vec![
            COMPLETED_ONLY_NOTE.to_owned(),
            "Cost is each item's configured cost price (which defaults to its selling price); \
             an item without one, or a laundry charge, shows no profit rather than a guessed one."
                .to_owned(),
        ],
        summary: vec![
            stat("Items sold", table.rows.len() as f64, "number"),
            stat("Quantity", round_money(quantity), "number"),
            stat("Revenue", round_money(revenue), "money"),
            stat("Cost", round_money(cost), "money"),
            stat("Profit", round_money(revenue - cost), "money"),
        ],
        tables: vec![table],
    })
}

/// Profit as a share of revenue, one decimal; zero without revenue.
fn margin(revenue: f64, cost: f64) -> f64 {
    if revenue > 0.0 {
        round_one((revenue - cost) / revenue * 100.0)
    } else {
        0.0
    }
}

/// Orders by the account that last moved them — the captain or owner at the
/// counter — exactly as the shop's own "Sales & Profit" tab attributes them.
async fn captain_performance_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    type Row = (
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        i64,
        f64,
        Option<f64>,
    );
    let rows = sqlx::query_as::<_, Row>(
        r#"WITH scoped AS (
          SELECT o.status, NULLIF(trim(o.handled_by), '') AS handled_by,
            o.total::float8 AS total, o.created_at, o.updated_at,
            COALESCE(exact.shop_key, fallback.shop_key, o.store) AS shop_key
          FROM campus_ops.canteen_orders o
          LEFT JOIN campus_ops.shops exact ON exact.tenant_id=o.tenant_id AND exact.shop_key=o.store
          LEFT JOIN LATERAL (
            SELECT s.shop_key FROM campus_ops.shops s
            WHERE exact.shop_key IS NULL AND s.tenant_id=o.tenant_id AND lower(s.category)=CASE
              WHEN lower(o.store) LIKE '%laundry%' THEN 'laundry'
              WHEN lower(o.store) LIKE '%station%' THEN 'stationery'
              ELSE 'canteen' END
            ORDER BY s.is_active DESC, s.created_at, s.shop_key LIMIT 1
          ) fallback ON true
          WHERE o.tenant_id=$1 AND (o.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
        )
        SELECT COALESCE(s.name, x.shop_key), x.handled_by,
          (SELECT a.assignment_role FROM campus_ops.shop_user_assignments a
            WHERE a.tenant_id=$1 AND a.shop_id=s.id AND a.user_id=x.handled_by AND a.is_active
            LIMIT 1),
          count(*)::int8,
          count(*) FILTER (WHERE x.status='completed')::int8,
          count(*) FILTER (WHERE x.status IN ('pending','accepted','preparing','ready'))::int8,
          count(*) FILTER (WHERE x.status='rejected')::int8,
          count(*) FILTER (WHERE x.status='cancelled')::int8,
          COALESCE(sum(x.total) FILTER (WHERE x.status='completed'), 0)::float8,
          (avg(extract(epoch FROM (x.updated_at - x.created_at)) / 60.0)
            FILTER (WHERE x.status='completed'))::float8
        FROM scoped x
        LEFT JOIN campus_ops.shops s ON s.tenant_id=$1 AND s.shop_key=x.shop_key
        WHERE ($5::text IS NULL OR x.shop_key=$5)
        GROUP BY s.id, 1, 2
        ORDER BY 1, (x.handled_by IS NULL), 9 DESC, 4 DESC"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let names = account_names(ctx, rows.iter().filter_map(|r| r.1.clone())).await?;
    let mut table = Table::new(
        "Orders by staff member",
        vec![
            col("shop", "Shop", "text"),
            col("name", "Handled by", "text"),
            col("role", "Role", "text"),
            col("orders", "Orders", "number"),
            col("completed", "Completed", "number"),
            col("active", "In progress", "number"),
            col("rejected", "Rejected", "number"),
            col("cancelled", "Cancelled", "number"),
            col("revenue", "Revenue", "money"),
            col("minutes", "Avg. minutes", "number"),
        ],
    );
    let (mut orders, mut completed, mut revenue) = (0, 0, 0.0);
    let (mut minutes_sum, mut minutes_orders) = (0.0, 0);
    let mut unattributed = 0;
    for (shop, handler, role, n, done, open, rejected, cancelled, amount, minutes) in &rows {
        orders += n;
        completed += done;
        revenue += amount;
        if let Some(minutes) = minutes {
            minutes_sum += minutes * *done as f64;
            minutes_orders += done;
        }
        let name = match handler {
            None => {
                unattributed += n;
                "Not yet handled".to_owned()
            }
            Some(id) => names
                .get(&id.to_lowercase())
                .cloned()
                .unwrap_or_else(|| "Former staff".into()),
        };
        table.rows.push(json!({
            "shop": shop,
            "name": name,
            "role": role.as_deref().map(words).unwrap_or_default(),
            "orders": n,
            "completed": done,
            "active": open,
            "rejected": rejected,
            "cancelled": cancelled,
            "revenue": round_money(*amount),
            "minutes": minutes.map(round_one),
        }));
    }
    let average = (minutes_orders > 0).then(|| round_one(minutes_sum / minutes_orders as f64));
    table.totals = Some(json!({
        "shop": "Total",
        "orders": orders,
        "completed": completed,
        "revenue": round_money(revenue),
        "minutes": average,
    }));
    let mut notes = vec![
        "Each order is credited to the account that last moved it (accepted, prepared, \
         handed over or rejected it). Avg. minutes is order placed to completed."
            .to_owned(),
        "Laundry charges are raised at the counter, not ordered, so they are not listed here."
            .to_owned(),
    ];
    if unattributed > 0 {
        notes.push(format!(
            "{unattributed} order(s) had not been picked up by anyone yet."
        ));
    }
    Ok(Report {
        notes,
        summary: vec![
            stat("Orders", orders as f64, "number"),
            stat("Completed", completed as f64, "number"),
            stat("Revenue", round_money(revenue), "money"),
            stat("Avg. minutes", average.unwrap_or(0.0), "number"),
        ],
        tables: vec![table],
    })
}

const WEEKDAYS: [(&str, &str); 7] = [
    ("mon", "Mon"),
    ("tue", "Tue"),
    ("wed", "Wed"),
    ("thu", "Thu"),
    ("fri", "Fri"),
    ("sat", "Sat"),
    ("sun", "Sun"),
];

/// "13" → "1 PM – 2 PM".
fn hour_label(hour: u32) -> String {
    let twelve = |h: u32| match h % 24 {
        0 => "12 AM".to_owned(),
        12 => "12 PM".to_owned(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    };
    format!("{} – {}", twelve(hour), twelve(hour + 1))
}

/// Revenue by hour × weekday: one row per hour from the first to the last
/// hour that sold anything, one column per weekday.
fn hourly_table(title: &str, cells: &HashMap<(u32, u32), (i64, f64)>, orders: bool) -> Table {
    let mut columns = vec![col("hour", "Hour", "text")];
    let format = if orders { "number" } else { "money" };
    for (key, label) in WEEKDAYS {
        columns.push(col(key, label, format));
    }
    columns.push(col("total", "Total", format));
    let mut table = Table::new(title, columns);
    let hours: Vec<u32> = cells.keys().map(|(hour, _)| *hour).collect();
    let (Some(first), Some(last)) = (hours.iter().min(), hours.iter().max()) else {
        return table;
    };
    let value = |cell: Option<&(i64, f64)>| -> f64 {
        cell.map_or(0.0, |(n, amount)| {
            if orders { *n as f64 } else { round_money(*amount) }
        })
    };
    let mut day_totals = [0.0; 7];
    for hour in *first..=*last {
        let mut row = serde_json::Map::new();
        row.insert("hour".into(), json!(hour_label(hour)));
        let mut total = 0.0;
        for (index, (key, _)) in WEEKDAYS.iter().enumerate() {
            let v = value(cells.get(&(hour, index as u32 + 1)));
            total += v;
            day_totals[index] += v;
            row.insert((*key).into(), json!(v));
        }
        row.insert("total".into(), json!(round_money(total)));
        table.rows.push(Value::Object(row));
    }
    let mut totals = serde_json::Map::new();
    totals.insert("hour".into(), json!("Total"));
    for (index, (key, _)) in WEEKDAYS.iter().enumerate() {
        totals.insert((*key).into(), json!(round_money(day_totals[index])));
    }
    totals.insert(
        "total".into(),
        json!(round_money(day_totals.iter().sum::<f64>())),
    );
    table.totals = Some(Value::Object(totals));
    table
}

async fn hourly_sales_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let sql = format!(
        r#"{SALES_CTE}
        SELECT extract(hour FROM x.local_at)::int4, extract(isodow FROM x.local_at)::int4,
          count(*)::int8, COALESCE(sum(x.total), 0)::float8
        FROM sales x
        WHERE x.bucket='completed'
          AND x.local_at::date BETWEEN $3 AND $4
          AND ($5::text IS NULL OR x.shop_key=$5)
        GROUP BY 1, 2"#
    );
    let rows = sqlx::query_as::<_, (i32, i32, i64, f64)>(&sql)
        .bind(ctx.tenant)
        .bind(TENANT_TIMEZONE)
        .bind(ctx.range.from)
        .bind(ctx.range.to)
        .bind(ctx.shop_key)
        .fetch_all(ctx.pool)
        .await?;
    let cells: HashMap<(u32, u32), (i64, f64)> = rows
        .iter()
        .map(|(hour, day, n, amount)| ((*hour as u32, *day as u32), (*n, *amount)))
        .collect();
    let mut by_hour: BTreeMap<u32, (i64, f64)> = BTreeMap::new();
    let mut by_day: BTreeMap<u32, (i64, f64)> = BTreeMap::new();
    for ((hour, day), (n, amount)) in &cells {
        let h = by_hour.entry(*hour).or_default();
        h.0 += n;
        h.1 += amount;
        let d = by_day.entry(*day).or_default();
        d.0 += n;
        d.1 += amount;
    }
    let orders: i64 = by_hour.values().map(|(n, _)| n).sum();
    let revenue: f64 = by_hour.values().map(|(_, amount)| amount).sum();
    let mut notes = vec![
        COMPLETED_ONLY_NOTE.to_owned(),
        "Hours are when the order was placed, in campus time.".to_owned(),
    ];
    if let Some((hour, (n, amount))) = by_hour
        .iter()
        .max_by(|a, b| a.1.1.total_cmp(&b.1.1).then(b.0.cmp(a.0)))
    {
        notes.push(format!(
            "Busiest hour: {} ({} across {n} order(s)).",
            hour_label(*hour),
            format_inr(*amount)
        ));
    }
    if let Some((day, (_, amount))) = by_day
        .iter()
        .max_by(|a, b| a.1.1.total_cmp(&b.1.1).then(b.0.cmp(a.0)))
    {
        let name = WEEKDAYS
            .get((*day as usize).saturating_sub(1))
            .map_or("", |(_, label)| label);
        notes.push(format!("Busiest weekday: {name} ({}).", format_inr(*amount)));
    }
    Ok(Report {
        notes,
        summary: vec![
            stat("Revenue", round_money(revenue), "money"),
            stat("Orders", orders as f64, "number"),
            stat("Active hours", by_hour.len() as f64, "number"),
        ],
        tables: vec![
            hourly_table("Revenue by hour and weekday", &cells, false),
            hourly_table("Orders by hour and weekday", &cells, true),
        ],
    })
}

async fn cancelled_orders_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let sql = format!(
        r#"{SALES_CTE}
        SELECT to_char(x.local_at, 'YYYY-MM-DD"T"HH24:MI:SS'), x.kind, x.number,
          COALESCE(x.customer_name, ''), COALESCE(s.name, x.shop_key), x.status,
          COALESCE(x.rejection_reason, ''), x.total,
          COALESCE((SELECT string_agg(
              trim(to_char(COALESCE(NULLIF(l->>'quantity','')::numeric, 1), 'FM999999990.##'))
              || ' × ' || COALESCE(l->>'name', 'Item'), ', ')
            FROM jsonb_array_elements(
              CASE WHEN jsonb_typeof(x.lines)='array' THEN x.lines ELSE '[]'::jsonb END) l), '')
        FROM sales x
        LEFT JOIN shop_list s ON s.shop_key=x.shop_key
        WHERE x.bucket='cancelled'
          AND x.local_at::date BETWEEN $3 AND $4
          AND ($5::text IS NULL OR x.shop_key=$5)
        ORDER BY x.local_at"#
    );
    let rows = sqlx::query_as::<
        _,
        (String, String, Option<i64>, String, String, String, String, f64, String),
    >(&sql)
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let mut table = Table::new(
        "Rejected and cancelled",
        vec![
            col("date", "Placed", "datetime"),
            col("order", "Order no.", "text"),
            col("customer", "Customer", "text"),
            col("shop", "Shop", "text"),
            col("items", "Items", "text"),
            col("status", "Status", "text"),
            col("reason", "Reason", "text"),
            col("amount", "Amount", "money"),
        ],
    );
    let (mut rejected, mut cancelled, mut value) = (0, 0, 0.0);
    let mut by_reason: BTreeMap<String, (i64, f64)> = BTreeMap::new();
    for (date, kind, number, customer, shop, status, reason, amount, items) in &rows {
        if status == "rejected" {
            rejected += 1;
        } else {
            cancelled += 1;
        }
        value += amount;
        let reason = if reason.trim().is_empty() {
            "No reason given".to_owned()
        } else {
            reason.trim().to_owned()
        };
        let entry = by_reason.entry(reason.clone()).or_default();
        entry.0 += 1;
        entry.1 += amount;
        table.rows.push(json!({
            "date": date,
            "order": match (kind.as_str(), number) {
                ("laundry", _) => "Laundry charge".to_owned(),
                (_, Some(n)) => format!("#{n}"),
                _ => String::new(),
            },
            "customer": customer,
            "shop": shop,
            "items": items,
            "status": words(status),
            "reason": reason,
            "amount": round_money(*amount),
        }));
    }
    table.totals = Some(json!({ "date": "Total", "amount": round_money(value) }));
    let mut reasons = Table::new(
        "By reason",
        vec![
            col("reason", "Reason", "text"),
            col("count", "Orders", "number"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut sorted: Vec<_> = by_reason.into_iter().collect();
    sorted.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    for (reason, (count, amount)) in sorted {
        reasons.rows.push(json!({
            "reason": reason, "count": count, "amount": round_money(amount),
        }));
    }
    reasons.totals = Some(json!({
        "reason": "Total", "count": rows.len(), "amount": round_money(value),
    }));
    Ok(Report {
        notes: vec![
            "A rejected order's wallet payment is refunded; a cancelled order or laundry \
             charge was never charged. Neither counts as a sale."
                .to_owned(),
        ],
        summary: vec![
            stat("Rejected", rejected as f64, "number"),
            stat("Cancelled", cancelled as f64, "number"),
            stat("Value", round_money(value), "money"),
        ],
        tables: vec![table, reasons],
    })
}

// ---------------------------------------------------------------------------
// Wallet kinds
// ---------------------------------------------------------------------------

async fn top_ups_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let rows = sqlx::query_as::<_, (NaiveDate, String, Option<String>, String, i64, f64)>(&format!(
        r#"SELECT (t.created_at AT TIME ZONE $2)::date, t.transaction_type, r.payment_method,
             COALESCE({bucket_label}, t.shop_key), count(*)::int8, sum(t.amount)::float8
           FROM campus_ops.canteen_wallet_transactions t
           LEFT JOIN campus_ops.razorpay_orders r
             ON r.tenant_id=t.tenant_id AND r.order_id=t.reference_id
           LEFT JOIN campus_ops.shops shop
             ON shop.tenant_id=t.tenant_id AND shop.shop_key=t.shop_key
           WHERE t.tenant_id=$1
             AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
             AND ($5::text IS NULL OR t.shop_key=$5)
             AND t.transaction_type IN ('manual_top_up', 'online_top_up')
           GROUP BY 1, 2, 3, 4
           ORDER BY 1"#,
        bucket_label = crate::operations::WALLET_BUCKET_LABEL_SQL,
    ))
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let source = |kind: &str| {
        if kind == "online_top_up" {
            "Online (Razorpay)"
        } else {
            "Recharge desk"
        }
    };
    let method = |kind: &str, method: &Option<String>| match (kind, method.as_deref()) {
        ("online_top_up", Some(m)) if !m.trim().is_empty() => match m {
            "upi" => "UPI".to_owned(),
            "netbanking" => "Net banking".to_owned(),
            other => words(other),
        },
        ("online_top_up", _) => "Not recorded".to_owned(),
        _ => "Added by staff".to_owned(),
    };
    let mut grouped: BTreeMap<(String, String, String), (i64, f64)> = BTreeMap::new();
    let mut days: BTreeMap<NaiveDate, (f64, f64)> = BTreeMap::new();
    let (mut desk, mut online, mut count) = (0.0, 0.0, 0);
    for (day, kind, pay_method, shop, n, amount) in &rows {
        count += n;
        let entry = grouped
            .entry((
                source(kind).to_owned(),
                method(kind, pay_method),
                shop.clone(),
            ))
            .or_default();
        entry.0 += n;
        entry.1 += amount;
        let d = days.entry(*day).or_default();
        if kind == "online_top_up" {
            online += amount;
            d.1 += amount;
        } else {
            desk += amount;
            d.0 += amount;
        }
    }
    let total = desk + online;
    let mut by_source = Table::new(
        "By source and method",
        vec![
            col("source", "Source", "text"),
            col("method", "Method", "text"),
            col("shop", "Wallet", "text"),
            col("count", "Top-ups", "number"),
            col("amount", "Amount", "money"),
            col("share", "Share %", "number"),
        ],
    );
    for ((src, how, shop), (n, amount)) in &grouped {
        by_source.rows.push(json!({
            "source": src, "method": how, "shop": shop, "count": n,
            "amount": round_money(*amount),
            "share": if total > 0.0 { round_one(amount / total * 100.0) } else { 0.0 },
        }));
    }
    by_source.totals = Some(json!({
        "source": "Total", "count": count, "amount": round_money(total),
        "share": if total > 0.0 { 100.0 } else { 0.0 },
    }));
    let mut by_day = Table::new(
        "By day",
        vec![
            col("date", "Date", "date"),
            col("desk", "Recharge desk", "money"),
            col("online", "Online", "money"),
            col("total", "Total", "money"),
        ],
    );
    for (day, (d, o)) in &days {
        by_day.rows.push(json!({
            "date": day.format("%Y-%m-%d").to_string(),
            "desk": round_money(*d), "online": round_money(*o), "total": round_money(d + o),
        }));
    }
    by_day.totals = Some(json!({
        "date": "Total", "desk": round_money(desk), "online": round_money(online),
        "total": round_money(total),
    }));
    Ok(Report {
        notes: vec![
            "Recharge-desk top-ups are entered by staff, who collect the money outside the app; \
             online top-ups are paid through Razorpay, with the method Razorpay reported."
                .to_owned(),
        ],
        summary: vec![
            stat("Top-ups", count as f64, "number"),
            stat("Total", round_money(total), "money"),
            stat("Recharge desk", round_money(desk), "money"),
            stat("Online", round_money(online), "money"),
        ],
        tables: vec![by_source, by_day],
    })
}

async fn deductions_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let (rows, names, truncated) =
        ledger_with_names(ctx, "t.transaction_type = 'manual_debit'").await?;
    let mut table = Table::new(
        "Deductions",
        vec![
            col("date", "Date & time", "datetime"),
            col("reference", "Reference", "text"),
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("shop", "Wallet", "text"),
            col("reason", "Reason", "text"),
            col("deductedBy", "Deducted by", "text"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut by_actor: BTreeMap<String, (i64, f64)> = BTreeMap::new();
    let mut total = 0.0;
    for row in &rows {
        let amount = round_money(-row.amount);
        total += amount;
        let actor = actor_name(row, &names);
        let entry = by_actor.entry(actor.clone()).or_default();
        entry.0 += 1;
        entry.1 += amount;
        table.rows.push(json!({
            "date": row.local_at,
            "reference": short_id(row.id),
            "name": member_name(row, &names),
            "roll": row.student_number.clone().unwrap_or_default(),
            "shop": row.shop_name,
            "reason": row.description,
            "deductedBy": actor,
            "amount": amount,
        }));
    }
    table.totals = Some(json!({ "date": "Total", "amount": round_money(total) }));
    let mut staff = Table::new(
        "By staff member",
        vec![
            col("deductedBy", "Deducted by", "text"),
            col("count", "Deductions", "number"),
            col("amount", "Amount", "money"),
        ],
    );
    for (actor, (count, amount)) in &by_actor {
        staff.rows.push(json!({
            "deductedBy": actor, "count": count, "amount": round_money(*amount),
        }));
    }
    staff.totals = Some(json!({
        "deductedBy": "Total", "count": rows.len(), "amount": round_money(total),
    }));
    Ok(Report {
        notes: std::iter::once(
            "Manual deductions taken from a wallet by finance staff (fines, damages and \
             corrections), with the reason entered at the time."
                .to_owned(),
        )
        .chain(truncation_note(truncated))
        .collect(),
        summary: vec![
            stat("Deductions", rows.len() as f64, "number"),
            stat("Total deducted", round_money(total), "money"),
        ],
        tables: vec![table, staff],
    })
}

async fn wallet_balances_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let rows = sqlx::query_as::<
        _,
        (String, String, f64, String, Option<String>, Option<String>),
    >(
        r#"SELECT lower(w.user_id), COALESCE(shop.name, w.shop_key), w.balance::float8,
             to_char(w.updated_at AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS'),
             student.full_name, student.student_number
           FROM campus_ops.canteen_wallets w
           LEFT JOIN campus_ops.shops shop
             ON shop.tenant_id=w.tenant_id AND shop.shop_key=w.shop_key
           LEFT JOIN core.students student
             ON student.tenant_id=w.tenant_id AND lower(student.user_account_id::text)=lower(w.user_id)
           WHERE w.tenant_id=$1 AND ($3::text IS NULL OR w.shop_key=$3)
           ORDER BY w.balance, student.student_number NULLS LAST, 2"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let names = account_names(
        ctx,
        rows.iter()
            .filter(|r| r.4.is_none())
            .map(|r| r.0.clone()),
    )
    .await?;
    let mut table = Table::new(
        "Wallets",
        vec![
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("shop", "Wallet", "text"),
            col("status", "Status", "text"),
            col("updated", "Last activity", "datetime"),
            col("balance", "Balance", "money"),
        ],
    );
    let mut by_shop: BTreeMap<String, (i64, i64, f64, f64)> = BTreeMap::new();
    let (mut held, mut owed, mut overdrawn) = (0.0, 0.0, 0);
    for (user_id, shop, balance, updated, name, roll) in &rows {
        let balance = round_money(*balance);
        let entry = by_shop.entry(shop.clone()).or_default();
        entry.0 += 1;
        let status = if balance < 0.0 {
            overdrawn += 1;
            owed += -balance;
            entry.1 += 1;
            entry.3 += -balance;
            "Overdrawn"
        } else {
            held += balance;
            entry.2 += balance;
            if balance == 0.0 { "Empty" } else { "In credit" }
        };
        let mut row = json!({
            "name": name.clone().filter(|n| !n.trim().is_empty())
                .or_else(|| names.get(user_id).cloned())
                .unwrap_or_else(|| "Unknown account".into()),
            "roll": roll.clone().unwrap_or_default(),
            "shop": shop,
            "status": status,
            "updated": updated,
            "balance": balance,
        });
        if balance < 0.0 {
            row[ROW_TONE] = json!("negative");
        }
        table.rows.push(row);
    }
    table.totals = Some(json!({
        "name": "Net total", "balance": round_money(held - owed),
    }));
    let mut shops = Table::new(
        "By wallet",
        vec![
            col("shop", "Wallet", "text"),
            col("wallets", "Wallets", "number"),
            col("overdrawn", "Overdrawn", "number"),
            col("held", "Held in credit", "money"),
            col("owed", "Owed (overdrawn)", "money"),
            col("net", "Net", "money"),
        ],
    );
    for (shop, (count, negative, credit, debt)) in &by_shop {
        shops.rows.push(json!({
            "shop": shop, "wallets": count, "overdrawn": negative,
            "held": round_money(*credit), "owed": round_money(*debt),
            "net": round_money(credit - debt),
        }));
    }
    shops.totals = Some(json!({
        "shop": "Total", "wallets": rows.len(), "overdrawn": overdrawn,
        "held": round_money(held), "owed": round_money(owed),
        "net": round_money(held - owed),
    }));
    Ok(Report {
        notes: vec![
            "Balances as they stand now, overdrawn wallets first. An overdrawn wallet is money \
             the member owes after a deduction or charge exceeded their balance."
                .to_owned(),
        ],
        summary: vec![
            stat("Wallets", rows.len() as f64, "number"),
            stat("Held in credit", round_money(held), "money"),
            stat("Overdrawn", overdrawn as f64, "number"),
            stat("Owed", round_money(owed), "money"),
        ],
        tables: vec![shops, table],
    })
}

async fn student_spending_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    let groups = sqlx::query_as::<_, (String, String, i64, i64, i64, f64, f64)>(
        r#"WITH spend AS (
             SELECT lower(t.user_id) AS user_id,
               count(*) FILTER (WHERE t.transaction_type='order_debit') AS purchases,
               COALESCE(-sum(t.amount) FILTER (WHERE t.transaction_type='order_debit'), 0) AS spent,
               COALESCE(sum(t.amount) FILTER (WHERE t.transaction_type='refund'), 0) AS refunded
             FROM campus_ops.canteen_wallet_transactions t
             WHERE t.tenant_id=$1
               AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
               AND ($5::text IS NULL OR t.shop_key=$5)
             GROUP BY 1
           )
           SELECT COALESCE(d.name, 'No department'),
             COALESCE(b.name, NULLIF(st.academic_year, ''), 'No batch'),
             count(*)::int8,
             count(*) FILTER (WHERE sp.purchases > 0)::int8,
             COALESCE(sum(sp.purchases), 0)::int8,
             COALESCE(sum(sp.spent), 0)::float8,
             COALESCE(sum(sp.refunded), 0)::float8
           FROM core.students st
           LEFT JOIN core.departments d ON d.tenant_id=st.tenant_id AND d.id::text=st.department_id
           LEFT JOIN core.batches b ON b.tenant_id=st.tenant_id AND b.id::text=st.batch_id
           LEFT JOIN spend sp ON sp.user_id=lower(st.user_account_id::text)
           WHERE st.tenant_id=$1
           GROUP BY 1, 2
           ORDER BY 6 DESC, 1, 2"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let mut table = Table::new(
        "By department and batch",
        vec![
            col("department", "Department", "text"),
            col("batch", "Batch / year", "text"),
            col("students", "Students", "number"),
            col("spenders", "Who spent", "number"),
            col("purchases", "Purchases", "number"),
            col("spent", "Spent", "money"),
            col("refunded", "Refunded", "money"),
            col("net", "Net spent", "money"),
            col("average", "Avg. per spender", "money"),
        ],
    );
    let (mut students, mut spenders, mut purchases, mut spent, mut refunded) = (0, 0, 0, 0.0, 0.0);
    for (department, batch, count, active, n, amount, back) in &groups {
        students += count;
        spenders += active;
        purchases += n;
        spent += amount;
        refunded += back;
        let net = amount - back;
        table.rows.push(json!({
            "department": department, "batch": batch, "students": count,
            "spenders": active, "purchases": n,
            "spent": round_money(*amount), "refunded": round_money(*back),
            "net": round_money(net),
            "average": if *active > 0 { round_money(net / *active as f64) } else { 0.0 },
        }));
    }
    let net = spent - refunded;
    table.totals = Some(json!({
        "department": "Total", "students": students, "spenders": spenders,
        "purchases": purchases, "spent": round_money(spent),
        "refunded": round_money(refunded), "net": round_money(net),
        "average": if spenders > 0 { round_money(net / spenders as f64) } else { 0.0 },
    }));

    let top = sqlx::query_as::<_, (String, Option<String>, String, String, i64, f64)>(
        r#"SELECT st.full_name, st.student_number, COALESCE(d.name, 'No department'),
             COALESCE(b.name, NULLIF(st.academic_year, ''), 'No batch'),
             count(*) FILTER (WHERE t.transaction_type='order_debit')::int8,
             (COALESCE(-sum(t.amount) FILTER (WHERE t.transaction_type='order_debit'), 0)
              - COALESCE(sum(t.amount) FILTER (WHERE t.transaction_type='refund'), 0))::float8
           FROM campus_ops.canteen_wallet_transactions t
           JOIN core.students st
             ON st.tenant_id=t.tenant_id AND lower(st.user_account_id::text)=lower(t.user_id)
           LEFT JOIN core.departments d ON d.tenant_id=st.tenant_id AND d.id::text=st.department_id
           LEFT JOIN core.batches b ON b.tenant_id=st.tenant_id AND b.id::text=st.batch_id
           WHERE t.tenant_id=$1
             AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
             AND ($5::text IS NULL OR t.shop_key=$5)
             AND t.transaction_type IN ('order_debit', 'refund')
           GROUP BY st.id, 1, 2, 3, 4
           HAVING count(*) FILTER (WHERE t.transaction_type='order_debit') > 0
           ORDER BY 6 DESC, 2
           LIMIT $6"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .bind(TOP_SPENDERS)
    .fetch_all(ctx.pool)
    .await?;
    let mut spenders_table = Table::new(
        format!("Top {TOP_SPENDERS} spenders"),
        vec![
            col("rank", "#", "number"),
            col("name", "Name", "text"),
            col("roll", "Roll no.", "text"),
            col("department", "Department", "text"),
            col("batch", "Batch / year", "text"),
            col("purchases", "Purchases", "number"),
            col("net", "Net spent", "money"),
        ],
    );
    for (index, (name, roll, department, batch, n, amount)) in top.iter().enumerate() {
        spenders_table.rows.push(json!({
            "rank": index + 1, "name": name, "roll": roll.clone().unwrap_or_default(),
            "department": department, "batch": batch, "purchases": n,
            "net": round_money(*amount),
        }));
    }
    Ok(Report {
        notes: vec![
            "Spending is wallet purchases (orders and laundry) less refunds, by each student's \
             department and batch on the roster. Staff wallets are not included."
                .to_owned(),
        ],
        summary: vec![
            stat("Net spent", round_money(net), "money"),
            stat("Students who spent", spenders as f64, "number"),
            stat(
                "Avg. per spender",
                if spenders > 0 {
                    round_money(net / spenders as f64)
                } else {
                    0.0
                },
                "money",
            ),
            stat("Purchases", purchases as f64, "number"),
        ],
        tables: vec![table, spenders_table],
    })
}

// ---------------------------------------------------------------------------
// Laundry and fee kinds
// ---------------------------------------------------------------------------

async fn laundry_charges_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    type Row = (
        String,
        String,
        String,
        String,
        f64,
        String,
        f64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT to_char(c.created_at AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS'),
             COALESCE(shop.name, c.shop_key), c.name, c.service_type, c.quantity::float8,
             c.unit_label, c.total::float8, c.status, lower(c.claimed_by),
             student.full_name, student.student_number,
             to_char(c.paid_at AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS')
           FROM campus_ops.laundry_charges c
           LEFT JOIN campus_ops.shops shop
             ON shop.tenant_id=c.tenant_id AND shop.shop_key=c.shop_key
           LEFT JOIN core.students student
             ON student.tenant_id=c.tenant_id AND lower(student.user_account_id::text)=lower(c.claimed_by)
           WHERE c.tenant_id=$1
             AND (c.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
             AND ($5::text IS NULL OR c.shop_key=$5)
           ORDER BY c.created_at"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.shop_key)
    .fetch_all(ctx.pool)
    .await?;
    let names = account_names(
        ctx,
        rows.iter()
            .filter(|r| r.9.is_none())
            .filter_map(|r| r.8.clone()),
    )
    .await?;
    let mut table = Table::new(
        "Charges",
        vec![
            col("date", "Raised", "datetime"),
            col("shop", "Shop", "text"),
            col("item", "Item", "text"),
            col("service", "Service", "text"),
            col("quantity", "Qty", "text"),
            col("customer", "Paid by", "text"),
            col("roll", "Roll no.", "text"),
            col("status", "Status", "text"),
            col("paidAt", "Paid", "datetime"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut by_status: BTreeMap<String, (i64, f64)> = BTreeMap::new();
    for (date, shop, item, service, qty, unit, amount, status, claimer, name, roll, paid_at) in
        &rows
    {
        let entry = by_status.entry(status.clone()).or_default();
        entry.0 += 1;
        entry.1 += amount;
        let customer = name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| claimer.as_ref().and_then(|id| names.get(id).cloned()))
            .unwrap_or_else(|| {
                if claimer.is_some() {
                    "Unknown account".into()
                } else {
                    "Not yet claimed".into()
                }
            });
        let quantity = if qty.fract() == 0.0 {
            format!("{} {unit}", *qty as i64)
        } else {
            format!("{qty} {unit}")
        };
        table.rows.push(json!({
            "date": date,
            "shop": shop,
            "item": item,
            "service": words(service),
            "quantity": quantity.trim(),
            "customer": customer,
            "roll": roll.clone().unwrap_or_default(),
            "status": words(status),
            "paidAt": paid_at,
            "amount": round_money(*amount),
        }));
    }
    let tally = |key: &str| by_status.get(key).copied().unwrap_or_default();
    let (paid, pending, cancelled) = (tally("paid"), tally("pending"), tally("cancelled"));
    table.totals = Some(json!({
        "date": "Paid total", "amount": round_money(paid.1),
    }));
    let mut statuses = Table::new(
        "By status",
        vec![
            col("status", "Status", "text"),
            col("count", "Charges", "number"),
            col("amount", "Amount", "money"),
        ],
    );
    for (status, (count, amount)) in &by_status {
        statuses.rows.push(json!({
            "status": words(status), "count": count, "amount": round_money(*amount),
        }));
    }
    Ok(Report {
        notes: vec![
            "A charge is raised at the laundry counter and paid when the student scans it; \
             pending charges are unpaid, cancelled ones were withdrawn and never charged."
                .to_owned(),
        ],
        summary: vec![
            stat("Paid", round_money(paid.1), "money"),
            stat("Paid charges", paid.0 as f64, "number"),
            stat("Pending", round_money(pending.1), "money"),
            stat("Pending charges", pending.0 as f64, "number"),
            stat("Cancelled charges", cancelled.0 as f64, "number"),
        ],
        tables: vec![statuses, table],
    })
}

fn purpose_label(key: &str) -> String {
    crate::payment_requests::PURPOSES
        .iter()
        .find(|(k, _)| *k == key)
        .map_or_else(|| words(key), |(_, label)| (*label).to_owned())
}

async fn payment_requests_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    type Row = (
        String,
        String,
        String,
        f64,
        Option<NaiveDate>,
        String,
        i64,
        i64,
        f64,
        i64,
        f64,
        i64,
        f64,
        i64,
    );
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT to_char(r.created_at AT TIME ZONE $2, 'YYYY-MM-DD'), r.purpose, r.title,
             r.amount::float8, r.due_date, r.status,
             count(p.id)::int8,
             count(p.id) FILTER (WHERE p.status='paid')::int8,
             COALESCE(sum(p.amount) FILTER (WHERE p.status='paid'), 0)::float8,
             count(p.id) FILTER (WHERE p.status='pending' AND r.status='active')::int8,
             COALESCE(sum(p.amount) FILTER (WHERE p.status='pending' AND r.status='active'), 0)::float8,
             count(p.id) FILTER (WHERE p.status='pending' AND r.status='active'
                                  AND r.due_date < $5)::int8,
             COALESCE(sum(p.amount) FILTER (WHERE p.status='pending' AND r.status='active'
                                  AND r.due_date < $5), 0)::float8,
             count(p.id) FILTER (WHERE p.status='cancelled' OR r.status='cancelled')::int8
           FROM campus_ops.payment_requests r
           LEFT JOIN campus_ops.payment_request_payers p
             ON p.tenant_id=r.tenant_id AND p.request_id=r.id
           WHERE r.tenant_id=$1
             AND (r.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
           GROUP BY r.id
           ORDER BY r.created_at"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .bind(ctx.today)
    .fetch_all(ctx.pool)
    .await?;
    let mut requests = Table::new(
        "Requests",
        vec![
            col("created", "Issued", "date"),
            col("purpose", "Purpose", "text"),
            col("title", "Title", "text"),
            col("amount", "Amount each", "money"),
            col("due", "Due", "date"),
            col("status", "Status", "text"),
            col("payers", "Payers", "number"),
            col("paid", "Paid", "number"),
            col("collected", "Collected", "money"),
            col("pending", "Pending", "number"),
            col("outstanding", "Outstanding", "money"),
            col("overdue", "Overdue", "number"),
        ],
    );
    let mut by_purpose: BTreeMap<String, [f64; 7]> = BTreeMap::new();
    let mut totals = [0.0; 7];
    for (created, purpose, title, amount, due, status, payers, paid, collected, pending, owed, overdue, overdue_amount, cancelled) in
        &rows
    {
        let figures = [
            1.0,
            *paid as f64,
            *collected,
            *pending as f64,
            *owed,
            *overdue as f64,
            *overdue_amount,
        ];
        let entry = by_purpose.entry(purpose_label(purpose)).or_insert([0.0; 7]);
        for (index, value) in figures.iter().enumerate() {
            entry[index] += value;
            totals[index] += value;
        }
        let _ = cancelled;
        let mut row = json!({
            "created": created,
            "purpose": purpose_label(purpose),
            "title": title,
            "amount": round_money(*amount),
            "due": due.map(|d| d.format("%Y-%m-%d").to_string()),
            "status": words(status),
            "payers": payers,
            "paid": paid,
            "collected": round_money(*collected),
            "pending": pending,
            "outstanding": round_money(*owed),
            "overdue": overdue,
        });
        if *overdue > 0 {
            row[ROW_TONE] = json!("negative");
        }
        requests.rows.push(row);
    }
    requests.totals = Some(json!({
        "created": "Total", "paid": totals[1] as i64, "collected": round_money(totals[2]),
        "pending": totals[3] as i64, "outstanding": round_money(totals[4]),
        "overdue": totals[5] as i64,
    }));
    let mut purposes = Table::new(
        "By purpose",
        vec![
            col("purpose", "Purpose", "text"),
            col("requests", "Requests", "number"),
            col("paid", "Paid", "number"),
            col("collected", "Collected", "money"),
            col("pending", "Pending", "number"),
            col("outstanding", "Outstanding", "money"),
            col("overdue", "Overdue", "number"),
            col("overdueAmount", "Overdue amount", "money"),
        ],
    );
    for (purpose, f) in &by_purpose {
        purposes.rows.push(json!({
            "purpose": purpose, "requests": f[0] as i64, "paid": f[1] as i64,
            "collected": round_money(f[2]), "pending": f[3] as i64,
            "outstanding": round_money(f[4]), "overdue": f[5] as i64,
            "overdueAmount": round_money(f[6]),
        }));
    }
    purposes.totals = Some(json!({
        "purpose": "Total", "requests": totals[0] as i64, "paid": totals[1] as i64,
        "collected": round_money(totals[2]), "pending": totals[3] as i64,
        "outstanding": round_money(totals[4]), "overdue": totals[5] as i64,
        "overdueAmount": round_money(totals[6]),
    }));
    Ok(Report {
        notes: vec![
            "Requests issued in the period, with where every payer stands today. Pending \
             counts only requests still active; overdue is pending past its due date."
                .to_owned(),
        ],
        summary: vec![
            stat("Requests", totals[0], "number"),
            stat("Collected", round_money(totals[2]), "money"),
            stat("Outstanding", round_money(totals[4]), "money"),
            stat("Overdue payers", totals[5], "number"),
            stat("Overdue", round_money(totals[6]), "money"),
        ],
        tables: vec![purposes, requests],
    })
}

/// Razorpay order status → how the office reads it.
fn razorpay_state(status: &str, captured: bool, settled: bool) -> &'static str {
    match status {
        _ if settled => "Settled",
        "captured" | "paid" => "Captured",
        _ if captured => "Captured",
        "failed" => "Failed",
        "refunded" => "Refunded",
        _ => "Pending",
    }
}

async fn online_payments_report(ctx: &Ctx<'_>) -> ApiResult<Report> {
    type Row = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
        i64,
        Option<i64>,
        Option<i64>,
        bool,
        bool,
        Option<String>,
    );
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT to_char(created_at AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS'),
             order_id, payment_id, user_name, user_number, purpose, status, payment_method,
             amount_paise, fee_paise, tax_paise,
             captured_at IS NOT NULL, COALESCE(settled, false),
             to_char(settled_at AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS')
           FROM campus_ops.razorpay_orders
           WHERE tenant_id=$1
             AND (created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
           ORDER BY created_at"#,
    )
    .bind(ctx.tenant)
    .bind(TENANT_TIMEZONE)
    .bind(ctx.range.from)
    .bind(ctx.range.to)
    .fetch_all(ctx.pool)
    .await?;
    let rupees = |paise: i64| round_money(paise as f64 / 100.0);
    let mut table = Table::new(
        "Payments",
        vec![
            col("date", "Created", "datetime"),
            col("payer", "Payer", "text"),
            col("roll", "Roll no.", "text"),
            col("purpose", "Purpose", "text"),
            col("method", "Method", "text"),
            col("state", "Status", "text"),
            col("payment", "Payment ID", "text"),
            col("settledAt", "Settled", "datetime"),
            col("fee", "Fee + tax", "money"),
            col("amount", "Amount", "money"),
        ],
    );
    let mut by_state: BTreeMap<&'static str, (i64, f64, f64)> = BTreeMap::new();
    for (date, order, payment, name, number, purpose, status, method, amount, fee, tax, captured, settled, settled_at) in
        &rows
    {
        let state = razorpay_state(status, *captured, *settled);
        let fees = rupees(fee.unwrap_or(0) + tax.unwrap_or(0));
        let entry = by_state.entry(state).or_default();
        entry.0 += 1;
        entry.1 += rupees(*amount);
        entry.2 += fees;
        table.rows.push(json!({
            "date": date,
            "payer": name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "—".into()),
            "roll": number.clone().unwrap_or_default(),
            "purpose": words(purpose),
            "method": method.as_deref().map(words).unwrap_or_default(),
            "state": state,
            "payment": payment.clone().unwrap_or_else(|| order.clone()),
            "settledAt": settled_at,
            "fee": fees,
            "amount": rupees(*amount),
        }));
    }
    let tally = |key: &str| by_state.get(key).copied().unwrap_or_default();
    let (captured, settled, pending, failed) = (
        tally("Captured"),
        tally("Settled"),
        tally("Pending"),
        tally("Failed"),
    );
    let received = captured.1 + settled.1;
    table.totals = Some(json!({
        "date": "Received",
        "fee": round_money(captured.2 + settled.2),
        "amount": round_money(received),
    }));
    let mut states = Table::new(
        "By status",
        vec![
            col("state", "Status", "text"),
            col("count", "Payments", "number"),
            col("amount", "Amount", "money"),
            col("fee", "Fee + tax", "money"),
        ],
    );
    for (state, (count, amount, fee)) in &by_state {
        states.rows.push(json!({
            "state": state, "count": count, "amount": round_money(*amount),
            "fee": round_money(*fee),
        }));
    }
    Ok(Report {
        notes: vec![
            "Captured money has reached Razorpay; settled money has been paid out to the \
             campus account. Pending orders were started but not paid (yet)."
                .to_owned(),
        ],
        summary: vec![
            stat("Received", round_money(received), "money"),
            stat("Settled", round_money(settled.1), "money"),
            stat("Awaiting settlement", round_money(captured.1), "money"),
            stat("Pending", pending.0 as f64, "number"),
            stat("Failed", failed.0 as f64, "number"),
        ],
        tables: vec![states, table],
    })
}

/// ₹1,23,456.78 — Indian digit grouping, for the email body.
fn format_inr(value: f64) -> String {
    let negative = value < 0.0;
    let fixed = format!("{:.2}", value.abs());
    let (whole, fraction) = fixed.split_once('.').unwrap_or((&fixed, "00"));
    format!(
        "{}₹{}.{fraction}",
        if negative { "-" } else { "" },
        indian_grouping(whole)
    )
}

fn indian_grouping(digits: &str) -> String {
    if digits.len() <= 3 {
        return digits.to_owned();
    }
    let (head, last3) = digits.split_at(digits.len() - 3);
    let mut groups = Vec::new();
    let mut rest = head;
    while rest.len() > 2 {
        let (a, b) = rest.split_at(rest.len() - 2);
        groups.push(b);
        rest = a;
    }
    if !rest.is_empty() {
        groups.push(rest);
    }
    groups.reverse();
    format!("{},{last3}", groups.join(","))
}

fn format_stat(value: f64, format: &str) -> String {
    match format {
        "money" => format_inr(value),
        _ if value.fract() == 0.0 => {
            let grouped = indian_grouping(&format!("{}", value.abs() as i64));
            if value < 0.0 {
                format!("-{grouped}")
            } else {
                grouped
            }
        }
        _ => format!("{value:.1}"),
    }
}

// ---------------------------------------------------------------------------
// Emailing a report
// ---------------------------------------------------------------------------
//
// The app renders the PDF and CSV (one design, the same files a download
// gives) and posts them with the report parameters. The server re-runs the
// report itself for the email body, so the summary in the message is always
// the server's own figures, then sends a branded HTML email with the files.

/// Two attachments of up to 10 MB together, base64-encoded, plus the JSON.
const EMAIL_BODY_LIMIT: usize = 16 * 1024 * 1024;
const MAX_RECIPIENTS: usize = 10;
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
/// Report emails one person may send per rolling hour.
const MAX_SENDS_PER_HOUR: i64 = 20;
const MAX_NOTE_CHARS: usize = 500;
/// SuperCampus purple (Pantone 2665 C), as on the PDF header.
const BRAND_COLOR: &str = "#7B42F6";
const EMAIL_EVENT: &str = "report.emailed";
const EMAIL_NOT_CONFIGURED: &str = "Email is not configured on the server";

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct EmailReportRequest {
    #[serde(flatten)]
    query: ReportQuery,
    #[serde(default)]
    recipients: Vec<String>,
    #[serde(default)]
    attachments: Vec<AttachmentInput>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct AttachmentInput {
    filename: String,
    content_type: String,
    base64: String,
}

/// A plain address: something@domain.tld, no display name, no spaces.
fn is_valid_email(address: &str) -> bool {
    if address.len() > 254 || address.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if address.contains(['<', '>', ',', ';', '"', '(', ')', '[', ']', '\\']) {
        return false;
    }
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && !domain.contains('@')
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && domain.rsplit('.').next().is_some_and(|tld| tld.len() >= 2)
}

/// Trimmed, lower-cased, de-duplicated; 1 to [`MAX_RECIPIENTS`] valid addresses.
fn validate_recipients(raw: &[String]) -> ApiResult<Vec<String>> {
    let mut seen = HashSet::new();
    let mut recipients = Vec::new();
    let mut invalid = Vec::new();
    for address in raw {
        let address = address.trim().to_ascii_lowercase();
        if address.is_empty() {
            continue;
        }
        if !is_valid_email(&address) {
            invalid.push(address);
        } else if seen.insert(address.clone()) {
            recipients.push(address);
        }
    }
    if !invalid.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "Not a valid email address: {}",
            invalid.join(", ")
        )));
    }
    if recipients.is_empty() {
        return Err(ApiError::BadRequest(
            "Add at least one email address".into(),
        ));
    }
    if recipients.len() > MAX_RECIPIENTS {
        return Err(ApiError::BadRequest(format!(
            "Send to at most {MAX_RECIPIENTS} addresses at a time"
        )));
    }
    Ok(recipients)
}

/// `credits 2026/09.pdf` → `credits_2026_09.pdf`; the extension always
/// matches the file's real type.
fn safe_filename(raw: &str, kind: ReportKind, extension: &str) -> String {
    let stem = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches(&format!(".{extension}"))
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut stem = stem
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    stem.truncate(100);
    if stem.is_empty() {
        stem = kind.key().to_owned();
    }
    format!("{stem}.{extension}")
}

/// Only the report's own PDF and CSV: one of each at most, checked to be
/// what they claim, within the size limits.
fn validate_attachments(
    kind: ReportKind,
    inputs: &[AttachmentInput],
) -> ApiResult<Vec<supercampus_notifications::EmailAttachment>> {
    use base64::Engine as _;
    if inputs.is_empty() {
        return Err(ApiError::BadRequest(
            "Choose PDF, CSV or both to attach".into(),
        ));
    }
    if inputs.len() > 2 {
        return Err(ApiError::BadRequest(
            "Attach at most one PDF and one CSV".into(),
        ));
    }
    let mut formats = HashSet::new();
    let mut total = 0;
    let mut files = Vec::new();
    for input in inputs {
        let media = input
            .content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let (extension, content_type) = match media.as_str() {
            "application/pdf" => ("pdf", "application/pdf"),
            "text/csv" => ("csv", "text/csv; charset=utf-8"),
            _ => {
                return Err(ApiError::BadRequest(
                    "Only the report's PDF and CSV can be attached".into(),
                ));
            }
        };
        if !formats.insert(extension) {
            return Err(ApiError::BadRequest(
                "Attach at most one PDF and one CSV".into(),
            ));
        }
        let encoded: String = input.base64.chars().filter(|c| !c.is_whitespace()).collect();
        if encoded.len() > MAX_ATTACHMENT_BYTES / 3 * 4 + 8 {
            return Err(too_large());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| ApiError::BadRequest("An attachment could not be read".into()))?;
        if bytes.is_empty() {
            return Err(ApiError::BadRequest("An attachment is empty".into()));
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(too_large());
        }
        total += bytes.len();
        let valid = match extension {
            "pdf" => bytes.starts_with(b"%PDF-"),
            _ => std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
                .is_ok(),
        };
        if !valid {
            return Err(ApiError::BadRequest(format!(
                "The attached {} is not a valid {} file",
                extension.to_uppercase(),
                extension.to_uppercase()
            )));
        }
        files.push(supercampus_notifications::EmailAttachment {
            filename: safe_filename(&input.filename, kind, extension),
            content_type: content_type.to_owned(),
            bytes,
        });
    }
    if total > MAX_TOTAL_ATTACHMENT_BYTES {
        return Err(too_large());
    }
    Ok(files)
}

fn too_large() -> ApiError {
    ApiError::BadRequest(format!(
        "The attachments are too large to email (limit {} MB). Choose a shorter period, a single \
         shop, or only the CSV.",
        MAX_TOTAL_ATTACHMENT_BYTES / (1024 * 1024)
    ))
}

fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
struct ReportEmail {
    subject: String,
    text: String,
    html: String,
}

struct Sender<'a> {
    name: &'a str,
    email: &'a str,
}

/// The branded message for one generated report (the JSON [`build_report`]
/// returns): subject, plain text and HTML with the summary figures.
fn compose_email(
    report: &Value,
    sender: &Sender<'_>,
    note: Option<&str>,
    files: &[String],
) -> ReportEmail {
    let text_of = |key: &str| report[key].as_str().unwrap_or_default().trim().to_owned();
    let title = text_of("title");
    let period = text_of("periodLabel");
    let description = text_of("description");
    let institution = report["institutionName"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("SuperCampus")
        .to_owned();
    let shop = report["shop"]["name"].as_str().map(str::to_owned);
    let generated = report["generatedAt"]
        .as_str()
        .and_then(|raw| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S").ok())
        .map(|at| at.format("%-d %b %Y, %-I:%M %p").to_string())
        .unwrap_or_default();
    let stats: Vec<(String, String)> = report["summary"]
        .as_array()
        .map(|stats| {
            stats
                .iter()
                .map(|s| {
                    (
                        s["label"].as_str().unwrap_or_default().to_owned(),
                        format_stat(
                            s["value"].as_f64().unwrap_or_default(),
                            s["format"].as_str().unwrap_or("number"),
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let notes: Vec<String> = report["notes"]
        .as_array()
        .map(|notes| {
            notes
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let scope = [Some(period.clone()), shop.clone()]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");

    let mut subject = format!("{title} · {scope} — {institution}");
    if subject.chars().count() > 180 {
        subject = subject.chars().take(177).collect::<String>() + "...";
    }

    // Plain text.
    let mut text = format!("{institution}\n{title}\n{scope}\n");
    if !description.is_empty() {
        text.push_str(&format!("{description}\n"));
    }
    if let Some(note) = note {
        text.push_str(&format!("\nMessage from {}:\n{note}\n", sender.name));
    }
    if !stats.is_empty() {
        text.push_str("\nSummary\n");
        for (label, value) in &stats {
            text.push_str(&format!("  {label}: {value}\n"));
        }
    }
    for note in &notes {
        text.push_str(&format!("\n{note}"));
    }
    if !notes.is_empty() {
        text.push('\n');
    }
    if !files.is_empty() {
        text.push_str(&format!("\nAttached: {}\n", files.join(", ")));
    }
    text.push_str(&format!(
        "\nSent by {} ({}) from SuperCampus. Figures as generated {generated} (campus time).\n",
        sender.name, sender.email
    ));

    // HTML: table layout and inline styles, which every mail client renders.
    let e = escape_html;
    let mut stat_cells = String::new();
    for pair in stats.chunks(2) {
        stat_cells.push_str("<tr>");
        for (label, value) in pair {
            stat_cells.push_str(&format!(
                r#"<td width="50%" style="padding:6px 6px 6px 0;vertical-align:top"><div style="background:#F4F2FA;border-radius:10px;padding:10px 12px"><div style="font-size:12px;color:#6B6F7A">{}</div><div style="font-size:18px;font-weight:700;color:#15161A;margin-top:2px">{}</div></div></td>"#,
                e(label),
                e(value)
            ));
        }
        if pair.len() == 1 {
            stat_cells.push_str(r#"<td width="50%"></td>"#);
        }
        stat_cells.push_str("</tr>");
    }
    let note_html = note.map_or_else(String::new, |note| {
        format!(
            r#"<tr><td style="padding:0 28px 8px"><div style="border-left:3px solid {BRAND_COLOR};padding:8px 12px;background:#FAF9FD;color:#15161A;font-size:14px;line-height:1.45"><div style="font-size:12px;color:#6B6F7A;margin-bottom:4px">Message from {}</div>{}</div></td></tr>"#,
            e(sender.name),
            e(note).replace('\n', "<br>")
        )
    });
    let notes_html = notes
        .iter()
        .map(|note| {
            format!(
                r#"<p style="margin:0 0 6px;font-size:12.5px;line-height:1.45;color:#6B6F7A">{}</p>"#,
                e(note)
            )
        })
        .collect::<String>();
    let files_html = if files.is_empty() {
        String::new()
    } else {
        format!(
            r#"<tr><td style="padding:4px 28px 18px"><div style="font-size:13px;color:#15161A"><strong>Attached:</strong> {}</div></td></tr>"#,
            files.iter().map(|f| e(f)).collect::<Vec<_>>().join(", ")
        )
    };
    let description_html = if description.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div style="font-size:13px;color:#6B6F7A;margin-top:2px">{}</div>"#,
            e(&description)
        )
    };
    let html = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title_e}</title></head>
<body style="margin:0;padding:0;background:#F4F2FA;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:#F4F2FA;padding:24px 12px"><tr><td align="center">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:600px;background:#FFFFFF;border-radius:16px;overflow:hidden">
<tr><td style="background:{BRAND_COLOR};padding:18px 28px">
<div style="font-size:18px;font-weight:700;color:#FFFFFF;letter-spacing:-0.2px">SuperCampus</div>
<div style="font-size:13px;color:#EDE4FE;margin-top:2px">{institution_e}</div>
</td></tr>
<tr><td style="padding:24px 28px 8px">
<div style="font-size:22px;font-weight:700;color:#15161A;letter-spacing:-0.3px">{title_e}</div>
{description_html}
<div style="font-size:14px;color:#15161A;margin-top:8px">{scope_e}</div>
</td></tr>
{note_html}
<tr><td style="padding:8px 22px 8px 28px"><table role="presentation" width="100%" cellpadding="0" cellspacing="0">{stat_cells}</table></td></tr>
<tr><td style="padding:8px 28px 6px">{notes_html}</td></tr>
{files_html}
<tr><td style="padding:14px 28px 22px;border-top:1px solid #E4E5EA;font-size:12px;line-height:1.5;color:#6B6F7A">
Sent by {sender_name_e} ({sender_email_e}) from SuperCampus.<br>Figures as generated {generated_e} (campus time). The attached files carry every row.
</td></tr>
</table>
</td></tr></table>
</body></html>"#,
        title_e = e(&title),
        institution_e = e(&institution),
        scope_e = e(&scope),
        sender_name_e = e(sender.name),
        sender_email_e = e(sender.email),
        generated_e = e(&generated),
    );
    ReportEmail {
        subject,
        text,
        html,
    }
}

fn clean_note(raw: Option<String>) -> ApiResult<Option<String>> {
    let note = raw
        .map(|n| n.replace("\r\n", "\n").trim().to_owned())
        .filter(|n| !n.is_empty());
    if let Some(note) = &note
        && note.chars().count() > MAX_NOTE_CHARS
    {
        return Err(ApiError::BadRequest(format!(
            "Keep the message under {MAX_NOTE_CHARS} characters"
        )));
    }
    Ok(note)
}

/// `POST /reports/{kind}/email` with the report parameters (`from`, `to`,
/// `month`, `shop`, `items`), `recipients` (1–10 addresses), `attachments`
/// (`[{filename, contentType, base64}]`, the app's own PDF and/or CSV) and an
/// optional `note`.
///
/// * 503 when outbound email is disabled on this server — nothing is sent.
/// * 429 past [`MAX_SENDS_PER_HOUR`] sends by the same person.
/// * Every send, delivered or not, is recorded as a `report.emailed` event.
async fn email_report(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(kind): Path<String>,
    Json(body): Json<EmailReportRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let kind =
        ReportKind::parse(&kind).ok_or_else(|| ApiError::NotFound("Unknown report kind".into()))?;
    require_kind_access(&access, kind)?;
    let recipients = validate_recipients(&body.recipients)?;
    let attachments = validate_attachments(kind, &body.attachments)?;
    let note = clean_note(body.note)?;

    let mailer = state.mailer();
    if mailer.transport() == "disabled" {
        return Err(ApiError::ServiceUnavailable(EMAIL_NOT_CONFIGURED.into()));
    }

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let pool = db.pool();
    let tenant = tenant_id(pool, &principal.student.tenant_id).await?;
    let recent = sqlx::query_scalar::<_, i64>(
        r#"SELECT count(*)::int8 FROM campus_ops.events
           WHERE tenant_id=$1 AND event_type=$2 AND actor_user_id=$3
             AND created_at > now() - interval '1 hour'"#,
    )
    .bind(tenant)
    .bind(EMAIL_EVENT)
    .bind(&principal.student.id)
    .fetch_one(pool)
    .await?;
    if recent >= MAX_SENDS_PER_HOUR {
        return Err(ApiError::TooManyRequests(format!(
            "You have emailed {MAX_SENDS_PER_HOUR} reports in the last hour. Try again later."
        )));
    }

    let report = build_report(&state, &principal, &access, kind, &body.query).await?;
    let files: Vec<String> = attachments.iter().map(|a| a.filename.clone()).collect();
    let email = compose_email(
        &report,
        &Sender {
            name: &principal.student.name,
            email: &principal.student.email,
        },
        note.as_deref(),
        &files,
    );

    let mut sent = Vec::new();
    let mut failed = Vec::new();
    for to in &recipients {
        let message = supercampus_notifications::EmailMessage {
            to: to.clone(),
            subject: email.subject.clone(),
            text_body: email.text.clone(),
            html_body: Some(email.html.clone()),
        };
        match mailer
            .send_with_attachments(message, attachments.clone())
            .await
        {
            Ok(()) => sent.push(to.clone()),
            Err(error) => {
                tracing::warn!(error = ?error, kind = kind.key(), "failed to email a report");
                failed.push(to.clone());
            }
        }
    }

    let send_id = Uuid::new_v4();
    let payload = json!({
        "kind": kind.key(),
        "title": report["title"],
        "from": report["from"],
        "to": report["to"],
        "shop": report["shop"]["shopKey"],
        "recipients": recipients,
        "sent": sent,
        "failed": failed,
        "attachments": attachments.iter().map(|a| json!({
            "filename": a.filename, "contentType": a.content_type, "bytes": a.bytes.len(),
        })).collect::<Vec<_>>(),
        "hasNote": note.is_some(),
        "transport": mailer.transport(),
    });
    sqlx::query(
        "INSERT INTO campus_ops.events(tenant_id,module_key,aggregate_type,aggregate_id,event_type,actor_user_id,payload) VALUES($1,'reports','report',$2,$3,$4,$5)",
    )
    .bind(tenant)
    .bind(send_id.to_string())
    .bind(EMAIL_EVENT)
    .bind(&principal.student.id)
    .bind(&payload)
    .execute(pool)
    .await?;

    if sent.is_empty() {
        return Err(ApiError::BadGateway(
            "The email service couldn't send the report. Try again later.".into(),
        ));
    }
    Ok(Json(ApiResponse::new(json!({
        "id": send_id,
        "sent": sent,
        "failed": failed,
        "attachments": files,
        "transport": mailer.transport(),
        // The development log transport records the message instead of sending it.
        "delivered": mailer.transport() != "log",
    }))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shop_staff_report_on_their_own_shops_only() {
        let one = ["qa-canteen-2".to_string()];
        let two = ["qa-canteen-2".to_string(), "mec-canteen".to_string()];
        // Campus readers keep whatever they asked for, including every shop.
        assert_eq!(
            scoped_report_shop(ReportKind::DailySales, None, None).unwrap(),
            None
        );
        // A one-shop owner gets their shop by default ...
        assert_eq!(
            scoped_report_shop(ReportKind::DailySales, Some(&one), None).unwrap(),
            Some("qa-canteen-2")
        );
        // ... may name it, but never another canteen.
        assert!(
            scoped_report_shop(ReportKind::ItemSales, Some(&one), Some("mec-canteen")).is_err()
        );
        assert_eq!(
            scoped_report_shop(ReportKind::ItemSales, Some(&two), Some("mec-canteen")).unwrap(),
            Some("mec-canteen")
        );
        // Several shops: choose one. No shop: nothing to report on.
        assert!(scoped_report_shop(ReportKind::DailySales, Some(&two), None).is_err());
        assert!(scoped_report_shop(ReportKind::DailySales, Some(&[]), None).is_err());
        // Campus-wide kinds are not a shop's to read.
        assert!(scoped_report_shop(ReportKind::OnlinePayments, Some(&one), None).is_err());
    }
    use base64::Engine as _;

    #[test]
    fn every_kind_round_trips_its_key() {
        for kind in ReportKind::ALL {
            assert_eq!(ReportKind::parse(kind.key()), Some(kind));
            assert!(!kind.title().is_empty());
            assert!(!kind.description().is_empty());
        }
        let keys: HashSet<_> = ReportKind::ALL.iter().map(|k| k.key()).collect();
        assert_eq!(keys.len(), ReportKind::ALL.len());
        assert_eq!(
            ReportKind::parse("Vendor-Payable"),
            Some(ReportKind::VendorPayable)
        );
        assert_eq!(
            ReportKind::parse("daily-sales"),
            Some(ReportKind::DailySales)
        );
        assert!(ReportKind::parse("salaries").is_none());
    }

    #[test]
    fn kinds_without_records_are_gone() {
        for removed in ["auto_debit", "complimentary", "self_registered"] {
            assert!(ReportKind::parse(removed).is_none(), "{removed}");
        }
    }

    #[test]
    fn fee_kinds_need_their_own_grant_and_ignore_shops() {
        assert_eq!(
            ReportKind::PaymentRequests.extra_grant(),
            Some("fees.payment_requests.read")
        );
        assert_eq!(
            ReportKind::OnlinePayments.extra_grant(),
            Some("fees.online_payments.read")
        );
        assert!(!ReportKind::PaymentRequests.uses_shop());
        assert!(ReportKind::DailySales.uses_shop());
        assert!(ReportKind::DailySales.extra_grant().is_none());
    }

    #[test]
    fn months_cover_every_day() {
        let feb = parse_month("2028-02").unwrap();
        assert_eq!(feb.from, NaiveDate::from_ymd_opt(2028, 2, 1).unwrap());
        assert_eq!(feb.to, NaiveDate::from_ymd_opt(2028, 2, 29).unwrap());
        let dec = parse_month("2026-12").unwrap();
        assert_eq!(dec.to, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
        assert!(parse_month("2026-13").is_err());
        assert!(parse_month("September").is_err());
    }

    #[test]
    fn ranges_default_to_today_and_monthly_kinds_to_this_month() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        let range = resolve_range(ReportKind::Credits, &ReportQuery::default(), today).unwrap();
        assert_eq!((range.from, range.to), (today, today));
        let month =
            resolve_range(ReportKind::VendorPayable, &ReportQuery::default(), today).unwrap();
        assert_eq!(month.from, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        assert_eq!(month.to, NaiveDate::from_ymd_opt(2026, 9, 30).unwrap());
    }

    #[test]
    fn snapshots_ignore_the_period() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        let backwards = ReportQuery {
            from: Some("2026-09-10".into()),
            to: Some("2026-09-01".into()),
            ..Default::default()
        };
        let range = resolve_range(ReportKind::WalletBalances, &backwards, today).unwrap();
        assert_eq!((range.from, range.to), (today, today));
    }

    #[test]
    fn ranges_are_ordered_and_bounded() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        let backwards = ReportQuery {
            from: Some("2026-09-10".into()),
            to: Some("2026-09-01".into()),
            ..Default::default()
        };
        assert!(resolve_range(ReportKind::Debits, &backwards, today).is_err());
        let long_eod = ReportQuery {
            from: Some("2026-08-01".into()),
            to: Some("2026-09-15".into()),
            ..Default::default()
        };
        assert!(resolve_range(ReportKind::StudentEodWallet, &long_eod, today).is_err());
        assert!(resolve_range(ReportKind::Debits, &long_eod, today).is_ok());
        assert!(resolve_range(ReportKind::HourlySales, &long_eod, today).is_ok());
    }

    #[test]
    fn payable_is_purchases_less_refunds() {
        let payable = Payable {
            gross: 380.0,
            refunded: 65.5,
            ..Default::default()
        };
        assert_eq!(payable.net(), 314.5);
        let table = payable_table("t", &[payable.clone(), payable]);
        assert_eq!(table.totals.as_ref().unwrap()["payable"], 629.0);
    }

    #[test]
    fn transaction_types_read_as_words() {
        assert_eq!(transaction_type_label("manual_top_up"), "Manual top-up");
        assert_eq!(transaction_type_label("order_debit"), "Purchase");
        assert_eq!(transaction_type_label("manual_debit"), "Deduction");
        assert_eq!(
            transaction_type_label("wallet_deduction"),
            "Wallet deduction"
        );
        assert_eq!(words("bank_transfer"), "Bank transfer");
    }

    #[test]
    fn rupees_use_indian_grouping() {
        assert_eq!(format_inr(0.0), "₹0.00");
        assert_eq!(format_inr(999.5), "₹999.50");
        assert_eq!(format_inr(1234.0), "₹1,234.00");
        assert_eq!(format_inr(1234567.891), "₹12,34,567.89");
        assert_eq!(format_inr(-145.0), "-₹145.00");
        assert_eq!(format_stat(12345.0, "number"), "12,345");
        assert_eq!(format_stat(4.26, "number"), "4.3");
        assert_eq!(format_stat(600.5, "money"), "₹600.50");
    }

    #[test]
    fn margins_and_hours_read_naturally() {
        assert_eq!(margin(200.0, 150.0), 25.0);
        assert_eq!(margin(0.0, 10.0), 0.0);
        assert_eq!(hour_label(0), "12 AM – 1 AM");
        assert_eq!(hour_label(11), "11 AM – 12 PM");
        assert_eq!(hour_label(13), "1 PM – 2 PM");
        assert_eq!(hour_label(23), "11 PM – 12 AM");
    }

    #[test]
    fn the_hourly_grid_spans_the_busy_hours_by_weekday() {
        // Monday 9 AM: 2 orders; Wednesday 11 AM: 1 order.
        let cells = HashMap::from([((9, 1), (2, 130.0)), ((11, 3), (1, 65.0))]);
        let revenue = hourly_table("Revenue", &cells, false);
        assert_eq!(revenue.columns.len(), 9);
        assert_eq!(revenue.rows.len(), 3, "9, 10 and 11 o'clock");
        assert_eq!(revenue.rows[0]["mon"], 130.0);
        assert_eq!(revenue.rows[1]["total"], 0.0);
        assert_eq!(revenue.rows[2]["wed"], 65.0);
        let totals = revenue.totals.unwrap();
        assert_eq!(totals["total"], 195.0);
        let orders = hourly_table("Orders", &cells, true);
        assert_eq!(orders.rows[0]["mon"], 2.0);
        assert!(hourly_table("Empty", &HashMap::new(), false).rows.is_empty());
    }

    #[test]
    fn razorpay_states_follow_the_money() {
        assert_eq!(razorpay_state("created", false, false), "Pending");
        assert_eq!(razorpay_state("authorized", false, false), "Pending");
        assert_eq!(razorpay_state("captured", true, false), "Captured");
        assert_eq!(razorpay_state("captured", true, true), "Settled");
        assert_eq!(razorpay_state("failed", false, false), "Failed");
    }

    #[test]
    fn recipients_are_checked_trimmed_and_deduplicated() {
        let list = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            validate_recipients(&list(&[
                " Accounts@MEC.edu.in ",
                "accounts@mec.edu.in",
                "",
                "principal@mec.edu.in"
            ]))
            .unwrap(),
            vec!["accounts@mec.edu.in", "principal@mec.edu.in"]
        );
        for bad in [
            "no-at-sign",
            "a@b",
            "two@@mec.in",
            "a b@mec.in",
            "Name <a@mec.in>",
            "a@mec.in,b@mec.in",
            "a@-mec.in",
            ".a@mec.in",
        ] {
            let error = validate_recipients(&list(&[bad])).unwrap_err();
            assert!(
                matches!(error, ApiError::BadRequest(ref m) if m.contains("Not a valid")),
                "{bad}"
            );
        }
        assert!(validate_recipients(&list(&["", "  "])).is_err());
        let eleven: Vec<String> = (0..11).map(|i| format!("user{i}@mec.edu.in")).collect();
        assert!(validate_recipients(&eleven).is_err());
        assert_eq!(validate_recipients(&eleven[..10]).unwrap().len(), 10);
    }

    fn attachment(filename: &str, content_type: &str, bytes: &[u8]) -> AttachmentInput {
        AttachmentInput {
            filename: filename.into(),
            content_type: content_type.into(),
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    #[test]
    fn only_the_reports_own_pdf_and_csv_are_attached() {
        let files = validate_attachments(
            ReportKind::Credits,
            &[
                attachment("../credits 2026-09.pdf", "application/pdf", b"%PDF-1.7 ..."),
                attachment(
                    "credits_2026-09.csv",
                    "text/csv; charset=utf-8",
                    b"\xEF\xBB\xBFReport,Credits\r\n",
                ),
            ],
        )
        .unwrap();
        assert_eq!(files[0].filename, "credits_2026-09.pdf");
        assert_eq!(files[0].content_type, "application/pdf");
        assert_eq!(files[1].filename, "credits_2026-09.csv");
        assert_eq!(files[1].content_type, "text/csv; charset=utf-8");

        // Nothing chosen, a fake PDF, bad text, another type, two PDFs, bad base64.
        assert!(validate_attachments(ReportKind::Credits, &[]).is_err());
        assert!(
            validate_attachments(
                ReportKind::Credits,
                &[attachment("r.pdf", "application/pdf", b"<html>")]
            )
            .is_err()
        );
        assert!(
            validate_attachments(
                ReportKind::Credits,
                &[attachment("r.csv", "text/csv", &[0xff, 0xfe, 0x00])]
            )
            .is_err()
        );
        assert!(
            validate_attachments(
                ReportKind::Credits,
                &[attachment("r.exe", "application/octet-stream", b"MZ")]
            )
            .is_err()
        );
        let pdf = attachment("r.pdf", "application/pdf", b"%PDF-1");
        let pdf2 = attachment("s.pdf", "application/pdf", b"%PDF-1");
        assert!(validate_attachments(ReportKind::Credits, &[pdf, pdf2]).is_err());
        let broken = AttachmentInput {
            filename: "r.csv".into(),
            content_type: "text/csv".into(),
            base64: "not base64!".into(),
        };
        assert!(validate_attachments(ReportKind::Credits, &[broken]).is_err());
    }

    #[test]
    fn oversized_attachments_are_refused() {
        let mut big = b"%PDF-".to_vec();
        big.resize(MAX_ATTACHMENT_BYTES + 1, b'0');
        let error = validate_attachments(
            ReportKind::Master,
            &[attachment("m.pdf", "application/pdf", &big)],
        )
        .unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(ref m) if m.contains("too large")));
    }

    #[test]
    fn filenames_are_safe_and_typed() {
        assert_eq!(
            safe_filename("C:\\x\\Wallet Balances!.pdf", ReportKind::WalletBalances, "pdf"),
            "Wallet_Balances.pdf"
        );
        assert_eq!(
            safe_filename("...", ReportKind::WalletBalances, "csv"),
            "wallet_balances.csv"
        );
        assert_eq!(
            safe_filename("report.pdf", ReportKind::Credits, "csv"),
            "report_pdf.csv"
        );
    }

    fn sample_report() -> Value {
        json!({
            "kind": "credits",
            "title": "Credits",
            "description": "Wallet top-ups and credit transactions",
            "periodLabel": "1 Sep 2026 to 29 Sep 2026",
            "shop": { "shopKey": "mec-canteen", "name": "Canteen" },
            "institutionName": "Madras <Engineering> College",
            "generatedAt": "2026-09-29T13:39:31",
            "notes": ["Manual top-ups & online ones."],
            "summary": [
                { "label": "Credits", "value": 12, "format": "number" },
                { "label": "Total credited", "value": 134600.5, "format": "money" },
                { "label": "Manual top-up", "value": 1346, "format": "money" },
            ],
            "tables": [],
        })
    }

    #[test]
    fn the_email_is_branded_escaped_and_states_the_summary() {
        let email = compose_email(
            &sample_report(),
            &Sender {
                name: "Abhinaya",
                email: "abhinaya@mec.local",
            },
            Some("Please review <before> Friday"),
            &["credits.pdf".into(), "credits.csv".into()],
        );
        assert_eq!(
            email.subject,
            "Credits · 1 Sep 2026 to 29 Sep 2026 · Canteen — Madras <Engineering> College"
        );
        assert!(email.html.contains(BRAND_COLOR));
        assert!(email.html.contains("SuperCampus"));
        assert!(email.html.contains("Madras &lt;Engineering&gt; College"));
        assert!(!email.html.contains("<Engineering>"));
        assert!(email.html.contains("Please review &lt;before&gt; Friday"));
        assert!(email.html.contains("₹1,34,600.50"));
        assert!(email.html.contains("Manual top-ups &amp; online ones."));
        assert!(email.html.contains("credits.pdf, credits.csv"));
        assert!(email.html.contains("29 Sep 2026, 1:39 PM"));
        assert!(email.text.contains("Total credited: ₹1,34,600.50"));
        assert!(email.text.contains("Credits: 12"));
        assert!(email.text.contains("Attached: credits.pdf, credits.csv"));
        assert!(email.text.contains("Sent by Abhinaya (abhinaya@mec.local)"));
    }

    #[test]
    fn a_note_is_optional_and_bounded() {
        assert_eq!(clean_note(Some("  ".into())).unwrap(), None);
        assert_eq!(
            clean_note(Some(" hi\r\nthere ".into())).unwrap().as_deref(),
            Some("hi\nthere")
        );
        assert!(clean_note(Some("x".repeat(MAX_NOTE_CHARS + 1))).is_err());
        let email = compose_email(
            &sample_report(),
            &Sender {
                name: "A",
                email: "a@mec.local",
            },
            None,
            &[],
        );
        assert!(!email.html.contains("Message from"));
        assert!(!email.text.contains("Attached:"));
    }

    #[test]
    fn the_email_request_reads_the_report_parameters() {
        let body: EmailReportRequest = serde_json::from_value(json!({
            "from": "2026-09-01", "to": "2026-09-29", "shop": "mec-canteen",
            "recipients": ["a@mec.edu.in"],
            "attachments": [{ "filename": "r.csv", "contentType": "text/csv", "base64": "YQ==" }],
        }))
        .unwrap();
        assert_eq!(body.query.from.as_deref(), Some("2026-09-01"));
        assert_eq!(body.query.shop.as_deref(), Some("mec-canteen"));
        assert_eq!(body.recipients.len(), 1);
        assert_eq!(body.attachments[0].content_type, "text/csv");
        assert!(body.note.is_none());
    }
}

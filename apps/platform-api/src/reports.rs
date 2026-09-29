//! Finance reports for the accountant and administrators: the app's
//! "Reports" page asks for one kind over a period, then renders the tables it
//! gets back as a PDF or CSV on the device.
//!
//! `GET /reports/options` — the shops and menu items the parameter pickers offer.
//! `GET /reports/{kind}?from=YYYY-MM-DD&to=YYYY-MM-DD&month=YYYY-MM&shop=<key>&items=<id,id>`
//!
//! * Every figure is read from the tenant's own ledger
//!   (`campus_ops.canteen_wallet_transactions`), orders
//!   (`campus_ops.canteen_orders`), laundry charges and wallets. Days are the
//!   tenant's local calendar (Asia/Kolkata).
//! * A kind whose source does not exist in this platform (complimentary
//!   servings, meal-compliance auto debits, accounts self-registered through
//!   the retired mobile API) still answers, with `available: false`, no rows
//!   and a note saying why — never invented figures.
//! * Reading tenant-wide balances is finance work: both the sales analytics
//!   grant and the wallet-ledger grant are required.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    routing::get,
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

fn require_report_access(access: &EffectiveAccess) -> ApiResult<()> {
    for grant in REPORT_GRANTS {
        require(access, grant)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportKind {
    Master,
    Credits,
    Debits,
    Refunds,
    VendorPayable,
    ShopTransactions,
    AccountantCredits,
    AutoDebit,
    ParameterSales,
    Complimentary,
    StudentEodWallet,
    SelfRegistered,
}

impl ReportKind {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        Some(
            match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                "master" => Self::Master,
                "credits" => Self::Credits,
                "debits" => Self::Debits,
                "refunds" => Self::Refunds,
                "vendor_payable" => Self::VendorPayable,
                "shop_transactions" => Self::ShopTransactions,
                "accountant_credits" => Self::AccountantCredits,
                "auto_debit" => Self::AutoDebit,
                "parameter_sales" => Self::ParameterSales,
                "complimentary" => Self::Complimentary,
                "student_eod_wallet" => Self::StudentEodWallet,
                "self_registered" => Self::SelfRegistered,
                _ => return None,
            },
        )
    }

    fn key(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Credits => "credits",
            Self::Debits => "debits",
            Self::Refunds => "refunds",
            Self::VendorPayable => "vendor_payable",
            Self::ShopTransactions => "shop_transactions",
            Self::AccountantCredits => "accountant_credits",
            Self::AutoDebit => "auto_debit",
            Self::ParameterSales => "parameter_sales",
            Self::Complimentary => "complimentary",
            Self::StudentEodWallet => "student_eod_wallet",
            Self::SelfRegistered => "self_registered",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Master => "Master Report",
            Self::Credits => "Credits",
            Self::Debits => "Debits",
            Self::Refunds => "Refunds",
            Self::VendorPayable => "Vendor Payable",
            Self::ShopTransactions => "Shop-wise Transactions",
            Self::AccountantCredits => "Accountant Credits",
            Self::AutoDebit => "Auto Debit History",
            Self::ParameterSales => "Parameter Sales",
            Self::Complimentary => "Complimentary Consumption",
            Self::StudentEodWallet => "Student EOD Wallet",
            Self::SelfRegistered => "Self-Registered Students",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Master => "Full operations: sales, vendors, ledger",
            Self::Credits => "Wallet top-ups and credit transactions",
            Self::Debits => "Purchases and wallet debit transactions",
            Self::Refunds => "Order refunds and reversal transactions",
            Self::VendorPayable => "Monthly vendor settlement statement",
            Self::ShopTransactions => "Monthly shop ledger with payable calculations",
            Self::AccountantCredits => "Credits manually added by the accountant",
            Self::AutoDebit => "Meal compliance debited student list with item and amount details",
            Self::ParameterSales => {
                "Sales and count for selected menu items in the selected period"
            }
            Self::Complimentary => {
                "Free items served to visitors, clients, guests, staff or others"
            }
            Self::StudentEodWallet => "Day-wise end-of-day wallet balance for every student",
            Self::SelfRegistered => "Students who created accounts through the old mobile API",
        }
    }

    /// Settlements run by calendar month rather than a free range.
    fn is_monthly(self) -> bool {
        matches!(self, Self::VendorPayable | Self::ShopTransactions)
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
/// month); every other kind takes `from`/`to` (default: today).
fn resolve_range(kind: ReportKind, query: &ReportQuery, today: NaiveDate) -> ApiResult<DateRange> {
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

/// "manual_top_up" → "Manual top-up"; unknown kinds are shown as written.
fn transaction_type_label(raw: &str) -> String {
    match raw {
        "manual_top_up" => "Manual top-up".into(),
        "online_top_up" => "Online top-up".into(),
        "order_debit" => "Purchase".into(),
        "refund" => "Refund".into(),
        other => {
            let spaced = other.replace(['_', '-'], " ");
            let mut chars = spaced.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        }
    }
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
    available: bool,
    notes: Vec<String>,
    summary: Vec<Value>,
    tables: Vec<Table>,
}

fn stat(label: &str, value: f64, format: &str) -> Value {
    json!({ "label": label, "value": value, "format": format })
}

fn unavailable(notes: &[&str], table: Table) -> Report {
    Report {
        available: false,
        notes: notes.iter().map(|note| (*note).to_owned()).collect(),
        summary: Vec::new(),
        tables: vec![table],
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
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
    let shops = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'shopKey', shop_key, 'name', name, 'category', lower(category),
             'isActive', is_active) ORDER BY name), '[]'::jsonb)
           FROM campus_ops.shops WHERE tenant_id=$1"#,
    )
    .bind(tenant)
    .fetch_one(pool)
    .await?;
    let items = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'id', item.id, 'name', item.name, 'category', item.category,
             'shopKey', item.store, 'shopName', COALESCE(shop.name, item.store),
             'price', item.price::float8) ORDER BY COALESCE(shop.name, item.store), item.name), '[]'::jsonb)
           FROM campus_ops.canteen_menu_items item
           LEFT JOIN campus_ops.shops shop ON shop.tenant_id=item.tenant_id AND shop.shop_key=item.store
           WHERE item.tenant_id=$1"#,
    )
    .bind(tenant)
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
    }))))
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
    require_report_access(&access)?;
    let kind =
        ReportKind::parse(&kind).ok_or_else(|| ApiError::NotFound("Unknown report kind".into()))?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let pool = db.pool();
    let tenant = tenant_id(pool, &principal.student.tenant_id).await?;
    let today = local_today(pool).await?;
    let range = resolve_range(kind, &query, today)?;

    let shop = match query
        .shop
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all"))
    {
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
        shop_key: shop_key.as_deref(),
    };
    let report = match kind {
        ReportKind::Master => master_report(&ctx).await?,
        ReportKind::Credits => credits_report(&ctx).await?,
        ReportKind::Debits => debits_report(&ctx).await?,
        ReportKind::Refunds => refunds_report(&ctx).await?,
        ReportKind::VendorPayable => vendor_payable_report(&ctx).await?,
        ReportKind::ShopTransactions => shop_transactions_report(&ctx).await?,
        ReportKind::AccountantCredits => accountant_credits_report(&ctx).await?,
        ReportKind::AutoDebit => auto_debit_report(),
        ReportKind::ParameterSales => parameter_sales_report(&ctx, &items).await?,
        ReportKind::Complimentary => complimentary_report(),
        ReportKind::StudentEodWallet => student_eod_report(&ctx, today).await?,
        ReportKind::SelfRegistered => self_registered_report(),
    };

    let (institution, generated_at) = sqlx::query_as::<_, (Option<String>, String)>(
        r#"SELECT (SELECT name FROM platform.tenants WHERE id=$1),
                  to_char(now() AT TIME ZONE $2, 'YYYY-MM-DD"T"HH24:MI:SS')"#,
    )
    .bind(tenant)
    .bind(TENANT_TIMEZONE)
    .fetch_one(pool)
    .await?;
    let period_label = if kind.is_monthly() {
        range.from.format("%B %Y").to_string()
    } else {
        range.label()
    };
    Ok(Json(ApiResponse::new(json!({
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
        "available": report.available,
        "notes": report.notes,
        "summary": report.summary,
        "tables": report.tables.iter().map(Table::to_json).collect::<Vec<_>>(),
    }))))
}

struct Ctx<'a> {
    pool: &'a sqlx::PgPool,
    control: &'a sqlx::PgPool,
    tenant: Uuid,
    range: DateRange,
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
             t.user_id, t.shop_key, shop.name, t.amount::float8, t.transaction_type,
             t.description, t.actor_user_id, student.full_name, student.student_number,
             o.order_number, o.rejection_reason
           FROM campus_ops.canteen_wallet_transactions t
           LEFT JOIN campus_ops.shops shop
             ON shop.tenant_id=t.tenant_id AND shop.shop_key=t.shop_key
           LEFT JOIN core.students student
             ON student.tenant_id=t.tenant_id AND student.user_account_id::text=t.user_id
           LEFT JOIN campus_ops.canteen_orders o
             ON o.tenant_id=t.tenant_id AND o.id::text=t.reference_id
           WHERE t.tenant_id=$1
             AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
             AND ($5::text IS NULL OR t.shop_key=$5)
             AND ({filter})
           ORDER BY t.created_at, t.id
           LIMIT $6"#
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
        available: true,
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
        available: true,
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
        available: true,
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
             ON t.tenant_id=s.tenant_id AND t.shop_key=s.shop_key
            AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
           WHERE s.tenant_id=$1 AND ($5::text IS NULL OR s.shop_key=$5)
           GROUP BY s.shop_key, s.name, s.category
           ORDER BY s.name"#,
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
        available: true,
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
        available: true,
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
        available: true,
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

fn auto_debit_report() -> Report {
    unavailable(
        &[
            "No automatic meal-compliance debits exist on this platform: shops can be marked \
           meal-compliant, but no wallet is ever debited automatically for it. There is \
           nothing to list.",
        ],
        Table::new(
            "Auto debits",
            vec![
                col("date", "Date", "date"),
                col("name", "Name", "text"),
                col("roll", "Roll no.", "text"),
                col("item", "Item", "text"),
                col("amount", "Amount", "money"),
            ],
        ),
    )
}

fn complimentary_report() -> Report {
    unavailable(
        &[
            "Complimentary servings are not recorded on this platform: every order is paid \
           from a wallet, and there is no free-item or guest-serving record. There is \
           nothing to list.",
        ],
        Table::new(
            "Complimentary servings",
            vec![
                col("date", "Date", "date"),
                col("servedTo", "Served to", "text"),
                col("category", "Category", "text"),
                col("item", "Item", "text"),
                col("quantity", "Quantity", "number"),
                col("value", "Value", "money"),
            ],
        ),
    )
}

fn self_registered_report() -> Report {
    unavailable(
        &[
            "Accounts on this platform are created by administrators or roster imports; \
           there is no self-registration, and accounts from the old mobile API were not \
           carried over. There is nothing to list.",
        ],
        Table::new(
            "Self-registered students",
            vec![
                col("registeredAt", "Registered", "datetime"),
                col("name", "Name", "text"),
                col("roll", "Roll no.", "text"),
                col("email", "Email", "text"),
            ],
        ),
    )
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
        available: true,
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

async fn student_eod_report(ctx: &Ctx<'_>, today: NaiveDate) -> ApiResult<Report> {
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
    let rows = sqlx::query_as::<_, (NaiveDate, Option<String>, String, f64)>(
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
             WHERE w.tenant_id=$1 AND ($5::text IS NULL OR w.shop_key=$5)
             GROUP BY lower(w.user_id)
           )
           SELECT d.day, st.student_number, st.full_name,
             round((COALESCE(b.balance, 0) - COALESCE((
               SELECT sum(t.amount)::float8 FROM campus_ops.canteen_wallet_transactions t
               WHERE t.tenant_id=$1 AND lower(t.user_id)=st.user_id
                 AND ($5::text IS NULL OR t.shop_key=$5)
                 AND t.created_at >= ((d.day + 1)::timestamp AT TIME ZONE $2)
             ), 0))::numeric, 2)::float8
           FROM days d CROSS JOIN students st
           LEFT JOIN balances b ON b.user_id=st.user_id
           ORDER BY d.day, st.student_number NULLS LAST, st.full_name"#,
    )
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
        available: true,
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

    let ledger = sqlx::query_as::<_, (String, i64, f64)>(
        r#"SELECT t.transaction_type, count(*)::int8, sum(t.amount)::float8
           FROM campus_ops.canteen_wallet_transactions t
           WHERE t.tenant_id=$1 AND (t.created_at AT TIME ZONE $2)::date BETWEEN $3 AND $4
             AND ($5::text IS NULL OR t.shop_key=$5)
           GROUP BY 1 ORDER BY 1"#,
    )
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
        available: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_its_key() {
        for key in [
            "master",
            "credits",
            "debits",
            "refunds",
            "vendor_payable",
            "shop_transactions",
            "accountant_credits",
            "auto_debit",
            "parameter_sales",
            "complimentary",
            "student_eod_wallet",
            "self_registered",
        ] {
            assert_eq!(ReportKind::parse(key).unwrap().key(), key);
        }
        assert_eq!(
            ReportKind::parse("Vendor-Payable"),
            Some(ReportKind::VendorPayable)
        );
        assert!(ReportKind::parse("salaries").is_none());
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
    fn unavailable_kinds_say_so_and_list_nothing() {
        for report in [
            auto_debit_report(),
            complimentary_report(),
            self_registered_report(),
        ] {
            assert!(!report.available);
            assert!(!report.notes.is_empty());
            assert!(report.tables.iter().all(|t| t.rows.is_empty()));
        }
    }

    #[test]
    fn transaction_types_read_as_words() {
        assert_eq!(transaction_type_label("manual_top_up"), "Manual top-up");
        assert_eq!(transaction_type_label("order_debit"), "Purchase");
        assert_eq!(
            transaction_type_label("wallet_deduction"),
            "Wallet deduction"
        );
    }
}

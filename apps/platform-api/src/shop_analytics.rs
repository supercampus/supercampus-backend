//! One shop's sales, cost, profit and counter-staff performance over an
//! explicit date range — the owner workspace's "Sales" destination.
//!
//! `GET /canteen/shop-analytics?shop=<key>&from=YYYY-MM-DD&to=YYYY-MM-DD`
//!
//! Every figure is derived from what was recorded; nothing is estimated. A
//! figure that cannot be derived is `null` (the app shows "not recorded").
//!
//! * `from`/`to` are inclusive calendar days in the tenant's local time
//!   (Asia/Kolkata). Both default to today; one alone is a single day.
//!
//! **Shop summary** (`summary`) — the orders *placed* in the range:
//! * Revenue is completed orders only: rejected orders are refunded and
//!   cancelled ones never charged.
//! * Cost is each delivered line's quantity times its menu item's cost price
//!   (`actual_price`). A line whose item no longer exists has no recorded
//!   cost: `uncostedItems` counts them and cost, profit and margin are `null`
//!   rather than a guess.
//! * `averageHandlingMinutes` is placement to the last item's recorded
//!   hand-over, over completed orders whose every item has a recorded
//!   hand-over (`timedOrders`); `null` without any.
//!
//! **Staff** (`captains`, `staffSummary`) — what each person *did* in the
//! range, from the per-item action log (`campus_ops.canteen_order_events`,
//! see `order_events`), by when they did it:
//! * `itemsDelivered` / `revenue`: the items they handed over and those
//!   items' line totals. Items of orders since rejected or cancelled are not
//!   revenue. `ordersDelivered`: orders they handed over at least one item of;
//!   `ordersTouched`: orders they moved at all.
//! * `itemsPrepared`: items they moved to preparing or ready.
//! * `rejectedOrders` / `refunded`: orders they rejected and the refunds.
//! * `averagePrepSeconds`: preparing → ready, credited to whoever marked the
//!   item ready, only where both steps were recorded. `averageHandoverSeconds`:
//!   ready → delivered (placed → delivered for instant items), credited to
//!   whoever handed it over, only where the earlier step was recorded.
//! * `revenueShare`: their delivered revenue over everything delivered in the
//!   range (`staffSummary.totalRevenue`).
//! * Every active account the admin assigned to the shop as a captain is
//!   listed, idle or not; anyone else who acted (an owner, an administrator, a
//!   captain since unassigned) follows. Deactivated and deleted accounts are
//!   not listed; what they did still counts in the totals.
//! * Orders completed before the log existed have no record of who handled
//!   which item. They are not credited to anyone: `staffSummary.unattributed`
//!   reports them ("before tracking"), dated by when the order completed.
//!
//! Adding `captain=<userId>` (with optional `page` and `pageSize`) returns
//! that person's detail for the same range under `captainDetail`: their
//! figures, a per-day series and every action they took, paginated.

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
    order_events::{line_amount, line_item_id, line_quantity},
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

/// Actions per page of a captain's detail, by default and at most.
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

    let placed = shop_orders(pool, tenant, &shop_key, range, OrderScope::Placed).await?;
    let completed = shop_orders(pool, tenant, &shop_key, range, OrderScope::Completed).await?;
    let events = range_events(pool, tenant, &shop_key, range).await?;

    // Every order the range's figures touch, and everything recorded on them
    // (an item made ready in the range may have been started before it).
    let mut orders: HashMap<Uuid, OrderFact> = HashMap::new();
    for order in placed.iter().chain(&completed) {
        orders.entry(order.id).or_insert_with(|| order.clone());
    }
    let missing: Vec<Uuid> = events
        .iter()
        .map(|event| event.order_id)
        .filter(|id| !orders.contains_key(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for order in orders_by_id(pool, tenant, &missing).await? {
        orders.insert(order.id, order);
    }
    let order_ids: Vec<Uuid> = orders.keys().copied().collect();
    let history = order_history(pool, tenant, &order_ids).await?;

    let mut item_ids: BTreeSet<String> = events.iter().filter_map(|e| e.item_id.clone()).collect();
    for order in orders.values() {
        for line in order.lines() {
            item_ids.extend(line_item_id(line));
        }
    }
    let costs = item_costs(pool, tenant, &item_ids.into_iter().collect::<Vec<_>>()).await?;

    let staff = shop_staff(pool, tenant, &shop_key).await?;
    let unknown: Vec<String> = events
        .iter()
        .map(|event| event.actor.clone())
        .filter(|id| !staff.iter().any(|member| member.matches(id)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let (others, hidden) = other_actors(pool, &unknown).await?;

    let facts = Facts {
        placed: &placed,
        completed: &completed,
        events: &events,
        history: &history,
        orders: &orders,
        costs: &costs,
    };
    let rows = staff_rows(&facts, &staff, &others, &hidden);
    let mut body = build_report(&facts, &rows);
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
        let names = actor_names(&rows, &others, &history);
        body["captainDetail"] = captain_detail(
            &facts,
            row,
            range,
            &body,
            &names,
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

    fn day_list(self) -> Vec<NaiveDate> {
        self.from
            .iter_days()
            .take_while(|day| *day <= self.to)
            .collect()
    }
}

/// One order of the shop.
#[derive(Debug, Clone, PartialEq)]
struct OrderFact {
    id: Uuid,
    order_number: i64,
    customer_name: String,
    status: String,
    handled_by: Option<String>,
    total: f64,
    fulfilment_mode: String,
    token_number: Option<i32>,
    lines: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl OrderFact {
    fn lines(&self) -> &[Value] {
        self.lines.as_array().map(Vec::as_slice).unwrap_or_default()
    }
    fn is_completed(&self) -> bool {
        self.status == "completed"
    }
    fn is_active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "pending" | "accepted" | "preparing" | "ready"
        )
    }
    /// Refunded or never charged: nothing it delivered is revenue.
    fn is_void(&self) -> bool {
        matches!(self.status.as_str(), "rejected" | "cancelled")
    }
}

/// One recorded action (see `order_events`).
#[derive(Debug, Clone, PartialEq)]
struct EventFact {
    id: i64,
    order_id: Uuid,
    line_index: Option<i32>,
    item_id: Option<String>,
    item_name: Option<String>,
    action: String,
    source: String,
    actor: String,
    actor_name: Option<String>,
    quantity: i64,
    amount: f64,
    instant: bool,
    occurred_at: DateTime<Utc>,
    /// The calendar day it happened, in the tenant's time zone.
    day: NaiveDate,
}

/// What a report is computed from.
struct Facts<'a> {
    /// Orders placed in the range.
    placed: &'a [OrderFact],
    /// Orders completed (last moved) in the range.
    completed: &'a [OrderFact],
    /// The shop's actions in the range, newest first.
    events: &'a [EventFact],
    /// Every action ever recorded on the orders above, oldest first.
    history: &'a [EventFact],
    orders: &'a HashMap<Uuid, OrderFact>,
    /// Menu item id → its cost price.
    costs: &'a HashMap<String, f64>,
}

impl Facts<'_> {
    /// When an item first reached `action`, if that was recorded.
    fn first(&self, order: Uuid, line: i32, action: &str) -> Option<DateTime<Utc>> {
        self.history
            .iter()
            .filter(|e| e.order_id == order && e.line_index == Some(line) && e.action == action)
            .map(|e| e.occurred_at)
            .min()
    }

    /// The order's items with a recorded hand-over, and the last one's time.
    fn deliveries(&self, order: Uuid) -> (HashSet<i32>, Option<DateTime<Utc>>) {
        let mut lines = HashSet::new();
        let mut last = None;
        for event in self
            .history
            .iter()
            .filter(|e| e.order_id == order && e.action == "delivered")
        {
            if let Some(line) = event.line_index {
                lines.insert(line);
                last = last.max(Some(event.occurred_at));
            }
        }
        (lines, last)
    }

    /// A delivered item's cost, when its menu item still records one.
    fn line_cost(&self, item_id: Option<&str>, quantity: i64) -> Option<f64> {
        item_id
            .and_then(|id| self.costs.get(id))
            .map(|cost| cost * quantity as f64)
    }

    /// Whether a delivered action still counts as a sale.
    fn counts_as_sale(&self, event: &EventFact) -> bool {
        event.action == "delivered"
            && event.line_index.is_some()
            && !self
                .orders
                .get(&event.order_id)
                .is_some_and(OrderFact::is_void)
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
    fn matches(&self, actor: &str) -> bool {
        self.assignment_user_id == actor || self.identity_id == actor
    }
}

/// Someone who acted without being assigned to the shop.
#[derive(Debug, Clone, PartialEq)]
struct OtherActor {
    name: String,
    email: Option<String>,
    last_login: Option<DateTime<Utc>>,
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round_one(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn share(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        round_one(part / whole * 100.0)
    } else {
        0.0
    }
}

fn average(sum: f64, count: i64) -> Value {
    if count > 0 {
        json!(round_one(sum / count as f64))
    } else {
        Value::Null
    }
}

fn seconds_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    ((to - from).num_milliseconds().max(0) as f64) / 1000.0
}

/// The shop's orders placed in the range.
#[derive(Debug, Default, Clone, PartialEq)]
struct Summary {
    orders: i64,
    completed: i64,
    active: i64,
    rejected: i64,
    cancelled: i64,
    items: i64,
    revenue: f64,
    cost: f64,
    uncosted_items: i64,
    active_value: f64,
    refunded: f64,
    handling_seconds: f64,
    timed_orders: i64,
    waiting: i64,
}

fn summarise(facts: &Facts) -> Summary {
    let mut s = Summary::default();
    for order in facts.placed {
        s.orders += 1;
        if order.is_completed() {
            s.completed += 1;
            s.revenue += order.total;
            for line in order.lines() {
                let quantity = line_quantity(line);
                s.items += quantity;
                match facts.line_cost(line_item_id(line).as_deref(), quantity) {
                    Some(cost) => s.cost += cost,
                    None => s.uncosted_items += quantity,
                }
            }
            let (delivered, last) = facts.deliveries(order.id);
            let every_item = (0..order.lines().len() as i32).all(|i| delivered.contains(&i));
            if let (true, Some(last)) = (every_item && !order.lines().is_empty(), last) {
                s.handling_seconds += seconds_between(order.created_at, last);
                s.timed_orders += 1;
            }
        } else if order.is_active() {
            s.active += 1;
            s.active_value += order.total;
            if order.handled_by.is_none() {
                s.waiting += 1;
            }
        } else if order.status == "rejected" {
            s.rejected += 1;
            s.refunded += order.total;
        } else if order.status == "cancelled" {
            s.cancelled += 1;
        }
    }
    s
}

impl Summary {
    fn json(&self) -> Value {
        let costed = self.uncosted_items == 0;
        let profit = self.revenue - self.cost;
        json!({
            "orders": self.orders,
            "completedOrders": self.completed,
            "activeOrders": self.active,
            "rejectedOrders": self.rejected,
            "cancelledOrders": self.cancelled,
            "itemsSold": self.items,
            "revenue": round_money(self.revenue),
            "cost": costed.then(|| round_money(self.cost)),
            "profit": costed.then(|| round_money(profit)),
            "marginPercent": (costed && self.revenue > 0.0)
                .then(|| round_one(profit / self.revenue * 100.0)),
            "uncostedItems": self.uncosted_items,
            "averageOrderValue": if self.completed > 0 {
                round_money(self.revenue / self.completed as f64)
            } else { 0.0 },
            "activeValue": round_money(self.active_value),
            "refunded": round_money(self.refunded),
            "averageHandlingMinutes": average(self.handling_seconds / 60.0, self.timed_orders),
            "timedOrders": self.timed_orders,
            "waitingOrders": self.waiting,
            // Older apps read this name for orders nobody has picked up.
            "unattributedOrders": self.waiting,
        })
    }
}

/// Completed orders in the range with items nobody is recorded handing over:
/// they predate the action log, so they are credited to no one.
#[derive(Debug, Default, Clone, PartialEq)]
struct Unattributed {
    orders: i64,
    items: i64,
    revenue: f64,
}

fn unattributed(facts: &Facts) -> Unattributed {
    let mut u = Unattributed::default();
    for order in facts.completed {
        let (delivered, _) = facts.deliveries(order.id);
        let mut counted = false;
        for (index, line) in order.lines().iter().enumerate() {
            if !delivered.contains(&(index as i32)) {
                u.items += line_quantity(line);
                u.revenue += line_amount(line);
                counted = true;
            }
        }
        if counted {
            u.orders += 1;
        }
    }
    u
}

/// One person's actions in the range.
#[derive(Debug, Default, Clone, PartialEq)]
struct Work {
    items_delivered: i64,
    delivered_orders: BTreeSet<Uuid>,
    touched_orders: BTreeSet<Uuid>,
    revenue: f64,
    cost: f64,
    uncosted_items: i64,
    /// (order, line) → quantity, for items they moved to preparing or ready.
    prepared: HashMap<(Uuid, i32), i64>,
    rejected_orders: BTreeSet<Uuid>,
    refunded: f64,
    prep_seconds: f64,
    prep_timed: i64,
    handover_seconds: f64,
    handover_timed: i64,
    first: Option<DateTime<Utc>>,
    last: Option<DateTime<Utc>>,
    days: BTreeSet<NaiveDate>,
    actions: i64,
}

impl Work {
    fn add(&mut self, facts: &Facts, event: &EventFact) {
        self.actions += 1;
        self.touched_orders.insert(event.order_id);
        self.first = Some(
            self.first
                .map_or(event.occurred_at, |t| t.min(event.occurred_at)),
        );
        self.last = self.last.max(Some(event.occurred_at));
        self.days.insert(event.day);
        match (event.action.as_str(), event.line_index) {
            ("delivered", Some(line)) if facts.counts_as_sale(event) => {
                self.items_delivered += event.quantity;
                self.delivered_orders.insert(event.order_id);
                self.revenue += event.amount;
                match facts.line_cost(event.item_id.as_deref(), event.quantity) {
                    Some(cost) => self.cost += cost,
                    None => self.uncosted_items += event.quantity,
                }
                let ready = facts.first(event.order_id, line, "ready");
                let started = match ready {
                    Some(ready) => Some(ready),
                    None if event.instant => {
                        facts.orders.get(&event.order_id).map(|o| o.created_at)
                    }
                    None => None,
                };
                if let Some(started) = started {
                    self.handover_seconds += seconds_between(started, event.occurred_at);
                    self.handover_timed += 1;
                }
            }
            ("preparing", Some(line)) => {
                self.prepared.insert((event.order_id, line), event.quantity);
            }
            ("ready", Some(line)) => {
                self.prepared.insert((event.order_id, line), event.quantity);
                if let Some(started) = facts.first(event.order_id, line, "preparing") {
                    self.prep_seconds += seconds_between(started, event.occurred_at);
                    self.prep_timed += 1;
                }
            }
            ("rejected", _) => {
                if self.rejected_orders.insert(event.order_id) {
                    self.refunded += event.amount;
                }
            }
            _ => {}
        }
    }

    fn items_prepared(&self) -> i64 {
        self.prepared.values().sum()
    }

    fn json(&self, total_revenue: f64) -> Value {
        let costed = self.uncosted_items == 0;
        json!({
            "itemsDelivered": self.items_delivered,
            "ordersDelivered": self.delivered_orders.len(),
            "ordersTouched": self.touched_orders.len(),
            "revenue": round_money(self.revenue),
            "cost": costed.then(|| round_money(self.cost)),
            "profit": costed.then(|| round_money(self.revenue - self.cost)),
            "uncostedItems": self.uncosted_items,
            "revenueShare": share(self.revenue, total_revenue),
            "itemsPrepared": self.items_prepared(),
            "rejectedOrders": self.rejected_orders.len(),
            "refunded": round_money(self.refunded),
            "averagePrepSeconds": average(self.prep_seconds, self.prep_timed),
            "prepTimedItems": self.prep_timed,
            "averageHandoverSeconds": average(self.handover_seconds, self.handover_timed),
            "handoverTimedItems": self.handover_timed,
            "firstActivityAt": self.first,
            "lastActivityAt": self.last,
            "lastHandledAt": self.last,
            "activeDays": self.days.len(),
            "actions": self.actions,
        })
    }
}

/// One person's row.
struct StaffRow<'a> {
    user_id: String,
    /// Other ids the same person's actions may carry (a legacy assignment id).
    aliases: Vec<String>,
    name: String,
    email: Option<String>,
    role: Option<String>,
    assigned: bool,
    last_login: Option<DateTime<Utc>>,
    work: Work,
    /// Their actions in the range, newest first.
    events: Vec<&'a EventFact>,
}

impl StaffRow<'_> {
    fn is(&self, actor: &str) -> bool {
        self.user_id == actor || self.aliases.iter().any(|alias| alias == actor)
    }
}

/// Credits the range's actions to whoever took them. Every assigned captain
/// is kept, idle or not; anyone else only when they acted. Hidden
/// (deactivated or deleted) accounts are left out.
fn staff_rows<'a>(
    facts: &Facts<'a>,
    staff: &[StaffMember],
    others: &HashMap<String, OtherActor>,
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
            work: Work::default(),
            events: Vec::new(),
        })
        .collect();

    for event in facts.events {
        if hidden.contains(&event.actor) {
            continue;
        }
        let index = match rows.iter().position(|row| row.is(&event.actor)) {
            Some(index) => index,
            None => {
                let other = others.get(&event.actor);
                rows.push(StaffRow {
                    user_id: event.actor.clone(),
                    aliases: Vec::new(),
                    name: other
                        .map(|o| o.name.clone())
                        .or_else(|| event.actor_name.clone())
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| "Former staff".into()),
                    email: other.and_then(|o| o.email.clone()),
                    role: None,
                    assigned: false,
                    last_login: other.and_then(|o| o.last_login),
                    work: Work::default(),
                    events: Vec::new(),
                });
                rows.len() - 1
            }
        };
        rows[index].work.add(facts, event);
        rows[index].events.push(event);
    }

    // Captains first — every one of them, even idle — then owners and anyone
    // else who acted; most delivered revenue first within each group.
    let rank = |row: &StaffRow| match row.role.as_deref() {
        Some("captain") => 0,
        Some(_) => 1,
        None => 2,
    };
    rows.retain(|row| row.role.as_deref() == Some("captain") || row.work.actions > 0);
    rows.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then(b.work.revenue.total_cmp(&a.work.revenue))
            .then(b.work.items_delivered.cmp(&a.work.items_delivered))
            .then(b.work.actions.cmp(&a.work.actions))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows
}

fn row_json(row: &StaffRow, total_revenue: f64) -> Value {
    let mut value = row.work.json(total_revenue);
    value["userId"] = json!(row.user_id);
    value["name"] = json!(row.name);
    value["email"] = json!(row.email);
    value["role"] = json!(row.role);
    value["assigned"] = json!(row.assigned);
    value["lastSeenAt"] = json!(row.last_login);
    value
}

/// Everything delivered in the range: what the log credits to someone (all
/// actors, listed or not) plus what predates it.
fn delivered_totals(facts: &Facts, unattributed: &Unattributed) -> (f64, i64, f64) {
    let mut revenue = 0.0;
    let mut items = 0;
    for event in facts.events.iter().filter(|e| facts.counts_as_sale(e)) {
        revenue += event.amount;
        items += event.quantity;
    }
    (revenue, items, revenue + unattributed.revenue)
}

/// Rolls the range up for the shop and per staff member.
fn build_report(facts: &Facts, rows: &[StaffRow]) -> Value {
    let summary = summarise(facts);
    let before = unattributed(facts);
    let (tracked_revenue, tracked_items, total_revenue) = delivered_totals(facts, &before);
    let captains: Vec<Value> = rows
        .iter()
        .map(|row| row_json(row, total_revenue))
        .collect();
    json!({
        "summary": summary.json(),
        "staffSummary": {
            "trackedRevenue": round_money(tracked_revenue),
            "trackedItems": tracked_items,
            "totalRevenue": round_money(total_revenue),
            "totalItems": tracked_items + before.items,
            "unattributed": {
                "orders": before.orders,
                "items": before.items,
                "revenue": round_money(before.revenue),
                "revenueShare": share(before.revenue, total_revenue),
            },
        },
        "captains": captains,
    })
}

/// Who is recorded acting on each order, by display name, in order.
fn actor_names(
    rows: &[StaffRow],
    others: &HashMap<String, OtherActor>,
    history: &[EventFact],
) -> HashMap<Uuid, Vec<String>> {
    let mut names: HashMap<Uuid, Vec<String>> = HashMap::new();
    for event in history {
        let name = rows
            .iter()
            .find(|row| row.is(&event.actor))
            .map(|row| row.name.clone())
            .or_else(|| others.get(&event.actor).map(|o| o.name.clone()))
            .or_else(|| event.actor_name.clone())
            .unwrap_or_else(|| "Former staff".into());
        let list = names.entry(event.order_id).or_default();
        if !list.contains(&name) {
            list.push(name);
        }
    }
    names
}

/// Everything the order detail page shows, in the shape the canteen order
/// endpoints use. `captainName` names everyone recorded acting on it.
fn order_json(order: &OrderFact, handlers: Option<&Vec<String>>, shop_key: &str) -> Value {
    json!({
        "id": order.id,
        "orderNumber": order.order_number,
        "customerName": order.customer_name,
        "status": order.status,
        "total": round_money(order.total),
        "itemCount": order.lines().iter().map(line_quantity).sum::<i64>(),
        "createdAt": order.created_at,
        "updatedAt": order.updated_at,
        "lines": order.lines,
        "fulfilmentMode": order.fulfilment_mode,
        "tokenNumber": order.token_number,
        "captainName": handlers.filter(|names| !names.is_empty()).map(|names| names.join(", ")),
        "shopKey": shop_key,
    })
}

/// One person's figures, a day-by-day series across the whole range (idle
/// days included, nothing smoothed) and one page of their actions, newest
/// first.
#[allow(clippy::too_many_arguments)]
fn captain_detail(
    facts: &Facts,
    row: &StaffRow,
    range: DateRange,
    report: &Value,
    names: &HashMap<Uuid, Vec<String>>,
    page: usize,
    page_size: usize,
    shop_key: &str,
) -> Value {
    let total_revenue = report["staffSummary"]["totalRevenue"]
        .as_f64()
        .unwrap_or(0.0);
    let daily: Vec<Value> = range
        .day_list()
        .into_iter()
        .map(|day| {
            let mut work = Work::default();
            for event in row.events.iter().filter(|e| e.day == day) {
                work.add(facts, event);
            }
            json!({
                "date": day.to_string(),
                "itemsDelivered": work.items_delivered,
                "ordersDelivered": work.delivered_orders.len(),
                "revenue": round_money(work.revenue),
                "itemsPrepared": work.items_prepared(),
                "actions": work.actions,
            })
        })
        .collect();

    let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
    let total = row.events.len();
    let total_pages = total.div_ceil(page_size).max(1);
    let page = page.clamp(1, total_pages);
    let activity: Vec<Value> = row
        .events
        .iter()
        .skip((page - 1) * page_size)
        .take(page_size)
        .map(|event| {
            let order = facts.orders.get(&event.order_id);
            json!({
                "id": event.id,
                "occurredAt": event.occurred_at,
                "action": event.action,
                "source": event.source,
                "orderId": event.order_id,
                "orderNumber": order.map(|o| o.order_number),
                "lineIndex": event.line_index,
                "itemName": event.item_name,
                "quantity": event.quantity,
                "amount": round_money(event.amount),
                // A hand-over on an order since refunded is not a sale.
                "countsAsSale": facts.counts_as_sale(event),
                "order": order.map(|o| order_json(o, names.get(&o.id), shop_key)),
            })
        })
        .collect();

    let mut value = row_json(row, total_revenue);
    value["daily"] = Value::Array(daily);
    value["activity"] = Value::Array(activity);
    value["page"] = json!(page);
    value["pageSize"] = json!(page_size);
    value["totalActivity"] = json!(total);
    value["totalPages"] = json!(total_pages);
    value
}

/// Which of the shop's orders to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrderScope {
    /// Placed in the range.
    Placed,
    /// Completed, and last moved in the range.
    Completed,
}

type OrderRow = (
    Uuid,
    i64,
    String,
    String,
    Option<String>,
    f64,
    String,
    Option<i32>,
    Value,
    DateTime<Utc>,
    DateTime<Utc>,
);

const ORDER_COLUMNS: &str = "o.id, o.order_number, o.customer_name, o.status, o.handled_by, \
     o.total::float8, o.fulfilment_mode, o.token_number, \
     CASE WHEN jsonb_typeof(o.lines)='array' THEN o.lines ELSE '[]'::jsonb END, \
     o.created_at, o.updated_at";

fn order_fact(r: OrderRow) -> OrderFact {
    OrderFact {
        id: r.0,
        order_number: r.1,
        customer_name: r.2,
        status: r.3,
        handled_by: r.4.filter(|id| !id.trim().is_empty()),
        total: r.5,
        fulfilment_mode: r.6,
        token_number: r.7,
        lines: r.8,
        created_at: r.9,
        updated_at: r.10,
    }
}

async fn shop_orders(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    shop_key: &str,
    range: DateRange,
    scope: OrderScope,
) -> ApiResult<Vec<OrderFact>> {
    let window = match scope {
        OrderScope::Placed => {
            "o.created_at >= ($4::date::timestamp AT TIME ZONE $3) \
             AND o.created_at < (($5::date + 1)::timestamp AT TIME ZONE $3)"
        }
        OrderScope::Completed => {
            "o.status = 'completed' \
             AND o.updated_at >= ($4::date::timestamp AT TIME ZONE $3) \
             AND o.updated_at < (($5::date + 1)::timestamp AT TIME ZONE $3)"
        }
    };
    // Orders whose `store` predates the shop register resolve to the shop of
    // the matching category, exactly as ordering and the sales dashboard do.
    let rows = sqlx::query_as::<_, OrderRow>(&format!(
        r#"SELECT {ORDER_COLUMNS}
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
          WHERE o.tenant_id=$1
            AND COALESCE(exact.shop_key, fallback.shop_key, o.store)=$2
            AND {window}
          ORDER BY o.created_at DESC"#
    ))
    .bind(tenant)
    .bind(shop_key)
    .bind(TENANT_TIMEZONE)
    .bind(range.from)
    .bind(range.to)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(order_fact).collect())
}

async fn orders_by_id(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    ids: &[Uuid],
) -> ApiResult<Vec<OrderFact>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, OrderRow>(&format!(
        "SELECT {ORDER_COLUMNS} FROM campus_ops.canteen_orders o \
         WHERE o.tenant_id=$1 AND o.id = ANY($2)"
    ))
    .bind(tenant)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(order_fact).collect())
}

type EventRow = (
    i64,
    Uuid,
    Option<i32>,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    i64,
    f64,
    bool,
    DateTime<Utc>,
    NaiveDate,
);

const EVENT_COLUMNS: &str = "e.id, e.order_id, e.line_index, e.item_id, e.item_name, e.action, \
     e.source, e.actor_user_id, e.actor_name, e.quantity::int8, e.amount::float8, e.instant, \
     e.occurred_at, (e.occurred_at AT TIME ZONE $3)::date";

fn event_fact(r: EventRow) -> EventFact {
    EventFact {
        id: r.0,
        order_id: r.1,
        line_index: r.2,
        item_id: r.3,
        item_name: r.4,
        action: r.5,
        source: r.6,
        actor: r.7,
        actor_name: r.8,
        quantity: r.9,
        amount: r.10,
        instant: r.11,
        occurred_at: r.12,
        day: r.13,
    }
}

/// The shop's recorded actions in the range, newest first.
async fn range_events(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    shop_key: &str,
    range: DateRange,
) -> ApiResult<Vec<EventFact>> {
    let rows = sqlx::query_as::<_, EventRow>(&format!(
        "SELECT {EVENT_COLUMNS} FROM campus_ops.canteen_order_events e \
         WHERE e.tenant_id=$1 AND e.shop_key=$2 \
           AND e.occurred_at >= ($4::date::timestamp AT TIME ZONE $3) \
           AND e.occurred_at < (($5::date + 1)::timestamp AT TIME ZONE $3) \
         ORDER BY e.occurred_at DESC, e.id DESC"
    ))
    .bind(tenant)
    .bind(shop_key)
    .bind(TENANT_TIMEZONE)
    .bind(range.from)
    .bind(range.to)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(event_fact).collect())
}

/// Every action recorded on `orders`, oldest first.
async fn order_history(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    orders: &[Uuid],
) -> ApiResult<Vec<EventFact>> {
    if orders.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, EventRow>(&format!(
        "SELECT {EVENT_COLUMNS} FROM campus_ops.canteen_order_events e \
         WHERE e.tenant_id=$1 AND e.order_id = ANY($2) \
         ORDER BY e.occurred_at, e.id"
    ))
    .bind(tenant)
    .bind(orders)
    .bind(TENANT_TIMEZONE)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(event_fact).collect())
}

/// Each menu item's cost price, for the items that still exist.
async fn item_costs(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    item_ids: &[String],
) -> ApiResult<HashMap<String, f64>> {
    if item_ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(sqlx::query_as::<_, (String, f64)>(
        "SELECT id::text, actual_price::float8 FROM campus_ops.canteen_menu_items \
         WHERE tenant_id=$1 AND id::text = ANY($2) AND actual_price IS NOT NULL",
    )
    .bind(tenant)
    .bind(item_ids)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect())
}

/// Accounts that acted without being assigned: the live ones by name, and
/// the deactivated or deleted ones to leave out of the list.
async fn other_actors(
    pool: &sqlx::PgPool,
    ids: &[String],
) -> ApiResult<(HashMap<String, OtherActor>, HashSet<String>)> {
    let mut others = HashMap::new();
    let mut hidden = HashSet::new();
    if ids.is_empty() {
        return Ok((others, hidden));
    }
    let rows = sqlx::query_as::<_, (String, String, Option<String>, bool, Option<DateTime<Utc>>)>(
        &format!(
            "SELECT u.id::text, COALESCE(NULLIF(trim(u.display_name), ''), u.email, u.id::text), \
                    u.email, ({LIVE_ACCOUNT_SQL}) AS live, u.last_login_at \
             FROM identity.users u WHERE u.id::text = ANY($1)"
        ),
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    for (id, name, email, live, last_login) in rows {
        if live {
            others.insert(
                id,
                OtherActor {
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
    Ok((others, hidden))
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
    use chrono::{Duration, TimeZone};

    fn day(raw: &str) -> NaiveDate {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").unwrap()
    }

    /// 2026-09-01 10:00 IST plus `minutes`.
    fn at(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 1, 4, 30, 0).unwrap() + Duration::minutes(minutes)
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Order `n`: a prepared Dosa (₹40 × 2, cost 25 each) and an instant
    /// Chips (₹10 × 1, cost 6), placed at minute 0.
    fn order(n: u128, status: &str) -> OrderFact {
        OrderFact {
            id: uuid(n),
            order_number: n as i64,
            customer_name: "Student".into(),
            status: status.into(),
            handled_by: Some("someone".into()),
            total: 90.0,
            fulfilment_mode: "pickup".into(),
            token_number: None,
            lines: json!([
                { "itemId": "dosa", "name": "Dosa", "price": 40, "quantity": 2 },
                { "itemId": "chips", "name": "Chips", "price": 10, "quantity": 1, "isInstant": true }
            ]),
            created_at: at(0),
            updated_at: at(30),
        }
    }

    fn event(
        id: i64,
        order: u128,
        line: Option<i32>,
        action: &str,
        actor: &str,
        minute: i64,
    ) -> EventFact {
        let (item, quantity, amount, instant) = match line {
            Some(0) => (Some("dosa"), 2, 80.0, false),
            Some(_) => (Some("chips"), 1, 10.0, true),
            None => (
                None,
                3,
                if action == "rejected" { 90.0 } else { 0.0 },
                false,
            ),
        };
        let occurred_at = at(minute);
        EventFact {
            id,
            order_id: uuid(order),
            line_index: line,
            item_id: item.map(str::to_owned),
            item_name: item.map(str::to_owned),
            action: action.into(),
            source: "item".into(),
            actor: actor.into(),
            actor_name: Some(actor.to_uppercase()),
            quantity,
            amount,
            instant,
            occurred_at,
            day: (occurred_at + Duration::minutes(330)).date_naive(),
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

    fn costs() -> HashMap<String, f64> {
        [("dosa".to_string(), 25.0), ("chips".to_string(), 6.0)].into()
    }

    struct World {
        placed: Vec<OrderFact>,
        completed: Vec<OrderFact>,
        events: Vec<EventFact>,
        orders: HashMap<Uuid, OrderFact>,
        costs: HashMap<String, f64>,
    }

    impl World {
        fn new(orders: Vec<OrderFact>, mut events: Vec<EventFact>) -> Self {
            events.sort_by_key(|e| e.occurred_at);
            Self {
                completed: orders
                    .iter()
                    .filter(|o| o.is_completed())
                    .cloned()
                    .collect(),
                orders: orders.iter().map(|o| (o.id, o.clone())).collect(),
                placed: orders,
                events,
                costs: costs(),
            }
        }

        fn report(&self, staff: &[StaffMember], hidden: &HashSet<String>) -> Value {
            let mut newest = self.events.clone();
            newest.reverse();
            let facts = Facts {
                placed: &self.placed,
                completed: &self.completed,
                events: &newest,
                history: &self.events,
                orders: &self.orders,
                costs: &self.costs,
            };
            let rows = staff_rows(&facts, staff, &HashMap::new(), hidden);
            let mut report = build_report(&facts, &rows);
            let names = actor_names(&rows, &HashMap::new(), &self.events);
            report["details"] = Value::Array(
                rows.iter()
                    .map(|row| {
                        captain_detail(
                            &facts,
                            row,
                            DateRange {
                                from: day("2026-08-31"),
                                to: day("2026-09-02"),
                            },
                            &report,
                            &names,
                            1,
                            3,
                            "mec-canteen",
                        )
                    })
                    .collect(),
            );
            report
        }
    }

    fn staff() -> Vec<StaffMember> {
        vec![
            member("anu", "captain", "Anu"),
            member("bala", "captain", "Bala"),
            member("idle", "captain", "Idle"),
            member("own", "owner", "Owner"),
        ]
    }

    fn captain<'a>(report: &'a Value, name: &str) -> &'a Value {
        report["captains"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()
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
        assert_eq!(range.day_list().len(), 30);
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
    fn each_person_is_credited_with_exactly_the_items_they_moved() {
        // Order 1: Anu prepares and readies the dosa, Bala hands it over;
        // the owner scans the chips out first.
        // Order 2: Bala hands the chips over; the dosa is still cooking.
        // Order 3: Anu rejects it.
        let world = World::new(
            vec![
                order(1, "completed"),
                order(2, "preparing"),
                order(3, "rejected"),
            ],
            vec![
                event(1, 1, Some(1), "delivered", "own", 2),
                event(2, 1, Some(0), "preparing", "anu", 3),
                event(3, 1, Some(0), "ready", "anu", 11),
                event(4, 1, Some(0), "delivered", "bala", 14),
                event(5, 2, Some(1), "delivered", "bala", 5),
                event(6, 2, Some(0), "preparing", "anu", 6),
                event(7, 3, None, "rejected", "anu", 7),
            ],
        );
        let report = world.report(&staff(), &HashSet::new());
        let names: Vec<&str> = report["captains"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        // Captains first (busiest first, idle kept), then the owner.
        assert_eq!(names, ["Bala", "Anu", "Idle", "Owner"]);

        let bala = captain(&report, "Bala");
        assert_eq!(bala["itemsDelivered"], 3);
        assert_eq!(bala["ordersDelivered"], 2);
        assert_eq!(bala["revenue"], 90.0);
        assert_eq!(bala["cost"], 56.0);
        assert_eq!(bala["profit"], 34.0);
        // Total delivered = 80 + 10 (Bala) + 10 (owner) = 100.
        assert_eq!(bala["revenueShare"], 90.0);
        // Dosa: ready at 11, delivered at 14. Chips: placed 0, delivered 5.
        assert_eq!(bala["averageHandoverSeconds"], 240.0);
        assert_eq!(bala["handoverTimedItems"], 2);
        assert_eq!(bala["itemsPrepared"], 0);
        assert!(bala["averagePrepSeconds"].is_null());

        let anu = captain(&report, "Anu");
        assert_eq!(anu["itemsDelivered"], 0);
        assert_eq!(anu["revenue"], 0.0);
        assert_eq!(anu["itemsPrepared"], 4);
        assert_eq!(anu["averagePrepSeconds"], 480.0);
        assert_eq!(anu["prepTimedItems"], 1);
        assert_eq!(anu["rejectedOrders"], 1);
        assert_eq!(anu["refunded"], 90.0);
        assert_eq!(anu["ordersTouched"], 3);
        assert_eq!(anu["actions"], 4);
        assert_eq!(anu["activeDays"], 1);

        let owner = captain(&report, "Owner");
        assert_eq!(owner["role"], "owner");
        assert_eq!(owner["itemsDelivered"], 1);
        assert_eq!(owner["revenue"], 10.0);
        assert_eq!(owner["revenueShare"], 10.0);
        assert_eq!(owner["averageHandoverSeconds"], 120.0);

        let idle = captain(&report, "Idle");
        assert_eq!(idle["actions"], 0);
        assert!(idle["averageHandoverSeconds"].is_null());
        assert!(idle["lastActivityAt"].is_null());

        let staff = &report["staffSummary"];
        assert_eq!(staff["trackedRevenue"], 100.0);
        assert_eq!(staff["totalRevenue"], 100.0);
        assert_eq!(staff["unattributed"]["orders"], 0);

        let summary = &report["summary"];
        assert_eq!(summary["orders"], 3);
        assert_eq!(summary["revenue"], 90.0);
        assert_eq!(summary["cost"], 56.0);
        assert_eq!(summary["profit"], 34.0);
        assert_eq!(summary["refunded"], 90.0);
        // Order 1: placed 0, last item handed over at 14.
        assert_eq!(summary["averageHandlingMinutes"], 14.0);
        assert_eq!(summary["timedOrders"], 1);
    }

    #[test]
    fn orders_from_before_tracking_are_credited_to_no_one() {
        let mut legacy = order(9, "completed");
        legacy.handled_by = Some("anu".into());
        let world = World::new(
            vec![legacy, order(1, "completed")],
            vec![
                event(1, 1, Some(0), "delivered", "bala", 10),
                event(2, 1, Some(1), "delivered", "bala", 10),
            ],
        );
        let report = world.report(&staff(), &HashSet::new());
        // handled_by is not trusted: Anu gets nothing for the legacy order.
        assert_eq!(captain(&report, "Anu")["revenue"], 0.0);
        let bala = captain(&report, "Bala");
        assert_eq!(bala["revenue"], 90.0);
        assert_eq!(bala["revenueShare"], 50.0);
        // Delivered from pending: no ready step was recorded for the dosa, so
        // only the instant chips have a hand-over time.
        assert_eq!(bala["handoverTimedItems"], 1);
        assert_eq!(bala["averageHandoverSeconds"], 600.0);
        let unattributed = &report["staffSummary"]["unattributed"];
        assert_eq!(unattributed["orders"], 1);
        assert_eq!(unattributed["items"], 3);
        assert_eq!(unattributed["revenue"], 90.0);
        assert_eq!(unattributed["revenueShare"], 50.0);
        assert_eq!(report["staffSummary"]["totalRevenue"], 180.0);
        // The legacy order has no recorded hand-over, so it is not timed.
        assert_eq!(report["summary"]["timedOrders"], 1);
        assert_eq!(report["summary"]["averageHandlingMinutes"], 10.0);
    }

    #[test]
    fn missing_costs_are_not_recorded_rather_than_guessed() {
        let mut world = World::new(
            vec![order(1, "completed")],
            vec![
                event(1, 1, Some(0), "delivered", "anu", 10),
                event(2, 1, Some(1), "delivered", "anu", 10),
            ],
        );
        world.costs.remove("chips");
        let report = world.report(&staff(), &HashSet::new());
        let anu = captain(&report, "Anu");
        assert_eq!(anu["revenue"], 90.0);
        assert!(anu["cost"].is_null());
        assert!(anu["profit"].is_null());
        assert_eq!(anu["uncostedItems"], 1);
        assert!(report["summary"]["cost"].is_null());
        assert!(report["summary"]["profit"].is_null());
        assert!(report["summary"]["marginPercent"].is_null());
        assert_eq!(report["summary"]["uncostedItems"], 1);
    }

    #[test]
    fn hand_overs_on_refunded_orders_are_not_sales() {
        let world = World::new(
            vec![order(1, "rejected")],
            vec![
                event(1, 1, Some(1), "delivered", "anu", 2),
                event(2, 1, None, "rejected", "bala", 5),
            ],
        );
        let report = world.report(&staff(), &HashSet::new());
        assert_eq!(captain(&report, "Anu")["itemsDelivered"], 0);
        assert_eq!(captain(&report, "Anu")["revenue"], 0.0);
        assert_eq!(captain(&report, "Bala")["refunded"], 90.0);
        assert_eq!(report["staffSummary"]["totalRevenue"], 0.0);
    }

    #[test]
    fn deactivated_actors_are_not_listed_but_stay_in_the_totals() {
        let world = World::new(
            vec![order(1, "completed")],
            vec![
                event(1, 1, Some(0), "delivered", "anu", 10),
                event(2, 1, Some(1), "delivered", "gone", 10),
            ],
        );
        let hidden: HashSet<String> = ["gone".to_string()].into();
        let report = world.report(&[member("anu", "captain", "Anu")], &hidden);
        let captains = report["captains"].as_array().unwrap();
        assert_eq!(captains.len(), 1);
        assert_eq!(captains[0]["revenueShare"], 88.9);
        assert_eq!(report["staffSummary"]["trackedRevenue"], 90.0);
    }

    #[test]
    fn unassigned_actors_follow_by_their_recorded_name() {
        let world = World::new(
            vec![order(1, "completed")],
            vec![event(1, 1, Some(1), "delivered", "admin-1", 10)],
        );
        let report = world.report(&[], &HashSet::new());
        let captains = report["captains"].as_array().unwrap();
        assert_eq!(captains.len(), 1);
        assert_eq!(captains[0]["name"], "ADMIN-1");
        assert!(captains[0]["role"].is_null());
        assert_eq!(captains[0]["assigned"], false);
    }

    #[test]
    fn no_assignments_and_no_actions_means_no_captains() {
        let world = World::new(vec![], vec![]);
        let report = world.report(&[], &HashSet::new());
        assert!(report["captains"].as_array().unwrap().is_empty());
        assert_eq!(report["summary"]["orders"], 0);
        assert!(report["summary"]["averageHandlingMinutes"].is_null());
        // Nothing sold: cost is recorded (zero), not unknown.
        assert_eq!(report["summary"]["cost"], 0.0);
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
        let world = World::new(
            vec![order(1, "completed")],
            vec![
                event(1, 1, Some(0), "delivered", "uuid-1", 3),
                event(2, 1, Some(1), "delivered", "dev@mec.local", 3),
            ],
        );
        let report = world.report(&staff, &HashSet::new());
        let captains = report["captains"].as_array().unwrap();
        assert_eq!(captains.len(), 1);
        assert_eq!(captains[0]["itemsDelivered"], 3);
        assert_eq!(captains[0]["userId"], "uuid-1");
    }

    #[test]
    fn captain_detail_has_a_daily_series_and_paginated_actions() {
        let mut late = event(5, 2, Some(1), "delivered", "anu", 0);
        // 2026-09-02 00:30 IST: the next local day.
        late.occurred_at = Utc.with_ymd_and_hms(2026, 9, 1, 19, 0, 0).unwrap();
        late.day = day("2026-09-02");
        let world = World::new(
            vec![order(1, "completed"), order(2, "completed")],
            vec![
                event(1, 1, Some(0), "preparing", "anu", 1),
                event(2, 1, Some(0), "ready", "anu", 4),
                event(3, 1, Some(0), "delivered", "anu", 6),
                event(4, 1, Some(1), "delivered", "bala", 7),
                late,
            ],
        );
        let report = world.report(&staff(), &HashSet::new());
        let details = report["details"].as_array().unwrap();
        let anu = details.iter().find(|d| d["name"] == "Anu").unwrap();
        let daily = anu["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 3);
        assert_eq!(daily[0]["date"], "2026-08-31");
        assert_eq!(daily[0]["actions"], 0);
        assert_eq!(daily[1]["itemsDelivered"], 2);
        assert_eq!(daily[1]["revenue"], 80.0);
        assert_eq!(daily[1]["itemsPrepared"], 2);
        assert_eq!(daily[1]["actions"], 3);
        assert_eq!(daily[2]["revenue"], 10.0);
        assert_eq!(anu["activeDays"], 2);
        assert_eq!(anu["totalActivity"], 4);
        assert_eq!(anu["totalPages"], 2);
        let activity = anu["activity"].as_array().unwrap();
        assert_eq!(activity.len(), 3);
        // Newest first, each with its order for the detail page.
        assert_eq!(activity[0]["action"], "delivered");
        assert_eq!(activity[0]["itemName"], "chips");
        assert_eq!(activity[0]["orderNumber"], 2);
        assert_eq!(activity[1]["order"]["captainName"], "Anu, Bala");
        assert_eq!(activity[1]["order"]["shopKey"], "mec-canteen");
        assert_eq!(activity[1]["order"]["lines"][0]["name"], "Dosa");
        // Order 2's dosa has no recorded hand-over: ₹80 before tracking, so
        // Anu's ₹90 is half of the ₹180 delivered.
        assert_eq!(anu["revenueShare"], 50.0);
    }
}

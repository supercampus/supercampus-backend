//! The per-item action log of canteen orders: who accepted, prepared, made
//! ready, handed over or rejected what, and when.
//!
//! `campus_ops.canteen_orders.handled_by` only keeps the last account that
//! moved an order, so it cannot say who did the work on a mixed order whose
//! items are swiped and scanned by different people. Every path that changes
//! an order's status writes rows here inside its own transaction:
//!
//! * one row per item that reached a new step (`line_index` = the item's
//!   position in the order's lines, `action` = the step it reached, with
//!   `completed` written as `delivered`), and
//! * one whole-order row (`line_index` NULL) when an order is accepted,
//!   rejected (amount = the refunded total) or cancelled.
//!
//! An item that skips steps (a whole order marked delivered from pending)
//! records only the step it reached, so no preparation time is invented for
//! it. See `shop_analytics` for how the owner's figures read these rows.

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::ApiResult,
    operations::{effective_line_status, instant_item_ids, line_is_instant, line_status_rank},
};

/// Who moved an order, and how.
pub(crate) struct EventContext<'a> {
    pub tenant: Uuid,
    pub order_id: Uuid,
    /// The order's shop, as the status paths resolve it.
    pub shop_key: &'a str,
    pub actor_id: &'a str,
    pub actor_name: &'a str,
    /// `item` (one item swiped), `order` (the whole order) or `scan`.
    pub source: &'a str,
}

/// One row to write.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OrderEvent {
    pub line_index: Option<usize>,
    pub item_id: Option<String>,
    pub item_name: Option<String>,
    pub action: String,
    pub quantity: i64,
    pub amount: f64,
    pub instant: bool,
}

/// A line's quantity as the order recorded it (at least one).
pub(crate) fn line_quantity(line: &Value) -> i64 {
    line.get("quantity")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|q| q.round() as i64))
                .or_else(|| value.as_str().and_then(|q| q.trim().parse().ok()))
        })
        .filter(|quantity| *quantity > 0)
        .unwrap_or(1)
}

/// What the customer paid for a line: its recorded price times quantity.
pub(crate) fn line_amount(line: &Value) -> f64 {
    let price = line
        .get("price")
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|p| p.trim().parse().ok()))
        })
        .unwrap_or(0.0);
    price * line_quantity(line) as f64
}

pub(crate) fn line_item_id(line: &Value) -> Option<String> {
    line.get("itemId")
        .or_else(|| line.get("item_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The action a line status stands for in the log.
fn action_for(status: &str) -> &str {
    if status == "completed" {
        "delivered"
    } else {
        status
    }
}

/// The row for item `index` reaching `status`.
pub(crate) fn item_event(index: usize, line: &Value, status: &str, instant: bool) -> OrderEvent {
    OrderEvent {
        line_index: Some(index),
        item_id: line_item_id(line),
        item_name: line.get("name").and_then(Value::as_str).map(str::to_owned),
        action: action_for(status).to_owned(),
        quantity: line_quantity(line),
        amount: line_amount(line),
        instant,
    }
}

fn whole_order_event(action: &str, lines: &[Value], amount: f64) -> OrderEvent {
    OrderEvent {
        line_index: None,
        item_id: None,
        item_name: None,
        action: action.to_owned(),
        quantity: lines.iter().map(line_quantity).sum(),
        amount,
        instant: false,
    }
}

/// The rows an order moving from `previous` (with `lines_before`) to `next`
/// (with `lines_after`) writes: the whole-order acceptance, rejection or
/// cancellation, or one row per item whose own step moved forward.
pub(crate) fn transition_events(
    previous: &str,
    lines_before: &[Value],
    next: &str,
    lines_after: &[Value],
    instant: &[bool],
    order_total: f64,
) -> Vec<OrderEvent> {
    if matches!(next, "rejected" | "cancelled") {
        return if previous == next {
            Vec::new()
        } else {
            vec![whole_order_event(next, lines_before, order_total)]
        };
    }
    let mut events = Vec::new();
    if next == "accepted" && previous == "pending" {
        events.push(whole_order_event("accepted", lines_before, 0.0));
    }
    for (index, before_line) in lines_before.iter().enumerate() {
        let is_instant = instant.get(index).copied().unwrap_or(false);
        let after_line = lines_after.get(index).unwrap_or(before_line);
        let before = effective_line_status(before_line, previous, is_instant);
        // A delivered order has delivered every item, whatever an item last
        // said on its own.
        let after = if next == "completed" {
            "completed".to_owned()
        } else {
            effective_line_status(after_line, next, is_instant)
        };
        if line_status_rank(&after) > line_status_rank(&before) {
            events.push(item_event(index, after_line, &after, is_instant));
        }
    }
    events
}

/// Writes the rows for an order moving from `previous` to `next`. Runs in
/// the transaction that moved the order, which holds its row.
pub(crate) async fn record_transition(
    tx: &mut Transaction<'_, Postgres>,
    context: &EventContext<'_>,
    previous: &str,
    lines_before: &Value,
    next: &str,
    lines_after: Option<&Value>,
    order_total: f64,
) -> ApiResult<()> {
    let before = lines_before.as_array().cloned().unwrap_or_default();
    let after = lines_after
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| before.clone());
    let instant_ids = instant_item_ids(tx, context.tenant, &before).await?;
    let instant: Vec<bool> = before
        .iter()
        .map(|line| line_is_instant(line, &instant_ids))
        .collect();
    let events = transition_events(previous, &before, next, &after, &instant, order_total);
    insert_events(tx, context, &events).await
}

/// Writes `events` for one order.
pub(crate) async fn insert_events(
    tx: &mut Transaction<'_, Postgres>,
    context: &EventContext<'_>,
    events: &[OrderEvent],
) -> ApiResult<()> {
    for event in events {
        sqlx::query(
            r#"INSERT INTO campus_ops.canteen_order_events
                 (tenant_id,order_id,line_index,item_id,item_name,action,source,
                  actor_user_id,actor_name,shop_key,quantity,amount,instant)
               VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)"#,
        )
        .bind(context.tenant)
        .bind(context.order_id)
        .bind(event.line_index.map(|index| index as i32))
        .bind(&event.item_id)
        .bind(&event.item_name)
        .bind(&event.action)
        .bind(context.source)
        .bind(context.actor_id)
        .bind(context.actor_name)
        .bind(context.shop_key)
        .bind(event.quantity as i32)
        .bind(event.amount)
        .bind(event.instant)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Mirrors migrations/runtime/0150_canteen_order_events.sql for databases the
/// migration runner has not reached. Runs the DDL at most once per database
/// per process, and only while the table is missing.
pub(crate) async fn ensure_order_event_schema(pool: &sqlx::PgPool) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static READY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let ready = READY.get_or_init(|| Mutex::new(HashSet::new()));
    let options = pool.connect_options();
    let key = format!(
        "{}:{}/{}",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or_default()
    );
    if ready.lock().map(|set| set.contains(&key)).unwrap_or(false) {
        return;
    }
    let present = sqlx::query_scalar::<_, bool>(
        r#"SELECT to_regclass('campus_ops.canteen_orders') IS NULL
               OR to_regclass('campus_ops.canteen_order_events') IS NOT NULL"#,
    )
    .fetch_one(pool)
    .await;
    let done = match present {
        Ok(true) => true,
        Ok(false) => sqlx::raw_sql(include_str!(
            "../../../migrations/runtime/0150_canteen_order_events.sql"
        ))
        .execute(pool)
        .await
        .map_err(|error| tracing::warn!(%error, "canteen order event log unavailable"))
        .is_ok(),
        Err(_) => false,
    };
    if done && let Ok(mut set) = ready.lock() {
        set.insert(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines() -> Vec<Value> {
        vec![
            json!({"itemId": "dosa", "name": "Dosa", "price": 40, "quantity": 2}),
            json!({"itemId": "chips", "name": "Chips", "price": 10, "quantity": 1}),
        ]
    }

    fn actions(events: &[OrderEvent]) -> Vec<(Option<usize>, &str)> {
        events
            .iter()
            .map(|e| (e.line_index, e.action.as_str()))
            .collect()
    }

    #[test]
    fn line_amounts_are_price_times_quantity() {
        let lines = lines();
        assert_eq!(line_amount(&lines[0]), 80.0);
        assert_eq!(line_quantity(&lines[1]), 1);
        assert_eq!(line_quantity(&json!({"price": 5})), 1);
        assert_eq!(
            line_amount(&json!({"price": "12.5", "quantity": "2"})),
            25.0
        );
    }

    #[test]
    fn whole_order_delivery_records_each_item_that_moved() {
        let before = lines();
        let mut after = lines();
        // Chips were already handed over on their own.
        after[1]["status"] = json!("completed");
        let mut pinned = lines();
        pinned[1]["status"] = json!("completed");
        let events = transition_events("ready", &pinned, "completed", &after, &[false, true], 90.0);
        assert_eq!(actions(&events), [(Some(0), "delivered")]);
        assert_eq!(events[0].amount, 80.0);
        assert_eq!(events[0].quantity, 2);
        assert_eq!(events[0].item_name.as_deref(), Some("Dosa"));
        // From pending, both items record only the step they reached.
        let events = transition_events(
            "pending",
            &before,
            "completed",
            &before,
            &[false, true],
            90.0,
        );
        assert_eq!(
            actions(&events),
            [(Some(0), "delivered"), (Some(1), "delivered")]
        );
        assert!(events[1].instant);
    }

    #[test]
    fn whole_order_steps_skip_instant_items() {
        let lines = lines();
        let events = transition_events(
            "accepted",
            &lines,
            "preparing",
            &lines,
            &[false, true],
            90.0,
        );
        assert_eq!(actions(&events), [(Some(0), "preparing")]);
        let events = transition_events("pending", &lines, "accepted", &lines, &[false, true], 90.0);
        assert_eq!(actions(&events), [(None, "accepted")]);
        assert_eq!(events[0].quantity, 3);
        // Moving backwards records nothing.
        let events = transition_events("ready", &lines, "preparing", &lines, &[false, false], 90.0);
        assert!(events.is_empty());
    }

    #[test]
    fn rejection_is_one_whole_order_row_with_the_refund() {
        let lines = lines();
        let events = transition_events(
            "preparing",
            &lines,
            "rejected",
            &lines,
            &[false, true],
            90.0,
        );
        assert_eq!(actions(&events), [(None, "rejected")]);
        assert_eq!(events[0].amount, 90.0);
        assert_eq!(events[0].quantity, 3);
        assert!(transition_events("rejected", &lines, "rejected", &lines, &[], 90.0).is_empty());
    }

    #[test]
    fn a_scan_records_the_step_each_item_reached() {
        let before = lines();
        let mut after = lines();
        after[0]["status"] = json!("preparing");
        after[1]["status"] = json!("completed");
        let events = transition_events(
            "pending",
            &before,
            "preparing",
            &after,
            &[false, true],
            90.0,
        );
        assert_eq!(
            actions(&events),
            [(Some(0), "preparing"), (Some(1), "delivered")]
        );
    }
}

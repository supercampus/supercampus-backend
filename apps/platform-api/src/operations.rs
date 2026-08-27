use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    realtime::RealtimePublication,
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/changes", get(changes))
        .route("/notifications", get(notifications))
        .route("/canteen/store", get(canteen_store))
        .route("/canteen/shops", get(list_shops).post(create_shop))
        .route(
            "/canteen/shops/{shop_id}",
            put(update_shop).delete(delete_shop),
        )
        .route("/canteen/menu", post(create_menu_item))
        .route(
            "/canteen/menu/{item_id}",
            put(update_menu_item).delete(delete_menu_item),
        )
        .route("/canteen/orders", post(place_order))
        .route(
            "/canteen/orders/{order_id}/status",
            put(update_order_status),
        )
        .route("/canteen/orders/scan", post(scan_order))
        .route("/canteen/wallets", get(wallet_directory))
        .route("/canteen/wallet-transactions", get(wallet_transactions))
        .route("/canteen/wallets/{user_id}/top-ups", post(top_up_wallet))
        .route("/canteen/staff-state", put(update_canteen_staff_state))
        .route("/gatepass/overview", get(gatepass_overview))
        .route("/gatepass/requests", post(create_gatepass_request))
        .route(
            "/gatepass/requests/{request_id}",
            axum::routing::delete(cancel_gatepass_request),
        )
        .route(
            "/gatepass/requests/{request_id}/decision",
            post(decide_gatepass_request),
        )
        .route("/gatepass/daily-access", post(activate_daily_access))
        .route("/campuses", get(list_campuses).post(create_campus))
        .route("/campuses/{campus_id}/geofence", put(set_campus_geofence))
        .route("/gatepass/scan", post(scan_gatepass))
        .route(
            "/gatepass/visitors",
            get(crate::visitors::list_visitor_passes).post(crate::visitors::create_visitor_pass),
        )
        .route(
            "/gatepass/visitors/{pass_id}/decision",
            post(crate::visitors::decide_visitor_pass),
        )
        .route("/attendance/roster", get(attendance_roster))
        .route("/attendance/classes", get(attendance_classes))
        .route("/student/assessments", get(student_assessments))
        .route("/advisor/students", get(advisor_students))
        .route(
            "/advisor/students/{student_id}/assessments",
            get(advisor_student_assessments).post(create_advisor_student_assessment),
        )
        .route(
            "/advisor/students/{student_id}/assessments/{assessment_id}",
            put(update_advisor_student_assessment),
        )
        .route("/attendance/wards", get(attendance_wards))
        .route("/attendance/summary/{student_id}", get(attendance_summary))
        .route(
            "/attendance/sessions",
            get(attendance_sessions).post(create_attendance_session),
        )
        .route(
            "/attendance/sessions/{session_id}/entries",
            put(replace_attendance_entries),
        )
        .route(
            "/attendance/sessions/{session_id}/publish",
            post(publish_attendance_session),
        )
        .route(
            "/attendance/reports",
            get(attendance_reports).post(create_attendance_report),
        )
        .route(
            "/attendance/reports/{report_id}/submit",
            post(submit_attendance_report),
        )
}

// ---------------------------------------------------------------- campuses

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CampusGeofenceRequest {
    /// Null clears the fence and permits activation from any location.
    geofence: Option<CampusGeofence>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CampusGeofence {
    latitude: f64,
    longitude: f64,
    radius_metres: f64,
}

const MIN_GEOFENCE_RADIUS_METRES: f64 = 50.0;
const MAX_GEOFENCE_RADIUS_METRES: f64 = 20_000.0;

async fn list_campuses(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &["platform.configuration.read", "timetable.config.read"],
    )?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let rows = sqlx::query_scalar::<_, Value>(
        r#"SELECT jsonb_build_object(
                     'id', id,
                     'code', code,
                     'name', name,
                     'geofence', metadata -> 'geofence')
           FROM core.campuses
           WHERE tenant_id = $1 AND active
           ORDER BY name"#,
    )
    .bind(tenant)
    .fetch_all(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({ "campuses": rows }))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateCampusRequest {
    name: String,
    code: Option<String>,
}

async fn create_campus(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<CreateCampusRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_any(
        &access,
        &["platform.configuration.update", "timetable.config.update"],
    )?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("A campus name is required".into()));
    }
    let code = match input.code.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_uppercase(),
        _ => campus_code_from(name),
    };

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let created = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO core.campuses (tenant_id, code, name)
           VALUES ($1, $2, $3)
           ON CONFLICT (tenant_id, code) DO NOTHING
           RETURNING jsonb_build_object(
                       'id', id, 'code', code, 'name', name,
                       'geofence', metadata -> 'geofence')"#,
    )
    .bind(tenant)
    .bind(&code)
    .bind(name)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::Conflict(format!("A campus with code {code} already exists")))?;

    Ok((StatusCode::CREATED, Json(ApiResponse::new(created))))
}

fn campus_code_from(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect();
    let joined = slug
        .split('-')
        .filter(|part| !part.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join("-");
    if joined.is_empty() {
        "CAMPUS".into()
    } else {
        joined.chars().take(24).collect()
    }
}

async fn set_campus_geofence(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(campus_id): Path<Uuid>,
    Json(input): Json<CampusGeofenceRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &["platform.configuration.update", "timetable.config.update"],
    )?;

    let patch = match input.geofence {
        None => Value::Null,
        Some(fence) => {
            if !(-90.0..=90.0).contains(&fence.latitude)
                || !(-180.0..=180.0).contains(&fence.longitude)
            {
                return Err(ApiError::BadRequest(
                    "That is not a valid campus location".into(),
                ));
            }
            if !(MIN_GEOFENCE_RADIUS_METRES..=MAX_GEOFENCE_RADIUS_METRES)
                .contains(&fence.radius_metres)
            {
                return Err(ApiError::BadRequest(format!(
                    "Radius must be between {MIN_GEOFENCE_RADIUS_METRES:.0} and \
                     {MAX_GEOFENCE_RADIUS_METRES:.0} metres"
                )));
            }
            json!({
                "latitude": fence.latitude,
                "longitude": fence.longitude,
                "radiusMetres": fence.radius_metres,
            })
        }
    };

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let updated = sqlx::query_scalar::<_, Value>(
        r#"UPDATE core.campuses
           SET metadata = CASE
                            WHEN $3::jsonb IS NULL OR $3::jsonb = 'null'::jsonb
                              THEN COALESCE(metadata, '{}'::jsonb) - 'geofence'
                            ELSE jsonb_set(
                                   COALESCE(metadata, '{}'::jsonb),
                                   '{geofence}', $3::jsonb, true)
                          END
           WHERE tenant_id = $1 AND id = $2 AND active
           RETURNING jsonb_build_object(
                       'id', id,
                       'code', code,
                       'name', name,
                       'geofence', metadata -> 'geofence')"#,
    )
    .bind(tenant)
    .bind(campus_id)
    .bind(&patch)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound("Campus not found".into()))?;

    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "gatepass",
        "campus_geofence",
        &campus_id.to_string(),
        "campus_geofence.updated",
        &principal.student.id,
        &json!({ "geofence": patch }),
    )
    .await?;

    Ok(Json(ApiResponse::new(updated)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeQuery {
    after: Option<i64>,
    limit: Option<i64>,
}

async fn changes(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<ChangeQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let modules = authorized_change_modules(&access);
    if modules.is_empty() {
        return Ok(Json(ApiResponse::new(json!({"changes": []}))));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let rows = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT COALESCE(jsonb_agg(item ORDER BY (item->>'sequence')::bigint), '[]'::jsonb)
        FROM (
          SELECT jsonb_build_object('sequence', sequence, 'module', module_key,
            'eventType', event_type, 'createdAt', created_at) item
          FROM campus_ops.events
          WHERE tenant_id = $1 AND sequence > $2 AND module_key = ANY($3)
          ORDER BY sequence LIMIT $4
        ) feed"#,
    )
    .bind(tenant)
    .bind(query.after.unwrap_or(0))
    .bind(&modules)
    .bind(query.limit.unwrap_or(100).clamp(1, 250))
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({"changes": rows}))))
}

async fn notifications(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let rows = sqlx::query_scalar::<_, Value>(
        r#"
      SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'category', category,
        'title', title, 'body', body, 'data', data, 'readAt', read_at,
        'createdAt', created_at) ORDER BY created_at DESC), '[]'::jsonb)
      FROM campus_ops.notifications
      WHERE tenant_id=$1 AND (recipient_user_id=$2 OR recipient_role = ANY($3))
      LIMIT 100"#,
    )
    .bind(tenant)
    .bind(&principal.student.id)
    .bind(&principal.roles)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({"notifications": rows}))))
}

async fn canteen_store(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &[
            "canteen.menu.read",
            "canteen.order.read",
            "canteen.orders.manage",
            "canteen.wallet.top_up",
            "vendor_management.vendors.read",
        ],
    )?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let assigned_shop_keys = assigned_shop_keys(db.pool(), tenant, &principal.student.id).await?;
    let configures_shops = access.allows("vendor_management.vendors.update");
    let is_vendor_operator = access.allows("canteen.orders.manage")
        || access.allows("canteen.menu.create")
        || access.allows("canteen.menu.update")
        || access.allows("canteen.menu.delete");
    let restrict_to_assignments = is_vendor_operator && !configures_shops;
    let can_manage = access.allows("canteen.orders.manage")
        && (configures_shops || !assigned_shop_keys.is_empty());
    let can_read_analytics = access.allows("canteen.analytics.read")
        && (configures_shops || !restrict_to_assignments || !assigned_shop_keys.is_empty());
    sqlx::query("INSERT INTO campus_ops.canteen_wallets (tenant_id,user_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
        .bind(tenant).bind(&principal.student.id).execute(db.pool()).await?;
    let mut data = sqlx::query_scalar::<_, Value>(r#"
      SELECT jsonb_build_object(
        'user', jsonb_build_object('id',$2::text,'name',$3::text,'email',$4::text,
          'rollNumber',$5::text,'department',$6::text),
        'walletBalance', COALESCE((SELECT balance::float8 FROM campus_ops.canteen_wallets WHERE tenant_id=$1 AND user_id=$2),0),
        'menu', COALESCE((SELECT jsonb_agg(jsonb_build_object('id',item.id,'name',item.name,
          'description',item.description,'store',resolved_shop.shop_key,'category',item.category,
          'price',item.price::float8,'prepMinutes',item.prep_minutes,
          'isVegetarian',item.is_vegetarian,'isPopular',item.is_popular,
          'isAvailable',item.is_available,'isInstant',item.is_instant,'imageUrl',item.image_url)
          ORDER BY resolved_shop.shop_key,item.category,item.name)
          FROM campus_ops.canteen_menu_items item
          JOIN LATERAL (
            SELECT shop.shop_key
            FROM campus_ops.shops shop
            WHERE shop.tenant_id=item.tenant_id AND shop.is_active
              AND (shop.shop_key=item.store
                OR (item.store IN ('classic','bites') AND lower(shop.category)='canteen')
                OR (item.store='stationery' AND lower(shop.category)='stationery'))
            ORDER BY CASE WHEN shop.shop_key=item.store THEN 0 ELSE 1 END,
              shop.created_at, shop.shop_key
            LIMIT 1
          ) resolved_shop ON true
          WHERE item.tenant_id=$1
            AND (NOT $9 OR resolved_shop.shop_key = ANY($10))), '[]'::jsonb),
        'orders', COALESCE((SELECT jsonb_agg(jsonb_build_object('id',id,'orderNumber',order_number,
          'customerUserId',customer_user_id,'customerName',customer_name,'lines',lines,
          'total',total::float8,'fulfilmentMode',fulfilment_mode,'status',status,
          'tokenNumber',token_number,'qrPayload',id::text,'createdAt',created_at,'updatedAt',updated_at)
          ORDER BY created_at DESC) FROM campus_ops.canteen_orders
          WHERE tenant_id=$1 AND (($7 AND (NOT $9 OR store = ANY($10))) OR customer_user_id=$2)), '[]'::jsonb),
        'walletTransactions', COALESCE((SELECT jsonb_agg(jsonb_build_object('id',id,
          'amount',amount::float8,'transactionType',transaction_type,'description',description,
          'referenceId',reference_id,'createdAt',created_at) ORDER BY created_at DESC)
          FROM campus_ops.canteen_wallet_transactions WHERE tenant_id=$1 AND user_id=$2), '[]'::jsonb),
        'staffState', COALESCE((SELECT jsonb_build_object('mode',mode,'shopOpen',shop_open)
          FROM campus_ops.canteen_staff_state WHERE tenant_id=$1 AND user_id=$2),
          jsonb_build_object('mode','eat','shopOpen',null)),
        'canManage', $7::boolean,
        'analytics', CASE WHEN $8 THEN jsonb_build_object(
          'ordersToday',(SELECT count(*) FROM campus_ops.canteen_orders WHERE tenant_id=$1 AND (NOT $9 OR store = ANY($10)) AND created_at::date=CURRENT_DATE),
          'revenueToday',COALESCE((SELECT sum(total)::float8 FROM campus_ops.canteen_orders WHERE tenant_id=$1 AND (NOT $9 OR store = ANY($10)) AND status='completed' AND created_at::date=CURRENT_DATE),0),
          'pending',(SELECT count(*) FROM campus_ops.canteen_orders WHERE tenant_id=$1 AND (NOT $9 OR store = ANY($10)) AND status IN ('pending','accepted','preparing','ready'))
        ) ELSE null END
      )"#)
      .bind(tenant).bind(&principal.student.id).bind(&principal.student.name)
      .bind(&principal.student.email).bind(&principal.student.roll).bind(&principal.student.dept)
      .bind(can_manage).bind(can_read_analytics).bind(restrict_to_assignments).bind(&assigned_shop_keys)
      .fetch_one(db.pool()).await?;
    let shops = shops_json(
        db.pool(),
        tenant,
        false,
        restrict_to_assignments.then_some(&assigned_shop_keys),
    )
    .await?;
    if let Some(object) = data.as_object_mut() {
        object.insert("shops".into(), shops);
        object.insert("assignedShopKeys".into(), json!(assigned_shop_keys));
        object.insert(
            "capabilities".into(),
            json!({
                "readShops": access.allows("vendor_management.vendors.read"),
                "createShops": access.allows("vendor_management.vendors.create"),
                "updateShops": access.allows("vendor_management.vendors.update"),
                "deleteShops": access.allows("vendor_management.vendors.delete"),
                "readMenu": access.allows("canteen.menu.read"),
                "createMenu": access.allows("canteen.menu.create"),
                "updateMenu": access.allows("canteen.menu.update"),
                "deleteMenu": access.allows("canteen.menu.delete"),
                "manageOrders": can_manage,
                "readAnalytics": can_read_analytics,
                "topUpWallets": access.allows("canteen.wallet.top_up")
            }),
        );
    }
    Ok(Json(ApiResponse::new(data)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShopRequest {
    shop_key: String,
    name: String,
    #[serde(default = "default_shop_category")]
    category: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_true")]
    is_active: bool,
    #[serde(default)]
    meal_compliance: bool,
    #[serde(default = "default_true")]
    qr_payments: bool,
    /// Omitted preserves assignments; an empty list clears them.
    operators: Option<Vec<ShopOperatorRequest>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShopOperatorRequest {
    user_id: String,
    #[serde(default = "default_shop_operator_role")]
    assignment_role: String,
}

fn default_shop_operator_role() -> String {
    "owner".into()
}

fn default_shop_category() -> String {
    "Canteen".into()
}

async fn list_shops(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "vendor_management.vendors.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    Ok(Json(ApiResponse::new(json!({
        "shops": shops_json(db.pool(), tenant, true, None).await?
    }))))
}

async fn create_shop(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<ShopRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "vendor_management.vendors.create")?;
    validate_shop(&input)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let shop_id = Uuid::new_v4();
    let shop = sqlx::query_scalar::<_, Value>(r#"
      INSERT INTO campus_ops.shops
        (id,tenant_id,shop_key,name,category,description,is_active,meal_compliance,qr_payments,created_by)
      VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
      RETURNING jsonb_build_object('id',id,'shopKey',shop_key,'name',name,'category',category,
        'description',description,'isActive',is_active,'mealCompliance',meal_compliance,
        'qrPayments',qr_payments,'createdAt',created_at,'updatedAt',updated_at)"#)
      .bind(shop_id).bind(tenant).bind(input.shop_key.trim()).bind(input.name.trim())
      .bind(input.category.trim()).bind(input.description.trim()).bind(input.is_active)
      .bind(input.meal_compliance).bind(input.qr_payments).bind(&principal.student.id)
      .fetch_one(&mut *tx).await?;
    sync_shop_operators(
        &mut tx,
        tenant,
        shop_id,
        input.operators.as_deref(),
        &principal.student.id,
    )
    .await?;
    tx.commit().await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "vendor_management",
        "shop",
        shop["id"].as_str().unwrap_or_default(),
        "shop.created",
        &principal.student.id,
        &shop,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(shop))))
}

async fn update_shop(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(shop_id): Path<Uuid>,
    Json(input): Json<ShopRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "vendor_management.vendors.update")?;
    validate_shop(&input)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let shop = sqlx::query_scalar::<_, Value>(
        r#"
      UPDATE campus_ops.shops SET shop_key=$3,name=$4,category=$5,description=$6,
        is_active=$7,meal_compliance=$8,qr_payments=$9,updated_at=now()
      WHERE tenant_id=$1 AND id=$2
      RETURNING jsonb_build_object('id',id,'shopKey',shop_key,'name',name,'category',category,
        'description',description,'isActive',is_active,'mealCompliance',meal_compliance,
        'qrPayments',qr_payments,'createdAt',created_at,'updatedAt',updated_at)"#,
    )
    .bind(tenant)
    .bind(shop_id)
    .bind(input.shop_key.trim())
    .bind(input.name.trim())
    .bind(input.category.trim())
    .bind(input.description.trim())
    .bind(input.is_active)
    .bind(input.meal_compliance)
    .bind(input.qr_payments)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Shop not found".into()))?;
    sync_shop_operators(
        &mut tx,
        tenant,
        shop_id,
        input.operators.as_deref(),
        &principal.student.id,
    )
    .await?;
    tx.commit().await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "vendor_management",
        "shop",
        &shop_id.to_string(),
        "shop.updated",
        &principal.student.id,
        &shop,
    )
    .await?;
    Ok(Json(ApiResponse::new(shop)))
}

async fn delete_shop(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(shop_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    require(&access, "vendor_management.vendors.delete")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let shop_key = sqlx::query_scalar::<_, String>(
        "UPDATE campus_ops.shops SET is_active=false,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND is_active RETURNING shop_key",
    )
    .bind(tenant).bind(shop_id).fetch_optional(db.pool()).await?
    .ok_or_else(|| ApiError::NotFound("Active shop not found".into()))?;
    sqlx::query("UPDATE campus_ops.canteen_menu_items SET is_available=false,updated_at=now() WHERE tenant_id=$1 AND store=$2")
        .bind(tenant).bind(&shop_key).execute(db.pool()).await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "vendor_management",
        "shop",
        &shop_id.to_string(),
        "shop.deactivated",
        &principal.student.id,
        &json!({"shopKey": shop_key}),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_shop(input: &ShopRequest) -> ApiResult<()> {
    let key = input.shop_key.trim();
    if input.name.trim().is_empty()
        || input.category.trim().is_empty()
        || !(2..=64).contains(&key.len())
        || !key.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
        || !key
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    {
        return Err(ApiError::BadRequest(
            "Enter a valid shop name, category and lowercase shop key".into(),
        ));
    }
    if input.operators.as_ref().is_some_and(|operators| {
        operators.iter().any(|operator| {
            operator.user_id.trim().is_empty()
                || !matches!(operator.assignment_role.as_str(), "owner" | "captain")
        })
    }) {
        return Err(ApiError::BadRequest("Choose valid shop operators".into()));
    }
    Ok(())
}

async fn shops_json(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    include_inactive: bool,
    keys: Option<&Vec<String>>,
) -> ApiResult<Value> {
    let restrict = keys.is_some();
    let keys = keys.cloned().unwrap_or_default();
    Ok(sqlx::query_scalar::<_, Value>(
        r#"
      SELECT COALESCE(jsonb_agg(jsonb_build_object('id',shop.id,'shopKey',shop.shop_key,'name',shop.name,
        'category',shop.category,'description',shop.description,'isActive',shop.is_active,
        'mealCompliance',shop.meal_compliance,'qrPayments',shop.qr_payments,'createdAt',shop.created_at,
        'updatedAt',shop.updated_at,'operators',COALESCE((SELECT jsonb_agg(jsonb_build_object(
          'userId',assignment.user_id,'assignmentRole',assignment.assignment_role) ORDER BY assignment.assignment_role,assignment.user_id)
          FROM campus_ops.shop_user_assignments assignment WHERE assignment.tenant_id=shop.tenant_id
            AND assignment.shop_id=shop.id AND assignment.is_active),'[]'::jsonb)) ORDER BY shop.name), '[]'::jsonb)
      FROM campus_ops.shops shop WHERE shop.tenant_id=$1 AND ($2 OR shop.is_active)
        AND (NOT $3 OR shop.shop_key = ANY($4))"#,
    )
    .bind(tenant)
    .bind(include_inactive)
    .bind(restrict)
    .bind(keys)
    .fetch_one(pool)
    .await?)
}

async fn assigned_shop_keys(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    user_id: &str,
) -> ApiResult<Vec<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        r#"SELECT shop.shop_key FROM campus_ops.shop_user_assignments assignment
           JOIN campus_ops.shops shop ON shop.tenant_id=assignment.tenant_id AND shop.id=assignment.shop_id
           WHERE assignment.tenant_id=$1 AND assignment.user_id=$2
             AND assignment.is_active AND shop.is_active ORDER BY shop.name"#,
    )
    .bind(tenant)
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

async fn sync_shop_operators(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    shop_id: Uuid,
    operators: Option<&[ShopOperatorRequest]>,
    actor_user_id: &str,
) -> ApiResult<()> {
    let Some(operators) = operators else {
        return Ok(());
    };
    sqlx::query("UPDATE campus_ops.shop_user_assignments SET is_active=false,updated_at=now() WHERE tenant_id=$1 AND shop_id=$2")
        .bind(tenant).bind(shop_id).execute(&mut **tx).await?;
    for operator in operators {
        sqlx::query(r#"INSERT INTO campus_ops.shop_user_assignments
          (tenant_id,shop_id,user_id,assignment_role,is_active,assigned_by)
          VALUES($1,$2,$3,$4,true,$5)
          ON CONFLICT(tenant_id,shop_id,user_id) DO UPDATE SET assignment_role=EXCLUDED.assignment_role,
            is_active=true,assigned_by=EXCLUDED.assigned_by,updated_at=now()"#)
            .bind(tenant).bind(shop_id).bind(operator.user_id.trim())
            .bind(&operator.assignment_role).bind(actor_user_id)
            .execute(&mut **tx).await?;
    }
    Ok(())
}

async fn require_assigned_shop(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    user_id: &str,
    shop_key: &str,
    access: &EffectiveAccess,
) -> ApiResult<()> {
    if access.allows("vendor_management.vendors.update") {
        return Ok(());
    }
    let assigned = sqlx::query_scalar::<_, bool>(r#"SELECT EXISTS(
      SELECT 1 FROM campus_ops.shop_user_assignments assignment
      JOIN campus_ops.shops shop ON shop.tenant_id=assignment.tenant_id AND shop.id=assignment.shop_id
      WHERE assignment.tenant_id=$1 AND assignment.user_id=$2 AND shop.shop_key=$3
        AND assignment.is_active AND shop.is_active)"#)
        .bind(tenant).bind(user_id).bind(shop_key).fetch_one(pool).await?;
    if assigned {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MenuItemRequest {
    name: String,
    #[serde(default)]
    description: String,
    /// Tenant-configured shop key the item belongs to.
    #[serde(default = "default_store")]
    store: String,
    category: String,
    price: f64,
    #[serde(default = "default_prep_minutes")]
    prep_minutes: i32,
    #[serde(default = "default_true")]
    is_vegetarian: bool,
    #[serde(default)]
    is_popular: bool,
    #[serde(default = "default_true")]
    is_available: bool,
    image_url: Option<String>,
    /// Served straight from the counter; the menu badges these.
    #[serde(default)]
    is_instant: bool,
}
fn default_store() -> String {
    "classic".into()
}
fn default_prep_minutes() -> i32 {
    10
}
fn default_true() -> bool {
    true
}

async fn create_menu_item(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<MenuItemRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "canteen.menu.create")?;
    validate_menu(&input)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    ensure_active_shop(db.pool(), tenant, input.store.trim()).await?;
    require_assigned_shop(
        db.pool(),
        tenant,
        &principal.student.id,
        input.store.trim(),
        &access,
    )
    .await?;
    let item = sqlx::query_scalar::<_, Value>(r#"
      INSERT INTO campus_ops.canteen_menu_items
       (tenant_id,name,description,store,category,price,prep_minutes,is_vegetarian,is_popular,is_available,is_instant,image_url,created_by)
      VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
      RETURNING jsonb_build_object('id',id,'name',name,'description',description,'store',store,
        'category',category,'price',price::float8,'prepMinutes',prep_minutes,'isVegetarian',is_vegetarian,
        'isPopular',is_popular,'isAvailable',is_available,'isInstant',is_instant,'imageUrl',image_url)"#)
      .bind(tenant).bind(input.name.trim()).bind(input.description.trim())
      .bind(input.store.trim()).bind(input.category.trim())
      .bind(input.price).bind(input.prep_minutes).bind(input.is_vegetarian).bind(input.is_popular)
      .bind(input.is_available).bind(input.is_instant).bind(input.image_url).bind(&principal.student.id)
      .fetch_one(db.pool()).await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "canteen",
        "menu_item",
        item["id"].as_str().unwrap_or_default(),
        "menu.created",
        &principal.student.id,
        &item,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(item))))
}

async fn update_menu_item(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(item_id): Path<Uuid>,
    Json(input): Json<MenuItemRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "canteen.menu.update")?;
    validate_menu(&input)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    ensure_active_shop(db.pool(), tenant, input.store.trim()).await?;
    require_assigned_shop(
        db.pool(),
        tenant,
        &principal.student.id,
        input.store.trim(),
        &access,
    )
    .await?;
    let item=sqlx::query_scalar::<_,Value>(r#"UPDATE campus_ops.canteen_menu_items SET name=$3,
      description=$4,store=$5,category=$6,price=$7,prep_minutes=$8,is_vegetarian=$9,is_popular=$10,
      is_available=$11,is_instant=$12,image_url=$13,updated_at=now() WHERE tenant_id=$1 AND id=$2
      RETURNING jsonb_build_object('id',id,'name',name,'description',description,'store',store,
      'category',category,'price',price::float8,'prepMinutes',prep_minutes,'isVegetarian',is_vegetarian,
      'isPopular',is_popular,'isAvailable',is_available,'isInstant',is_instant,'imageUrl',image_url)"#)
      .bind(tenant).bind(item_id).bind(input.name.trim()).bind(input.description.trim())
      .bind(input.store.trim()).bind(input.category.trim())
      .bind(input.price).bind(input.prep_minutes).bind(input.is_vegetarian)
      .bind(input.is_popular).bind(input.is_available).bind(input.is_instant).bind(input.image_url)
      .fetch_optional(db.pool()).await?.ok_or_else(||ApiError::NotFound("Menu item not found".into()))?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "canteen",
        "menu_item",
        &item_id.to_string(),
        "menu.updated",
        &principal.student.id,
        &item,
    )
    .await?;
    Ok(Json(ApiResponse::new(item)))
}

async fn delete_menu_item(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(item_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    require(&access, "canteen.menu.delete")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let store = sqlx::query_scalar::<_, String>(
        "SELECT store FROM campus_ops.canteen_menu_items WHERE tenant_id=$1 AND id=$2",
    )
    .bind(tenant)
    .bind(item_id)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound("Menu item not found".into()))?;
    require_assigned_shop(db.pool(), tenant, &principal.student.id, &store, &access).await?;
    let result =
        sqlx::query("DELETE FROM campus_ops.canteen_menu_items WHERE tenant_id=$1 AND id=$2")
            .bind(tenant)
            .bind(item_id)
            .execute(db.pool())
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Menu item not found".into()));
    }
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "canteen",
        "menu_item",
        &item_id.to_string(),
        "menu.deleted",
        &principal.student.id,
        &json!({}),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_menu(input: &MenuItemRequest) -> ApiResult<()> {
    if input.name.trim().is_empty()
        || input.price < 0.0
        || input.prep_minutes < 1
        || input.category.trim().is_empty()
        || input.store.trim().is_empty()
    {
        return Err(ApiError::BadRequest(
            "Enter a valid name, store, category, price and preparation time".into(),
        ));
    }
    Ok(())
}

async fn ensure_active_shop(pool: &sqlx::PgPool, tenant: Uuid, shop_key: &str) -> ApiResult<()> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM campus_ops.shops WHERE tenant_id=$1 AND shop_key=$2 AND is_active)",
    )
    .bind(tenant)
    .bind(shop_key)
    .fetch_one(pool)
    .await?;
    if exists {
        Ok(())
    } else {
        Err(ApiError::BadRequest("Choose an active tenant shop".into()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrderLineInput {
    item_id: Uuid,
    quantity: i32,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlaceOrderRequest {
    lines: Vec<OrderLineInput>,
    /// Optional: the app stopped asking. Every shop hands the order over at its
    /// own counter, so collection is the only mode that still means anything.
    fulfilment_mode: Option<String>,
    idempotency_key: Option<String>,
}

impl PlaceOrderRequest {
    fn fulfilment_mode(&self) -> &str {
        match self.fulfilment_mode.as_deref() {
            Some("dine_in") => "dine_in",
            _ => "pickup",
        }
    }
}

async fn place_order(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<PlaceOrderRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "canteen.order.create")?;
    if input.lines.is_empty()
        || input
            .lines
            .iter()
            .any(|line| line.quantity < 1 || line.quantity > 20)
    {
        return Err(ApiError::BadRequest("Choose valid available items".into()));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    sqlx::query("INSERT INTO campus_ops.canteen_wallets(tenant_id,user_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
      .bind(tenant).bind(&principal.student.id).execute(&mut *tx).await?;
    // A cart can hold items from more than one shop, but each shop hands its
    // food over at its own counter — so the cart becomes one order per shop,
    // each with its own QR. They are created in a single transaction: the
    // wallet must not be charged for one shop and not the other.
    let mut baskets: Vec<(String, Vec<Value>, f64)> = Vec::new();
    let mut grand_total = 0.0;
    for requested in &input.lines {
        // The line is a snapshot, so it carries everything history needs to stay
        // readable after the item is edited or removed — including whether it was
        // vegetarian, which the streak screens count.
        let item=sqlx::query_as::<_,(String,String,String,f64,bool)>("SELECT item.name,item.store,item.category,item.price::float8,item.is_vegetarian FROM campus_ops.canteen_menu_items item JOIN campus_ops.shops shop ON shop.tenant_id=item.tenant_id AND shop.shop_key=item.store AND shop.is_active WHERE item.tenant_id=$1 AND item.id=$2 AND item.is_available FOR SHARE OF item")
        .bind(tenant).bind(requested.item_id).fetch_optional(&mut *tx).await?
        .ok_or_else(||ApiError::BadRequest("An item is unavailable".into()))?;
        let line_total = item.3 * f64::from(requested.quantity);
        grand_total += line_total;
        let line = json!({"itemId":requested.item_id,"name":item.0,"store":item.1,"category":item.2,
            "price":item.3,"isVegetarian":item.4,"quantity":requested.quantity});
        match baskets.iter_mut().find(|(store, _, _)| *store == item.1) {
            Some(basket) => {
                basket.1.push(line);
                basket.2 += line_total;
            }
            None => baskets.push((item.1.clone(), vec![line], line_total)),
        }
    }

    // One wallet serves every shop, so the balance is checked once against the
    // whole cart rather than shop by shop.
    let balance=sqlx::query_scalar::<_,f64>("SELECT balance::float8 FROM campus_ops.canteen_wallets WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE")
      .bind(tenant).bind(&principal.student.id).fetch_one(&mut *tx).await?;
    if balance + 0.0001 < grand_total {
        return Err(ApiError::Conflict("Wallet balance is insufficient".into()));
    }

    let mut orders = Vec::new();
    let mut transactions = Vec::new();
    let mut new_balance = balance;
    for (store, store_lines, store_total) in baskets {
        let raw_qr = Uuid::new_v4().to_string();
        let hash = token_hash(&raw_qr);
        let order_id = Uuid::new_v4();
        // The basket is already one shop's worth, so the order records which
        // shop it is. Everything that shows a counter its own orders filters on
        // this, and it is the only place the value is known.
        let order=sqlx::query_scalar::<_,Value>(r#"INSERT INTO campus_ops.canteen_orders
          (id,tenant_id,customer_user_id,customer_name,lines,total,fulfilment_mode,qr_token_hash,store)
          VALUES($1,$2,$3,$4,$5,$6,$7,$8,$10) RETURNING jsonb_build_object('id',id,'orderNumber',order_number,
          'lines',lines,'total',total::float8,'fulfilmentMode',fulfilment_mode,'status',status,
          'qrPayload',$9::text,'createdAt',created_at)"#)
          .bind(order_id).bind(tenant).bind(&principal.student.id).bind(&principal.student.name)
          .bind(Value::Array(store_lines)).bind(store_total).bind(input.fulfilment_mode()).bind(hash).bind(&raw_qr)
          .bind(&store)
          .fetch_one(&mut *tx).await?;

        // Debiting per order keeps the ledger aligned with what can be refunded:
        // rejecting one shop's order returns exactly that order's money.
        new_balance=sqlx::query_scalar::<_,f64>("UPDATE campus_ops.canteen_wallets SET balance=balance-$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 RETURNING balance::float8")
          .bind(tenant).bind(&principal.student.id).bind(store_total).fetch_one(&mut *tx).await?;

        // The caller sends one key for the cart; each shop's debit needs its own
        // so the uniqueness guard does not collapse them into a single row.
        let idempotency_key = input
            .idempotency_key
            .as_ref()
            .map(|key| format!("{key}:{store}"));
        let transaction=sqlx::query_scalar::<_,Value>(r#"INSERT INTO campus_ops.canteen_wallet_transactions
          (tenant_id,user_id,amount,transaction_type,description,reference_id,idempotency_key,actor_user_id)
          VALUES($1,$2,$3,'order_debit',$6,$4,$5,$2)
          RETURNING jsonb_build_object('id',id,'amount',amount::float8,'transactionType',transaction_type,
          'description',description,'referenceId',reference_id,'createdAt',created_at)"#)
          .bind(tenant).bind(&principal.student.id).bind(-store_total).bind(order_id.to_string())
          .bind(idempotency_key).bind(format!("{} order", shop_label(&store)))
          .fetch_one(&mut *tx).await?;

        emit_tx(
            &mut tx,
            tenant,
            "canteen",
            "order",
            &order_id.to_string(),
            "order.created",
            &principal.student.id,
            &order,
        )
        .await?;
        orders.push(order);
        transactions.push(transaction);
    }
    tx.commit().await?;
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "canteen",
        "order",
        "batch",
        "order.created",
    );
    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::new(
            json!({"balance":new_balance,"orders":orders,"transactions":transactions}),
        )),
    ))
}

/// How a shop names itself on a wallet line.
fn shop_label(store: &str) -> String {
    match store {
        "classic" => "Campus Classic".into(),
        "bites" => "Quick Bites".into(),
        "stationery" => "Stationery Store".into(),
        _ => store
            .split(['_', '-'])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrderStatusRequest {
    status: String,
    reason: Option<String>,
}
async fn update_order_status(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(order_id): Path<Uuid>,
    Json(input): Json<OrderStatusRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "canteen.orders.manage")?;
    if !matches!(
        input.status.as_str(),
        "accepted" | "preparing" | "ready" | "completed" | "rejected"
    ) {
        return Err(ApiError::BadRequest("Invalid order status".into()));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let current = sqlx::query_as::<_, (String, String, f64, String)>(
        "SELECT status,customer_user_id,total::float8,store FROM campus_ops.canteen_orders WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(order_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Order not found".into()))?;
    require_assigned_shop(
        db.pool(),
        tenant,
        &principal.student.id,
        &current.3,
        &access,
    )
    .await?;
    let order=sqlx::query_scalar::<_,Value>(r#"UPDATE campus_ops.canteen_orders SET status=$3,handled_by=$4,rejection_reason=$5,
   token_number=CASE WHEN $3='accepted' AND token_number IS NULL THEN (order_number % 1000)::int ELSE token_number END,updated_at=now()
   WHERE tenant_id=$1 AND id=$2 RETURNING jsonb_build_object('id',id,'customerUserId',customer_user_id,'status',status,
   'tokenNumber',token_number,'updatedAt',updated_at)"#).bind(tenant).bind(order_id).bind(&input.status).bind(&principal.student.id).bind(input.reason)
   .fetch_one(&mut *tx).await?;
    if input.status == "rejected" && current.0 != "rejected" {
        sqlx::query("INSERT INTO campus_ops.canteen_wallets(tenant_id,user_id,balance,version) VALUES($1,$2,$3,1) ON CONFLICT(tenant_id,user_id) DO UPDATE SET balance=campus_ops.canteen_wallets.balance+EXCLUDED.balance,version=campus_ops.canteen_wallets.version+1,updated_at=now()")
            .bind(tenant).bind(&current.1).bind(current.2).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO campus_ops.canteen_wallet_transactions(tenant_id,user_id,amount,transaction_type,description,reference_id,idempotency_key,actor_user_id) VALUES($1,$2,$3,'refund','Rejected canteen order refund',$4,$5,$6) ON CONFLICT(tenant_id,idempotency_key) DO NOTHING")
            .bind(tenant).bind(&current.1).bind(current.2).bind(order_id.to_string())
            .bind(format!("order-refund-{order_id}")).bind(&principal.student.id).execute(&mut *tx).await?;
        notify_tx(
            &mut tx,
            tenant,
            Some(&current.1),
            None,
            "canteen",
            "Order rejected and refunded",
            &format!("{:.2} credits were returned to your wallet", current.2),
            &order,
        )
        .await?;
    } else {
        notify_tx(
            &mut tx,
            tenant,
            Some(&current.1),
            None,
            "canteen",
            "Order updated",
            &format!("Your canteen order is now {}", input.status),
            &order,
        )
        .await?;
    }
    emit_tx(
        &mut tx,
        tenant,
        "canteen",
        "order",
        &order_id.to_string(),
        &format!("order.{}", input.status),
        &principal.student.id,
        &order,
    )
    .await?;
    tx.commit().await?;
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "canteen",
        "order",
        &order_id.to_string(),
        &format!("order.{}", input.status),
    );
    Ok(Json(ApiResponse::new(order)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScanRequest {
    qr_payload: String,
    action: Option<String>,
}
async fn scan_order(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<ScanRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "canteen.orders.manage")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let desired = input.action.unwrap_or_else(|| "completed".into());
    if !matches!(desired.as_str(), "accepted" | "rejected" | "completed") {
        return Err(ApiError::BadRequest("Invalid scan action".into()));
    }
    let mut tx = db.pool().begin().await?;
    let order_id = Uuid::parse_str(input.qr_payload.trim()).ok();
    let current = sqlx::query_as::<_, (Uuid, String, String, f64, String)>(
        "SELECT id,status,customer_user_id,total::float8,store FROM campus_ops.canteen_orders WHERE tenant_id=$1 AND (qr_token_hash=$2 OR id=$3) FOR UPDATE",
    )
    .bind(tenant)
    .bind(token_hash(&input.qr_payload))
    .bind(order_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Order QR is invalid".into()))?;
    require_assigned_shop(
        db.pool(),
        tenant,
        &principal.student.id,
        &current.4,
        &access,
    )
    .await?;
    let value=sqlx::query_scalar::<_,Value>("UPDATE campus_ops.canteen_orders SET status=$3,handled_by=$4,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING jsonb_build_object('id',id,'status',status,'customerUserId',customer_user_id)")
 .bind(tenant).bind(current.0).bind(&desired).bind(&principal.student.id).fetch_one(&mut *tx).await?;
    if desired == "rejected" && current.1 != "rejected" {
        sqlx::query("INSERT INTO campus_ops.canteen_wallets(tenant_id,user_id,balance,version) VALUES($1,$2,$3,1) ON CONFLICT(tenant_id,user_id) DO UPDATE SET balance=campus_ops.canteen_wallets.balance+EXCLUDED.balance,version=campus_ops.canteen_wallets.version+1,updated_at=now()")
            .bind(tenant).bind(&current.2).bind(current.3).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO campus_ops.canteen_wallet_transactions(tenant_id,user_id,amount,transaction_type,description,reference_id,idempotency_key,actor_user_id) VALUES($1,$2,$3,'refund','Rejected canteen order refund',$4,$5,$6) ON CONFLICT(tenant_id,idempotency_key) DO NOTHING")
            .bind(tenant).bind(&current.2).bind(current.3).bind(current.0.to_string())
            .bind(format!("order-refund-{}", current.0)).bind(&principal.student.id).execute(&mut *tx).await?;
        notify_tx(
            &mut tx,
            tenant,
            Some(&current.2),
            None,
            "canteen",
            "Order rejected and refunded",
            &format!("{:.2} credits were returned to your wallet", current.3),
            &value,
        )
        .await?;
    }
    emit_tx(
        &mut tx,
        tenant,
        "canteen",
        "order",
        value["id"].as_str().unwrap_or_default(),
        "order.scanned",
        &principal.student.id,
        &value,
    )
    .await?;
    tx.commit().await?;
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "canteen",
        "order",
        value["id"].as_str().unwrap_or_default(),
        "order.scanned",
    );
    Ok(Json(ApiResponse::new(value)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TopUpRequest {
    amount: f64,
    source: Option<String>,
    reference: Option<String>,
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WalletDirectoryQuery {
    search: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WalletTransactionQuery {
    limit: Option<i64>,
}

async fn wallet_directory(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<WalletDirectoryQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "canteen.wallet.top_up")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let search = query.search.unwrap_or_default().trim().to_lowercase();
    let pattern = format!("%{search}%");
    let limit = query.limit.unwrap_or(500).clamp(1, 2000);
    let wallets = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT COALESCE(jsonb_agg(row ORDER BY lower(row->>'studentName'), row->>'studentNumber'), '[]'::jsonb)
        FROM (
          SELECT jsonb_build_object(
            'userId', student.user_account_id::text,
            'studentId', student.id,
            'studentNumber', student.student_number,
            'studentName', student.full_name,
            'email', student.email,
            'department', COALESCE(department.code, student.department_id, ''),
            'balance', COALESCE(wallet.balance, 0)::float8,
            'updatedAt', wallet.updated_at,
            'lastTransactionAt', (
              SELECT transaction.created_at
              FROM campus_ops.canteen_wallet_transactions transaction
              WHERE transaction.tenant_id=student.tenant_id
                AND transaction.user_id=student.user_account_id::text
              ORDER BY transaction.created_at DESC LIMIT 1
            )
          ) AS row
          FROM core.students student
          LEFT JOIN core.departments department
            ON department.tenant_id=student.tenant_id
           AND department.id::text=student.department_id
          LEFT JOIN campus_ops.canteen_wallets wallet
            ON wallet.tenant_id=student.tenant_id
           AND wallet.user_id=student.user_account_id::text
          WHERE student.tenant_id=$1
            AND student.user_account_id IS NOT NULL
            AND student.status IN ('provisional','active')
            AND ($2='' OR lower(concat_ws(' ', student.student_number,
              student.full_name, student.email, department.code)) LIKE $3)
          ORDER BY lower(student.full_name), student.student_number
          LIMIT $4
        ) directory"#,
    )
    .bind(tenant)
    .bind(&search)
    .bind(pattern)
    .bind(limit)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({"wallets": wallets}))))
}

async fn wallet_transactions(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<WalletTransactionQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "canteen.wallet.top_up")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let transactions = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT COALESCE(jsonb_agg(row ORDER BY row->>'createdAt' DESC), '[]'::jsonb)
        FROM (
          SELECT jsonb_build_object(
            'id', transaction.id,
            'userId', transaction.user_id,
            'studentName', COALESCE(student.full_name, 'Campus user'),
            'studentNumber', COALESCE(student.student_number, ''),
            'amount', transaction.amount::float8,
            'transactionType', transaction.transaction_type,
            'description', transaction.description,
            'referenceId', transaction.reference_id,
            'createdAt', transaction.created_at
          ) AS row
          FROM campus_ops.canteen_wallet_transactions transaction
          LEFT JOIN core.students student
            ON student.tenant_id=transaction.tenant_id
           AND student.user_account_id::text=transaction.user_id
          WHERE transaction.tenant_id=$1
          ORDER BY transaction.created_at DESC
          LIMIT $2
        ) activity"#,
    )
    .bind(tenant)
    .bind(limit)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(
        json!({"transactions": transactions}),
    )))
}

async fn top_up_wallet(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(user_id): Path<String>,
    Json(input): Json<TopUpRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "canteen.wallet.top_up")?;
    if input.amount <= 0.0 || input.amount > 100000.0 {
        return Err(ApiError::BadRequest(
            "Top-up amount must be greater than zero".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let target_user_id = Uuid::parse_str(&user_id)
        .map_err(|_| ApiError::BadRequest("Wallet user id is invalid".into()))?;
    let active_tenant_member = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
               SELECT 1
               FROM identity.tenant_memberships membership
               JOIN identity.users user_account ON user_account.id = membership.user_id
               WHERE membership.tenant_id = $1
                 AND membership.user_id = $2
                 AND membership.active
                 AND user_account.active
           )"#,
    )
    .bind(tenant)
    .bind(target_user_id)
    .fetch_one(db.pool())
    .await?;
    if !active_tenant_member {
        return Err(ApiError::NotFound(
            "Active tenant wallet user was not found".into(),
        ));
    }
    let target_user_id = target_user_id.to_string();
    let mut tx = db.pool().begin().await?;
    let balance=sqlx::query_scalar::<_,f64>("INSERT INTO campus_ops.canteen_wallets(tenant_id,user_id,balance,version) VALUES($1,$2,$3,1) ON CONFLICT(tenant_id,user_id) DO UPDATE SET balance=campus_ops.canteen_wallets.balance+EXCLUDED.balance,version=campus_ops.canteen_wallets.version+1,updated_at=now() RETURNING balance::float8")
 .bind(tenant).bind(&target_user_id).bind(input.amount).fetch_one(&mut *tx).await?;
    let kind = if input.source.as_deref() == Some("online") {
        "online_top_up"
    } else {
        "manual_top_up"
    };
    let transaction=sqlx::query_scalar::<_,Value>("INSERT INTO campus_ops.canteen_wallet_transactions(tenant_id,user_id,amount,transaction_type,description,reference_id,idempotency_key,actor_user_id) VALUES($1,$2,$3,$4,'Wallet top-up',$5,$6,$7) RETURNING jsonb_build_object('id',id,'amount',amount::float8,'transactionType',transaction_type,'description',description,'createdAt',created_at)")
 .bind(tenant).bind(&target_user_id).bind(input.amount).bind(kind).bind(input.reference).bind(input.idempotency_key).bind(&principal.student.id).fetch_one(&mut *tx).await?;
    let payload = json!({"userId":target_user_id,"balance":balance,"transaction":transaction});
    emit_tx(
        &mut tx,
        tenant,
        "canteen",
        "wallet",
        &target_user_id,
        "wallet.credited",
        &principal.student.id,
        &payload,
    )
    .await?;
    notify_tx(
        &mut tx,
        tenant,
        Some(&target_user_id),
        None,
        "canteen",
        "Wallet credited",
        &format!(
            "{:.2} credits were added to your canteen wallet",
            input.amount
        ),
        &payload,
    )
    .await?;
    tx.commit().await?;
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "canteen",
        "wallet",
        &target_user_id,
        "wallet.credited",
    );
    Ok((StatusCode::CREATED, Json(ApiResponse::new(payload))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StaffStateRequest {
    mode: String,
    shop_open: Option<bool>,
}
async fn update_canteen_staff_state(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<StaffStateRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "canteen.orders.manage")?;
    if !matches!(input.mode.as_str(), "eat" | "work") {
        return Err(ApiError::BadRequest("Mode must be eat or work".into()));
    }
    if input.shop_open.is_some() {
        require(&access, "canteen.orders.manage")?;
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    if assigned_shop_keys(db.pool(), tenant, &principal.student.id)
        .await?
        .is_empty()
    {
        return Err(ApiError::Forbidden);
    }
    let value=sqlx::query_scalar::<_,Value>("INSERT INTO campus_ops.canteen_staff_state(tenant_id,user_id,mode,shop_open) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,user_id) DO UPDATE SET mode=EXCLUDED.mode,shop_open=COALESCE(EXCLUDED.shop_open,campus_ops.canteen_staff_state.shop_open),updated_at=now() RETURNING jsonb_build_object('mode',mode,'shopOpen',shop_open)")
 .bind(tenant).bind(&principal.student.id).bind(&input.mode).bind(input.shop_open).fetch_one(db.pool()).await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "canteen",
        "staff_state",
        &principal.student.id,
        "staff_state.updated",
        &principal.student.id,
        &value,
    )
    .await?;
    Ok(Json(ApiResponse::new(value)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GatepassRequestInput {
    pass_type: String,
    residency: String,
    destination: String,
    reason: String,
    guardian_phone: Option<String>,
    departure_at: DateTime<Utc>,
    return_at: DateTime<Utc>,
}
async fn gatepass_overview(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &[
            "gatepass.outpass.read",
            "gatepass.leave.read",
            "gatepass.scan.read",
            "gatepass.outpass.approve",
            "gatepass.leave.approve",
        ],
    )?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let manage = access.allows("gatepass.outpass.approve")
        || access.allows("gatepass.leave.approve")
        || access.allows("gatepass.scan.read");
    let data=sqlx::query_scalar::<_,Value>(r#"SELECT jsonb_build_object('requests',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',id,'requesterUserId',requester_user_id,'requesterName',requester_name,'passType',pass_type,'residency',residency,'departureAt',departure_at,'returnAt',return_at,'destination',destination,'reason',reason,'guardianPhone',guardian_phone,'state',state,'workflow',workflow,'createdAt',created_at,'updatedAt',updated_at) ORDER BY created_at DESC) FROM campus_ops.gatepass_requests WHERE tenant_id=$1 AND ($3 OR requester_user_id=$2)),'[]'::jsonb),'movements',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',id,'userId',user_id,'requestId',request_id,'direction',direction,'checkpoint',checkpoint,'method',method,'createdAt',created_at) ORDER BY created_at DESC) FROM campus_ops.gate_movements WHERE tenant_id=$1 AND ($3 OR user_id=$2)),'[]'::jsonb),'canManage',$3::boolean)"#)
 .bind(tenant).bind(&principal.student.id).bind(manage).fetch_one(db.pool()).await?;
    Ok(Json(ApiResponse::new(data)))
}

async fn create_gatepass_request(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<GatepassRequestInput>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    let permission = if input.pass_type == "outpass" {
        "gatepass.outpass.create"
    } else if input.pass_type == "leave_pass" {
        "gatepass.leave.create"
    } else {
        return Err(ApiError::BadRequest(
            "Pass type must be outpass or leave_pass".into(),
        ));
    };
    require(&access, permission)?;
    if input.pass_type == "outpass" && input.residency != "hosteller" {
        return Err(ApiError::BadRequest(
            "Outpass is available only to hostellers".into(),
        ));
    }
    if input.return_at <= input.departure_at || input.reason.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "Enter valid pass dates and reason".into(),
        ));
    }
    let workflow = if input.pass_type == "outpass" {
        json!({"steps":["parent","warden","security"],"current":"parent"})
    } else {
        json!({"steps":["advisor_or_hod","principal","security_or_warden"],"current":"advisor_or_hod"})
    };
    let state_name = if input.pass_type == "outpass" {
        "pending_parent"
    } else {
        "pending_advisor_or_hod"
    };
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let id = Uuid::new_v4();
    let value=sqlx::query_scalar::<_,Value>("INSERT INTO campus_ops.gatepass_requests(id,tenant_id,requester_user_id,requester_name,pass_type,residency,destination,reason,guardian_phone,departure_at,return_at,state,workflow) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING jsonb_build_object('id',id,'passType',pass_type,'residency',residency,'destination',destination,'reason',reason,'guardianPhone',guardian_phone,'departureAt',departure_at,'returnAt',return_at,'state',state,'workflow',workflow,'createdAt',created_at)")
 .bind(id).bind(tenant).bind(&principal.student.id).bind(&principal.student.name).bind(&input.pass_type).bind(&input.residency).bind(input.destination.trim()).bind(input.reason.trim()).bind(&input.guardian_phone).bind(input.departure_at).bind(input.return_at).bind(state_name).bind(&workflow).fetch_one(db.pool()).await?;

    // An outpass waits on a guardian who has no account, so the link that lets
    // them answer is minted and sent here. The guardian's number comes from the
    // student record when it is on file, and from the request only as a
    // fallback — a student should not be able to nominate their own approver by
    // typing a different number into the form.
    if input.pass_type == "outpass" {
        let on_file = sqlx::query_as::<_, (String, String)>(
            r#"SELECT guardian.full_name, guardian.phone
               FROM core.guardians guardian
               JOIN core.students student
                 ON student.tenant_id = guardian.tenant_id AND student.id = guardian.student_id
               WHERE guardian.tenant_id = $1
                 AND student.user_account_id::text = $2
                 AND guardian.is_primary
                 AND guardian.phone IS NOT NULL"#,
        )
        .bind(tenant)
        .bind(&principal.student.id)
        .fetch_optional(db.pool())
        .await?;

        let guardian = on_file.or_else(|| {
            input
                .guardian_phone
                .clone()
                .filter(|phone| phone.trim().len() >= 8)
                .map(|phone| ("Guardian".to_string(), phone))
        });

        match guardian {
            Some((name, phone)) => {
                let link = crate::guardian_link::issue_guardian_link(
                    &state,
                    &principal.student.tenant_id,
                    tenant,
                    db.pool(),
                    id,
                    &name,
                    &phone,
                    &principal.student.name,
                    input.departure_at,
                )
                .await?;
                tracing::info!(request = %id, delivery = %link["deliveryState"], "guardian approval link issued");
            }
            None => {
                // Recorded rather than rejected: an administrator can still
                // approve the parent step by hand, and the student should not
                // be blocked by a gap in their own record.
                tracing::warn!(
                    request = %id,
                    "outpass raised with no guardian on file and no number supplied"
                );
            }
        }
    }

    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "gatepass",
        "request",
        &id.to_string(),
        "request.created",
        &principal.student.id,
        &value,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

async fn cancel_gatepass_request(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(request_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &["gatepass.outpass.create", "gatepass.leave.create"],
    )?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let value = sqlx::query_scalar::<_, Value>(
        r#"UPDATE campus_ops.gatepass_requests
           SET state='cancelled', updated_at=now()
           WHERE tenant_id=$1 AND id=$2 AND requester_user_id=$3
             AND state IN ('pending_parent','pending_warden','pending_advisor_or_hod','pending_principal')
           RETURNING jsonb_build_object('id',id,'passType',pass_type,'state',state,
             'departureAt',departure_at,'returnAt',return_at,'destination',destination,
             'reason',reason,'guardianPhone',guardian_phone,'createdAt',created_at)"#,
    )
    .bind(tenant)
    .bind(request_id)
    .bind(&principal.student.id)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::Conflict("Only your pending pass can be cancelled".into()))?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "gatepass",
        "request",
        &request_id.to_string(),
        "request.cancelled",
        &principal.student.id,
        &value,
    )
    .await?;
    Ok(Json(ApiResponse::new(value)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecisionRequest {
    decision: String,
    note: Option<String>,
}
/// Advances a pass one step along its chain, inside a caller-supplied
/// transaction.
///
/// Two very different callers reach this: a member of staff with a session and
/// a permission, and a guardian holding a one-time link and no account at all.
/// They differ entirely in how they are *authorised* and not at all in what the
/// decision *means*, so the state machine lives here and is shared. A second
/// copy for the guardian path would drift, and the two would eventually
/// disagree about what "approved" does to an outpass.
///
/// `actor` is recorded as the decision's author — a user id for staff, and the
/// guardian's phone for a link, so the audit trail names whoever actually
/// pressed the button.
pub(crate) struct StepOutcome {
    pub value: Value,
    pub next_state: String,
    pub pass_type: String,
    pub requester_user_id: String,
}

pub(crate) async fn advance_gatepass_step(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: Uuid,
    request_id: Uuid,
    decision: &str,
    note: Option<&str>,
    actor: &str,
    // When set, the request must already be waiting on exactly this step. The
    // guardian link passes `Some("parent")` so a leaked link cannot be spent
    // later to approve the warden's step instead.
    expected_step: Option<&str>,
) -> ApiResult<StepOutcome> {
    let current = sqlx::query_as::<_, (String, String, String)>(
        "SELECT pass_type,state,requester_user_id FROM campus_ops.gatepass_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(request_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Pass request not found".into()))?;

    let step = match current.1.as_str() {
        "pending_parent" => "parent",
        "pending_warden" => "warden",
        "pending_advisor_or_hod" => "advisor_or_hod",
        "pending_principal" => "principal",
        _ => {
            return Err(ApiError::Conflict(
                "This pass is not awaiting a decision".into(),
            ));
        }
    };
    if let Some(expected) = expected_step
        && expected != step
    {
        return Err(ApiError::Conflict(
            "This pass is not awaiting that decision".into(),
        ));
    }

    let (next, raw_qr) = if decision == "rejected" {
        ("rejected".to_string(), None)
    } else {
        match current.1.as_str() {
            "pending_parent" => ("pending_warden".into(), None),
            "pending_advisor_or_hod" => ("pending_principal".into(), None),
            _ => ("approved".into(), Some(Uuid::new_v4().to_string())),
        }
    };

    sqlx::query("INSERT INTO campus_ops.gatepass_approvals(tenant_id,request_id,step_key,decision,actor_user_id,note) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(tenant).bind(request_id).bind(step).bind(decision).bind(actor).bind(note)
        .execute(&mut **tx).await?;

    let value = sqlx::query_scalar::<_, Value>("UPDATE campus_ops.gatepass_requests SET state=$3,qr_token_hash=$4,decided_by=$5,decision_note=$6,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING jsonb_build_object('id',id,'requesterUserId',requester_user_id,'state',state,'qrPayload',$7::text,'updatedAt',updated_at)")
        .bind(tenant).bind(request_id).bind(&next)
        .bind(raw_qr.as_ref().map(|v| token_hash(v)))
        .bind(actor).bind(note).bind(&raw_qr)
        .fetch_one(&mut **tx).await?;

    Ok(StepOutcome {
        value,
        next_state: next,
        pass_type: current.0,
        requester_user_id: current.2,
    })
}

async fn decide_gatepass_request(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(request_id): Path<Uuid>,
    Json(input): Json<DecisionRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &["gatepass.outpass.approve", "gatepass.leave.approve"],
    )?;
    if !matches!(input.decision.as_str(), "approved" | "rejected") {
        return Err(ApiError::BadRequest(
            "Decision must be approved or rejected".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let outcome = advance_gatepass_step(
        &mut tx,
        tenant,
        request_id,
        &input.decision,
        input.note.as_deref(),
        &principal.student.id,
        // Staff may answer whichever step the pass is on; the permission check
        // above is what limits them.
        None,
    )
    .await?;
    let StepOutcome {
        value,
        next_state: next,
        pass_type,
        requester_user_id,
    } = outcome;
    emit_tx(
        &mut tx,
        tenant,
        "gatepass",
        "request",
        &request_id.to_string(),
        "request.decided",
        &principal.student.id,
        &value,
    )
    .await?;
    notify_tx(
        &mut tx,
        tenant,
        Some(&requester_user_id),
        None,
        "gatepass",
        "Gatepass updated",
        &format!("Your {pass_type} is now {next}"),
        &value,
    )
    .await?;
    tx.commit().await?;
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "gatepass",
        "request",
        &request_id.to_string(),
        "request.decided",
    );
    Ok(Json(ApiResponse::new(value)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DailyAccessRequest {
    latitude: f64,
    longitude: f64,
    accuracy_metres: Option<f64>,
}
/// Rejects a gate-in activation raised from outside the campus.
///
/// The coordinates have been recorded since daily_access_passes was created and
/// never once read, which meant the pass could be activated from anywhere — the
/// geofence existed only in the description of the feature. A tenant that has
/// not drawn a fence yet is let through, because failing closed would lock out
/// every institution that upgrades before configuring one; that is the single
/// deliberate hole, and it closes the moment a campus gets a geofence.
struct CampusFenceCheck {
    inside: bool,
    nearest_fence: Option<(f64, f64, f64)>,
}

async fn campus_fence_check(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    latitude: f64,
    longitude: f64,
    accuracy_metres: Option<f64>,
) -> ApiResult<CampusFenceCheck> {
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return Err(ApiError::BadRequest("That is not a valid location".into()));
    }

    let fences = sqlx::query_as::<_, (f64, f64, f64)>(
        r#"SELECT (metadata -> 'geofence' ->> 'latitude')::float8,
                  (metadata -> 'geofence' ->> 'longitude')::float8,
                  COALESCE((metadata -> 'geofence' ->> 'radiusMetres')::float8, 250)
           FROM core.campuses
           WHERE tenant_id = $1
             AND active
             AND metadata -> 'geofence' ->> 'latitude' IS NOT NULL
             AND metadata -> 'geofence' ->> 'longitude' IS NOT NULL"#,
    )
    .bind(tenant)
    .fetch_all(pool)
    .await?;

    if fences.is_empty() {
        return Ok(CampusFenceCheck {
            inside: true,
            nearest_fence: None,
        });
    }

    // Phone fixes report their uncertainty as a radius. Allow a small,
    // bounded accuracy margin so a learner physically inside the configured
    // boundary is not rejected merely because the current GPS fix is noisy.
    // The cap prevents a low-quality location from opening the fence broadly.
    let accuracy_margin = accuracy_metres
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(0.0)
        .min(100.0);
    let nearest_fence = fences.iter().copied().min_by(|left, right| {
        metres_between(latitude, longitude, left.0, left.1)
            .total_cmp(&metres_between(latitude, longitude, right.0, right.1))
    });
    let inside = fences.iter().any(|(fence_lat, fence_lon, radius)| {
        position_is_within_fence(
            latitude,
            longitude,
            *fence_lat,
            *fence_lon,
            *radius,
            accuracy_margin,
        )
    });
    Ok(CampusFenceCheck {
        inside,
        nearest_fence,
    })
}

fn position_is_within_fence(
    latitude: f64,
    longitude: f64,
    fence_latitude: f64,
    fence_longitude: f64,
    radius_metres: f64,
    accuracy_margin_metres: f64,
) -> bool {
    metres_between(latitude, longitude, fence_latitude, fence_longitude)
        <= radius_metres + accuracy_margin_metres
}

/// Great-circle distance in metres.
///
/// Haversine rather than a flat approximation: a campus fence is small enough
/// that either would do, but the flat version misbehaves near the poles and
/// across the antimeridian for no saving worth having.
fn metres_between(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_METRES: f64 = 6_371_000.0;
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let delta_phi = (lat2 - lat1).to_radians();
    let delta_lambda = (lon2 - lon1).to_radians();
    let a = (delta_phi / 2.0).sin().powi(2)
        + phi1.cos() * phi2.cos() * (delta_lambda / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_METRES * a.sqrt().asin()
}

async fn activate_daily_access(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<DailyAccessRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "gatepass.access.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let fence_check = campus_fence_check(
        db.pool(),
        tenant,
        input.latitude,
        input.longitude,
        input.accuracy_metres,
    )
    .await?;
    if !fence_check.inside {
        // Crossing out of the zone invalidates the previously displayed token
        // immediately. Merely hiding it in the app would leave a screenshot of
        // the old QR valid at the scanner until another token replaced it.
        sqlx::query(
            "DELETE FROM campus_ops.daily_access_passes \
             WHERE tenant_id = $1 AND user_id = $2 AND valid_on = CURRENT_DATE",
        )
        .bind(tenant)
        .bind(&principal.student.id)
        .execute(db.pool())
        .await?;
        return Err(ApiError::Forbidden);
    }
    let raw = Uuid::new_v4().to_string();
    let hash = token_hash(&raw);
    let mut value=sqlx::query_scalar::<_,Value>("INSERT INTO campus_ops.daily_access_passes(tenant_id,user_id,valid_on,qr_token_hash,activated_latitude,activated_longitude) VALUES($1,$2,CURRENT_DATE,$3,$4,$5) ON CONFLICT(tenant_id,user_id,valid_on) DO UPDATE SET qr_token_hash=EXCLUDED.qr_token_hash,activated_latitude=EXCLUDED.activated_latitude,activated_longitude=EXCLUDED.activated_longitude,activated_at=now() RETURNING jsonb_build_object('id',id,'validOn',valid_on,'validFrom',activated_at,'validUntil',(valid_on+1)::timestamptz,'qrPayload',$6::text)")
 .bind(tenant).bind(&principal.student.id).bind(hash).bind(input.latitude).bind(input.longitude).bind(&raw).fetch_one(db.pool()).await?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "location".into(),
            json!({
                "latitude": input.latitude,
                "longitude": input.longitude,
                "accuracyMetres": input.accuracy_metres,
            }),
        );
        if let Some((latitude, longitude, radius_metres)) = fence_check.nearest_fence {
            object.insert(
                "campusGeofence".into(),
                json!({
                    "latitude": latitude,
                    "longitude": longitude,
                    "radiusMetres": radius_metres,
                }),
            );
        }
    }
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "gatepass",
        "daily_access",
        &principal.student.id,
        "daily_access.activated",
        &principal.student.id,
        &json!({"validOn":value["validOn"]}),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GateScanRequest {
    qr_payload: String,
    direction: String,
    checkpoint: String,
}
async fn scan_gatepass(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<GateScanRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "gatepass.scan.create")?;
    if !matches!(input.direction.as_str(), "entry" | "exit") {
        return Err(ApiError::BadRequest(
            "Direction must be entry or exit".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let hash = token_hash(&input.qr_payload);
    // Three kinds of token arrive at the same scanner and the guard cannot tell
    // them apart by looking: an approved outpass or leave pass, a member's
    // geofenced daily gate-in, or a visitor's card. A visitor has no account, so
    // their pass id stands in for the user id — gate_movements.user_id is NOT
    // NULL and every "movements for this person" query already keys on it.
    let match_row = sqlx::query_as::<_, (String, Option<Uuid>, Option<Uuid>)>(
        r#"SELECT user_id,request_id,visitor_pass_id FROM (
          SELECT requester_user_id user_id,id request_id,NULL::uuid visitor_pass_id
            FROM campus_ops.gatepass_requests
           WHERE tenant_id=$1 AND state='approved' AND qr_token_hash=$2
          UNION ALL
          SELECT user_id,NULL::uuid,NULL::uuid
            FROM campus_ops.daily_access_passes
           WHERE tenant_id=$1 AND valid_on=CURRENT_DATE AND qr_token_hash=$2
          UNION ALL
          -- A visitor pass is only good inside the window it was approved for.
          SELECT id::text,NULL::uuid,id
            FROM campus_ops.visitor_passes
           WHERE tenant_id=$1 AND state='approved' AND qr_token_hash=$2
             AND now() BETWEEN visit_from AND visit_until
        ) valid LIMIT 1"#,
    )
    .bind(tenant)
    .bind(hash)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound("QR is invalid or expired".into()))?;
    let value=sqlx::query_scalar::<_,Value>("INSERT INTO campus_ops.gate_movements(tenant_id,user_id,request_id,visitor_pass_id,direction,checkpoint,scanned_by) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING jsonb_build_object('id',id,'userId',user_id,'requestId',request_id,'visitorPassId',visitor_pass_id,'direction',direction,'checkpoint',checkpoint,'createdAt',created_at)")
 .bind(tenant).bind(&match_row.0).bind(match_row.1).bind(match_row.2).bind(&input.direction).bind(input.checkpoint.trim()).bind(&principal.student.id).fetch_one(db.pool()).await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "gatepass",
        "movement",
        value["id"].as_str().unwrap_or_default(),
        "movement.scanned",
        &principal.student.id,
        &value,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttendanceSessionRequest {
    timetable_entry_id: Option<Uuid>,
    subject_name: String,
    held_on: NaiveDate,
    period_label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttendanceRosterQuery {
    section_id: Option<String>,
    section_ids: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttendanceClassesQuery {
    held_on: Option<NaiveDate>,
}

/// Published classes the signed-in faculty member is actually responsible for
/// on a date. Department/advisor reach intentionally does not widen this list:
/// attendance ownership follows the timetable's teaching matrix.
async fn attendance_classes(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<AttendanceClassesQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "attendance.session.create")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let held_on = query.held_on.unwrap_or_else(|| Utc::now().date_naive());
    let classes = sqlx::query_scalar::<_, Value>(
        r#"
        WITH assigned AS (
          SELECT entry.id AS timetable_entry_id,
                 offering.id AS subject_offering_id,
                 section.id AS section_id,
                 subject.code AS subject_code,
                 subject.name AS subject_name,
                 section.name AS section_name,
                 slot.label AS starts_label,
                 ending_slot.label AS ends_label,
                 slot.sequence,
                 slot.starts_at,
                 ending_slot.ends_at,
                 entry.delivery_type,
                 entry.block_length,
                 NULLIF(entry.metadata ->> 'combinedClassCode', '') AS combined_class_code,
                 NULLIF(entry.metadata ->> 'combinedClassName', '') AS combined_class_name
          FROM core.timetable_entries entry
          JOIN core.timetable_versions version
            ON version.id = entry.version_id AND version.status = 'published'
          JOIN core.timetable_slots slot ON slot.id = entry.slot_id
          JOIN core.timetable_entries ending_entry
            ON ending_entry.version_id = entry.version_id
           AND ending_entry.session_block_id = entry.session_block_id
           AND ending_entry.block_sequence = entry.block_length
          JOIN core.timetable_slots ending_slot ON ending_slot.id = ending_entry.slot_id
          JOIN core.subject_offerings offering ON offering.id = entry.subject_offering_id
          JOIN core.subjects subject ON subject.id = offering.subject_id
          JOIN core.sections section ON section.id = offering.section_id
          JOIN core.teaching_assignments teaching ON teaching.id = entry.teaching_assignment_id
          LEFT JOIN LATERAL (
            SELECT substitution.substitute_faculty_user_id
            FROM core.faculty_substitution_requests substitution
            WHERE substitution.tenant_id = entry.tenant_id
              AND substitution.timetable_entry_id = entry.id
              AND substitution.service_date = $3
              AND substitution.status = 'approved'
            ORDER BY substitution.created_at DESC
            LIMIT 1
          ) replacement ON true
          WHERE entry.tenant_id = $1
            AND slot.day_of_week = EXTRACT(ISODOW FROM $3::date)::int
            AND COALESCE(replacement.substitute_faculty_user_id, teaching.faculty_user_id)::text = $2
            AND entry.block_sequence = 1
        ), collapsed AS (
          SELECT min(timetable_entry_id::text)::uuid AS timetable_entry_id,
                 min(subject_offering_id::text)::uuid AS subject_offering_id,
                 min(section_id::text)::uuid AS section_id,
                 jsonb_agg(DISTINCT section_id::text) AS section_ids,
                 subject_code, subject_name,
                 COALESCE(max(combined_class_name), min(section_name)) AS section_name,
                 starts_label, ends_label, sequence, starts_at, max(ends_at) AS ends_at,
                 delivery_type, block_length,
                 max(combined_class_code) AS combined_class_code,
                 max(combined_class_name) AS combined_class_name
            FROM assigned
           GROUP BY COALESCE(combined_class_code, timetable_entry_id::text),
                    subject_code, subject_name, starts_label, ends_label,
                    sequence, starts_at, delivery_type, block_length
        )
        SELECT COALESCE(jsonb_agg(jsonb_build_object(
          'timetableEntryId', timetable_entry_id,
          'subjectOfferingId', subject_offering_id,
          'sectionId', section_id,
          'sectionIds', section_ids,
          'subjectCode', subject_code,
          'subjectName', subject_name,
          'sectionName', section_name,
          'periodLabel', CASE WHEN block_length > 1
                              THEN starts_label || '-' || ends_label ELSE starts_label END,
          'sequence', sequence,
          'startsAt', starts_at,
          'endsAt', ends_at,
          'deliveryType', delivery_type,
          'combinedClassCode', combined_class_code,
          'combinedClassName', combined_class_name
        ) ORDER BY sequence), '[]'::jsonb)
        FROM collapsed
        "#,
    )
    .bind(tenant)
    .bind(&principal.student.id)
    .bind(held_on)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(
        json!({"classes": classes, "heldOn": held_on}),
    )))
}

/// The complete student directory slice owned by the signed-in class advisor.
/// Department ownership comes from explicit assignments rather than a client
/// supplied filter, so one advisor can safely cover multiple departments.
async fn advisor_students(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    if !access.roles.iter().any(|role| role == "class_advisor") {
        return Err(ApiError::Forbidden);
    }
    require(&access, "students.directory.read")?;

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let students = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT COALESCE(jsonb_agg(jsonb_build_object(
          'studentUserId', student.user_account_id::text,
          'studentId', student.id,
          'studentNumber', student.student_number,
          'studentName', student.full_name,
          'email', student.email,
          'phone', student.phone,
          'departmentId', student.department_id,
          'departmentCode', department.code,
          'departmentName', department.name,
          'programmeName', programme.name,
          'academicYear', student.academic_year,
          'sectionId', student.section_id,
          'sectionName', section.name,
          'campusName', campus.name,
          'status', student.status,
          'photoUrl', NULLIF(student.profile ->> 'photoUrl', ''),
          'profile', student.profile
        ) ORDER BY department.code, student.student_number, student.full_name), '[]'::jsonb)
        FROM core.students student
        JOIN core.class_advisor_assignments assignment
          ON assignment.tenant_id = student.tenant_id
         AND assignment.department_id::text = student.department_id
         AND assignment.advisor_user_id::text = $2
         AND assignment.active
        LEFT JOIN core.departments department
          ON department.tenant_id = student.tenant_id
         AND department.id::text = student.department_id
        LEFT JOIN core.programmes programme
          ON programme.tenant_id = student.tenant_id
         AND programme.id::text = student.program_id
        LEFT JOIN core.sections section
          ON section.tenant_id = student.tenant_id
         AND section.id::text = student.section_id
        LEFT JOIN core.campuses campus
          ON campus.tenant_id = student.tenant_id
         AND campus.id::text = student.campus_id
        WHERE student.tenant_id = $1
          AND student.status IN ('provisional', 'active')
          AND student.user_account_id IS NOT NULL
        "#,
    )
    .bind(tenant)
    .bind(&principal.student.id)
    .fetch_one(db.pool())
    .await?;

    Ok(Json(ApiResponse::new(json!({"students": students}))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdvisorAssessmentRequest {
    assessment_kind: String,
    title: String,
    semester: Option<i16>,
    marks_obtained: f64,
    maximum_marks: f64,
    notes: Option<String>,
    assessed_on: Option<NaiveDate>,
}

struct ValidatedAdvisorAssessment {
    assessment_kind: String,
    title: String,
    semester: Option<i16>,
    marks_obtained: f64,
    maximum_marks: f64,
    notes: Option<String>,
    assessed_on: Option<NaiveDate>,
}

fn validate_advisor_assessment(
    input: AdvisorAssessmentRequest,
) -> ApiResult<ValidatedAdvisorAssessment> {
    let kind = input.assessment_kind.trim().to_ascii_lowercase();
    if !matches!(kind.as_str(), "semester" | "internal" | "test") {
        return Err(ApiError::BadRequest(
            "Assessment type must be semester, internal, or test".into(),
        ));
    }
    let title = input.title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(ApiError::BadRequest(
            "Assessment title must contain 1 to 120 characters".into(),
        ));
    }
    if input
        .semester
        .is_some_and(|value| !(1..=12).contains(&value))
    {
        return Err(ApiError::BadRequest(
            "Semester must be between 1 and 12".into(),
        ));
    }
    if !input.maximum_marks.is_finite()
        || input.maximum_marks <= 0.0
        || input.maximum_marks > 10_000.0
        || !input.marks_obtained.is_finite()
        || input.marks_obtained < 0.0
        || input.marks_obtained > input.maximum_marks
    {
        return Err(ApiError::BadRequest(
            "Marks must be between zero and the maximum mark".into(),
        ));
    }
    let notes = input
        .notes
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if notes
        .as_ref()
        .is_some_and(|value| value.chars().count() > 2_000)
    {
        return Err(ApiError::BadRequest(
            "Assessment notes cannot exceed 2000 characters".into(),
        ));
    }
    Ok(ValidatedAdvisorAssessment {
        assessment_kind: kind,
        title: title.to_owned(),
        semester: input.semester,
        marks_obtained: input.marks_obtained,
        maximum_marks: input.maximum_marks,
        notes,
        assessed_on: input.assessed_on,
    })
}

fn require_class_advisor(access: &EffectiveAccess) -> ApiResult<()> {
    if !access.roles.iter().any(|role| role == "class_advisor") {
        return Err(ApiError::Forbidden);
    }
    require(access, "students.directory.read")
}

async fn ensure_advisor_owns_student(
    pool: &sqlx::PgPool,
    tenant: Uuid,
    advisor_user_id: &str,
    student_id: Uuid,
) -> ApiResult<()> {
    let owned = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
             SELECT 1
             FROM core.students student
             JOIN core.class_advisor_assignments assignment
               ON assignment.tenant_id = student.tenant_id
              AND assignment.department_id::text = student.department_id
              AND assignment.advisor_user_id::text = $2
              AND assignment.active
             WHERE student.tenant_id = $1
               AND student.id = $3
               AND student.status IN ('provisional', 'active')
           )"#,
    )
    .bind(tenant)
    .bind(advisor_user_id)
    .bind(student_id)
    .fetch_one(pool)
    .await?;
    if owned {
        Ok(())
    } else {
        Err(ApiError::NotFound("Student not found".into()))
    }
}

async fn advisor_student_assessments(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(student_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_class_advisor(&access)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    ensure_advisor_owns_student(db.pool(), tenant, &principal.student.id, student_id).await?;

    let assessments = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'id', mark.id,
             'assessmentKind', mark.assessment_kind,
             'title', mark.title,
             'semester', mark.semester,
             'marksObtained', mark.marks_obtained,
             'maximumMarks', mark.maximum_marks,
             'notes', mark.notes,
             'assessedOn', mark.assessed_on,
             'updatedAt', mark.updated_at
           ) ORDER BY mark.semester DESC NULLS LAST,
                      mark.assessed_on DESC NULLS LAST,
                      mark.created_at DESC), '[]'::jsonb)
           FROM core.student_assessment_marks mark
           WHERE mark.tenant_id = $1 AND mark.student_id = $2"#,
    )
    .bind(tenant)
    .bind(student_id)
    .fetch_one(db.pool())
    .await?;

    Ok(Json(ApiResponse::new(json!({"assessments": assessments}))))
}

async fn create_advisor_student_assessment(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(student_id): Path<Uuid>,
    Json(input): Json<AdvisorAssessmentRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require_class_advisor(&access)?;
    let input = validate_advisor_assessment(input)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    ensure_advisor_owns_student(db.pool(), tenant, &principal.student.id, student_id).await?;

    let created = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO core.student_assessment_marks
             (tenant_id, student_id, advisor_user_id, assessment_kind, title,
              semester, marks_obtained, maximum_marks, notes, assessed_on)
           VALUES ($1, $2, $3::uuid, $4, $5, $6, $7, $8, $9, $10)
           RETURNING jsonb_build_object(
             'id', id, 'assessmentKind', assessment_kind, 'title', title,
             'semester', semester, 'marksObtained', marks_obtained,
             'maximumMarks', maximum_marks, 'notes', notes,
             'assessedOn', assessed_on, 'updatedAt', updated_at)"#,
    )
    .bind(tenant)
    .bind(student_id)
    .bind(&principal.student.id)
    .bind(&input.assessment_kind)
    .bind(&input.title)
    .bind(input.semester)
    .bind(input.marks_obtained)
    .bind(input.maximum_marks)
    .bind(&input.notes)
    .bind(input.assessed_on)
    .fetch_one(db.pool())
    .await?;

    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "students",
        "assessment",
        &student_id.to_string(),
        "assessment.created",
    );
    Ok((StatusCode::CREATED, Json(ApiResponse::new(created))))
}

async fn update_advisor_student_assessment(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path((student_id, assessment_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<AdvisorAssessmentRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_class_advisor(&access)?;
    let input = validate_advisor_assessment(input)?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    ensure_advisor_owns_student(db.pool(), tenant, &principal.student.id, student_id).await?;

    let updated = sqlx::query_scalar::<_, Value>(
        r#"UPDATE core.student_assessment_marks
           SET assessment_kind = $4, title = $5, semester = $6,
               marks_obtained = $7, maximum_marks = $8, notes = $9,
               assessed_on = $10, advisor_user_id = $11::uuid, updated_at = now()
           WHERE tenant_id = $1 AND student_id = $2 AND id = $3
           RETURNING jsonb_build_object(
             'id', id, 'assessmentKind', assessment_kind, 'title', title,
             'semester', semester, 'marksObtained', marks_obtained,
             'maximumMarks', maximum_marks, 'notes', notes,
             'assessedOn', assessed_on, 'updatedAt', updated_at)"#,
    )
    .bind(tenant)
    .bind(student_id)
    .bind(assessment_id)
    .bind(&input.assessment_kind)
    .bind(&input.title)
    .bind(input.semester)
    .bind(input.marks_obtained)
    .bind(input.maximum_marks)
    .bind(&input.notes)
    .bind(input.assessed_on)
    .bind(&principal.student.id)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound("Assessment not found".into()))?;

    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "students",
        "assessment",
        &assessment_id.to_string(),
        "assessment.updated",
    );
    Ok(Json(ApiResponse::new(updated)))
}

/// Assessment marks for the student linked to the signed-in user account.
/// The student identity is resolved from the bearer token rather than a path
/// parameter so a learner cannot request another student's results.
async fn student_assessments(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "academics.marks.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let student_id = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT id
           FROM core.students
           WHERE tenant_id = $1
             AND user_account_id::text = $2
             AND status IN ('provisional', 'active')
           LIMIT 1"#,
    )
    .bind(tenant)
    .bind(&principal.student.id)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound("Student profile not found".into()))?;

    let assessments = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'id', mark.id,
             'assessmentKind', mark.assessment_kind,
             'title', mark.title,
             'semester', mark.semester,
             'marksObtained', mark.marks_obtained,
             'maximumMarks', mark.maximum_marks,
             'notes', mark.notes,
             'assessedOn', mark.assessed_on,
             'updatedAt', mark.updated_at
           ) ORDER BY mark.semester DESC NULLS LAST,
                      mark.assessed_on DESC NULLS LAST,
                      mark.created_at DESC), '[]'::jsonb)
           FROM core.student_assessment_marks mark
           WHERE mark.tenant_id = $1 AND mark.student_id = $2"#,
    )
    .bind(tenant)
    .bind(student_id)
    .fetch_one(db.pool())
    .await?;

    Ok(Json(ApiResponse::new(json!({"assessments": assessments}))))
}

async fn attendance_roster(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Query(query): Query<AttendanceRosterQuery>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &["attendance.roster.read", "attendance.roster.update"],
    )?;

    // The section comes off the query string, so on its own it is a request,
    // not a permission. Reach decides what the request is allowed to mean:
    // without this, omitting it returned every active student in the tenant,
    // and naming someone else's section returned theirs.
    let scope = access
        .scope_for("attendance.roster.read")
        .or_else(|| access.scope_for("attendance.roster.update"))
        .unwrap_or("own");
    let institution_wide = matches!(scope, "institution" | "all");
    let department_wide = scope == "department";

    // Anything narrower than the whole institution has to say which section it
    // wants. A caller who cannot name one has nothing to be shown.
    let requested_section_ids = query
        .section_ids
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .chain(query.section_id.iter().cloned())
        .collect::<Vec<_>>();
    if !institution_wide && requested_section_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "sectionId or sectionIds is required at this access level".into(),
        ));
    }
    let requested_section_ids =
        (!requested_section_ids.is_empty()).then_some(requested_section_ids);

    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let students = sqlx::query_scalar::<_, Value>(
        r#"
        WITH member_departments AS (
            SELECT authority.department_id
            FROM core.department_authorities authority
            WHERE authority.tenant_id = $1
              AND authority.user_id::text = $3
              AND authority.active
              AND (authority.starts_on IS NULL OR authority.starts_on <= CURRENT_DATE)
              AND (authority.ends_on IS NULL OR authority.ends_on >= CURRENT_DATE)
            UNION
            SELECT employee.department_id
            FROM core.employees employee
            WHERE employee.tenant_id = $1
              AND employee.user_id::text = $3
              AND employee.department_id IS NOT NULL
        )
        SELECT COALESCE(jsonb_agg(jsonb_build_object(
          'studentUserId', student.user_account_id::text,
          'studentId', student.id,
          'studentNumber', student.student_number,
          'studentName', student.full_name,
          'departmentId', student.department_id,
          -- The roll card names the programme and department a student belongs
          -- to. Both id columns on core.students are text holding a uuid, hence
          -- the casts on the other side of the join.
          'departmentCode', department.code,
          'programmeName', programme.name,
          -- Set from the tenant admin's Students page; the roll card shows it.
          'photoUrl', NULLIF(student.profile ->> 'photoUrl', ''),
          'sectionId', student.section_id
        ) ORDER BY student.student_number, student.full_name), '[]'::jsonb)
        FROM core.students student
        LEFT JOIN core.departments department
          ON department.tenant_id = student.tenant_id
         AND department.id::text = student.department_id
        LEFT JOIN core.programmes programme
          ON programme.tenant_id = student.tenant_id
         AND programme.id::text = student.program_id
        WHERE student.tenant_id = $1
          AND student.status = 'active'
          AND student.user_account_id IS NOT NULL
          AND ($2::text[] IS NULL OR student.section_id = ANY($2))
          AND (
            $4
            OR EXISTS (
              SELECT 1 FROM core.class_advisor_assignments advisor
              WHERE advisor.tenant_id = student.tenant_id
                AND advisor.advisor_user_id::text = $3
                AND advisor.department_id::text = student.department_id
                AND advisor.active
            )
            OR EXISTS (
              SELECT 1
              FROM core.timetable_entries timetable_entry
              JOIN core.timetable_versions timetable_version
                ON timetable_version.id = timetable_entry.version_id
               AND timetable_version.status = 'published'
              JOIN core.subject_offerings timetable_offering
                ON timetable_offering.id = timetable_entry.subject_offering_id
              JOIN core.teaching_assignments timetable_teaching
                ON timetable_teaching.id = timetable_entry.teaching_assignment_id
              WHERE timetable_entry.tenant_id = student.tenant_id
                AND timetable_offering.section_id::text = student.section_id
                AND timetable_teaching.faculty_user_id::text = $3
            )
            OR EXISTS (
              SELECT 1
              FROM core.faculty_substitution_requests substitution
              JOIN core.timetable_entries timetable_entry
                ON timetable_entry.id = substitution.timetable_entry_id
              JOIN core.timetable_versions timetable_version
                ON timetable_version.id = timetable_entry.version_id
               AND timetable_version.status = 'published'
              JOIN core.subject_offerings timetable_offering
                ON timetable_offering.id = timetable_entry.subject_offering_id
              WHERE substitution.tenant_id = student.tenant_id
                AND substitution.substitute_faculty_user_id::text = $3
                AND substitution.status = 'approved'
                AND timetable_offering.section_id::text = student.section_id
            )
            OR ($5 AND student.department_id IN (
              -- core.students.department_id is text where the authority tables
              -- hold uuid; compare on one side rather than trusting a cast.
              SELECT department_id::text FROM member_departments
            ))
          )
        "#,
    )
    .bind(tenant)
    .bind(requested_section_ids)
    .bind(&principal.student.id)
    .bind(institution_wide)
    .bind(department_wide)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({"students": students}))))
}

async fn attendance_wards(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "attendance.parent.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let wards = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT COALESCE(jsonb_agg(jsonb_build_object(
          'studentUserId', student.user_account_id,
          'studentId', student.id,
          'studentNumber', student.student_number,
          'studentName', student.full_name,
          'departmentId', student.department_id,
          'sectionId', student.section_id
        ) ORDER BY student.full_name), '[]'::jsonb)
        FROM campus_ops.parent_student_links link
        JOIN core.students student
          ON student.tenant_id = link.tenant_id
         AND student.user_account_id = link.student_user_id
        WHERE link.tenant_id = $1
          AND link.parent_user_id = $2
          AND link.active
          AND student.status = 'active'
        "#,
    )
    .bind(tenant)
    .bind(&principal.student.id)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({"wards": wards}))))
}

async fn attendance_sessions(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &[
            "attendance.roster.read",
            "attendance.records.read",
            "attendance.reports.create",
        ],
    )?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let manage = access.allows("attendance.reports.create");
    let rows=sqlx::query_scalar::<_,Value>("SELECT COALESCE(jsonb_agg(jsonb_build_object('id',id,'timetableEntryId',timetable_entry_id,'subjectOfferingId',subject_offering_id,'sectionId',section_id,'subjectName',subject_name,'facultyUserId',faculty_user_id,'heldOn',held_on,'periodLabel',period_label,'status',status,'updatedAt',updated_at) ORDER BY held_on DESC,created_at DESC),'[]'::jsonb) FROM campus_ops.attendance_sessions WHERE tenant_id=$1 AND ($3 OR faculty_user_id=$2)").bind(tenant).bind(&principal.student.id).bind(manage).fetch_one(db.pool()).await?;
    Ok(Json(ApiResponse::new(json!({"sessions":rows}))))
}
async fn create_attendance_session(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<AttendanceSessionRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "attendance.session.create")?;
    if input.subject_name.trim().is_empty() || input.period_label.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "Subject and period are required".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let entry_id = input.timetable_entry_id.ok_or_else(|| {
        ApiError::BadRequest("Select a class from today's published timetable".into())
    })?;
    let (subject_offering_id, section_id, subject_name, period_label) =
        sqlx::query_as::<_, (Uuid, Uuid, String, String)>(
            r#"SELECT offering.id,
                      offering.section_id,
                      subject.name,
                      CASE WHEN entry.block_length > 1
                           THEN slot.label || '-' || ending_slot.label
                           ELSE slot.label
                      END AS period_label
               FROM core.timetable_entries entry
               JOIN core.timetable_versions version ON version.id = entry.version_id
               JOIN core.timetable_slots slot ON slot.id = entry.slot_id
               JOIN core.timetable_entries ending_entry
                 ON ending_entry.tenant_id = entry.tenant_id
                AND ending_entry.version_id = entry.version_id
                AND ending_entry.session_block_id = entry.session_block_id
                AND ending_entry.block_sequence = entry.block_length
               JOIN core.timetable_slots ending_slot ON ending_slot.id = ending_entry.slot_id
               JOIN core.subject_offerings offering ON offering.id = entry.subject_offering_id
               JOIN core.subjects subject ON subject.id = offering.subject_id
               JOIN core.teaching_assignments teaching ON teaching.id = entry.teaching_assignment_id
               LEFT JOIN LATERAL (
                 SELECT substitution.substitute_faculty_user_id
                 FROM core.faculty_substitution_requests substitution
                 WHERE substitution.tenant_id = entry.tenant_id
                   AND substitution.timetable_entry_id = entry.id
                   AND substitution.service_date = $4
                   AND substitution.status = 'approved'
                 ORDER BY substitution.created_at DESC
                 LIMIT 1
               ) replacement ON true
               WHERE entry.tenant_id = $1
                 AND entry.id = $3
                 AND version.status = 'published'
                 AND entry.block_sequence = 1
                 AND slot.day_of_week = EXTRACT(ISODOW FROM $4::date)::int
                 AND COALESCE(replacement.substitute_faculty_user_id, teaching.faculty_user_id)::text = $2"#,
        )
        .bind(tenant)
        .bind(&principal.student.id)
        .bind(entry_id)
        .bind(input.held_on)
        .fetch_optional(db.pool())
        .await?
        .ok_or(ApiError::Forbidden)?;
    let value=sqlx::query_scalar::<_,Value>("INSERT INTO campus_ops.attendance_sessions(tenant_id,timetable_entry_id,subject_offering_id,section_id,subject_name,faculty_user_id,held_on,period_label) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING jsonb_build_object('id',id,'timetableEntryId',timetable_entry_id,'subjectOfferingId',subject_offering_id,'sectionId',section_id,'subjectName',subject_name,'heldOn',held_on,'periodLabel',period_label,'status',status)").bind(tenant).bind(entry_id).bind(subject_offering_id).bind(section_id).bind(subject_name).bind(&principal.student.id).bind(input.held_on).bind(period_label).fetch_one(db.pool()).await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "attendance",
        "session",
        value["id"].as_str().unwrap_or_default(),
        "session.created",
        &principal.student.id,
        &value,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttendanceEntryInput {
    student_user_id: String,
    student_name: String,
    status: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplaceEntriesRequest {
    entries: Vec<AttendanceEntryInput>,
}
async fn replace_attendance_entries(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(session_id): Path<Uuid>,
    Json(input): Json<ReplaceEntriesRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "attendance.roster.update")?;
    if input
        .entries
        .iter()
        .any(|e| !matches!(e.status.as_str(), "present" | "absent" | "od" | "leave"))
    {
        return Err(ApiError::BadRequest("Invalid attendance status".into()));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let owner=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM campus_ops.attendance_sessions WHERE tenant_id=$1 AND id=$2 AND faculty_user_id=$3 AND status IN('draft','returned'))").bind(tenant).bind(session_id).bind(&principal.student.id).fetch_one(&mut *tx).await?;
    if !owner {
        return Err(ApiError::Conflict(
            "Only the assigned faculty can mark this draft session".into(),
        ));
    }
    for e in &input.entries {
        sqlx::query("INSERT INTO campus_ops.attendance_entries(tenant_id,session_id,student_user_id,student_name,status,marked_by) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,session_id,student_user_id) DO UPDATE SET student_name=EXCLUDED.student_name,status=EXCLUDED.status,marked_by=EXCLUDED.marked_by,marked_at=now()").bind(tenant).bind(session_id).bind(&e.student_user_id).bind(&e.student_name).bind(&e.status).bind(&principal.student.id).execute(&mut *tx).await?;
    }
    let payload = json!({"sessionId":session_id,"entries":input.entries.len()});
    emit_tx(
        &mut tx,
        tenant,
        "attendance",
        "session",
        &session_id.to_string(),
        "entries.updated",
        &principal.student.id,
        &payload,
    )
    .await?;
    tx.commit().await?;
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "attendance",
        "session",
        &session_id.to_string(),
        "entries.updated",
    );
    Ok(Json(ApiResponse::new(payload)))
}
async fn publish_attendance_session(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(session_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "attendance.session.publish")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let value=sqlx::query_scalar::<_,Value>("UPDATE campus_ops.attendance_sessions SET status='published_to_hod',updated_at=now() WHERE tenant_id=$1 AND id=$2 AND faculty_user_id=$3 AND status IN('draft','returned') RETURNING jsonb_build_object('id',id,'subjectName',subject_name,'heldOn',held_on,'status',status)").bind(tenant).bind(session_id).bind(&principal.student.id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::Conflict("Attendance session cannot be published".into()))?;
    let students=sqlx::query_as::<_,(String,String)>("SELECT student_user_id,status FROM campus_ops.attendance_entries WHERE tenant_id=$1 AND session_id=$2").bind(tenant).bind(session_id).fetch_all(&mut *tx).await?;
    for (student, status) in &students {
        let body = format!(
            "You were marked {} for {}",
            status,
            value["subjectName"].as_str().unwrap_or("class")
        );
        notify_tx(
            &mut tx,
            tenant,
            Some(student),
            None,
            "attendance",
            "Attendance marked",
            &body,
            &value,
        )
        .await?;
    }
    notify_tx(
        &mut tx,
        tenant,
        None,
        Some("hod"),
        "attendance",
        "Attendance ready for review",
        "A faculty attendance session was published",
        &value,
    )
    .await?;
    emit_tx(
        &mut tx,
        tenant,
        "attendance",
        "session",
        &session_id.to_string(),
        "session.published_to_hod",
        &principal.student.id,
        &value,
    )
    .await?;
    tx.commit().await?;
    for (student, _) in students {
        state.publish_realtime(
            RealtimePublication::tenant(
                &principal.student.tenant_id,
                "attendance.record.published",
                json!({
                    "module": "attendance",
                    "sessionId": session_id,
                    "invalidate": true,
                }),
            )
            .for_user(student),
        );
    }
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "attendance",
        "session",
        &session_id.to_string(),
        "session.published_to_hod",
    );
    Ok(Json(ApiResponse::new(value)))
}

async fn attendance_summary(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(student_id): Path<String>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &["attendance.records.read", "attendance.parent.read"],
    )?;
    let target = if student_id == "me" {
        principal.student.id.clone()
    } else {
        student_id
    };
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    if target != principal.student.id && !access.allows("attendance.parent.read") {
        return Err(ApiError::Forbidden);
    }
    if target != principal.student.id && access.allows("attendance.parent.read") {
        let linked=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM campus_ops.parent_student_links WHERE tenant_id=$1 AND parent_user_id=$2 AND student_user_id=$3 AND active)").bind(tenant).bind(&principal.student.id).bind(&target).fetch_one(db.pool()).await?;
        if !linked {
            return Err(ApiError::Forbidden);
        }
    }
    let value = sqlx::query_scalar::<_, Value>(
        r#"
        WITH student_records AS (
          SELECT
            s.id AS session_id,
            s.subject_offering_id,
            s.subject_name,
            subject.code AS subject_code,
            s.held_on,
            s.period_label,
            s.created_at,
            e.status
          FROM campus_ops.attendance_entries e
          JOIN campus_ops.attendance_sessions s
            ON s.tenant_id = e.tenant_id AND s.id = e.session_id
          LEFT JOIN core.subject_offerings offering
            ON offering.tenant_id = s.tenant_id
           AND offering.id = s.subject_offering_id
          LEFT JOIN core.subjects subject
            ON subject.tenant_id = offering.tenant_id
           AND subject.id = offering.subject_id
          WHERE e.tenant_id = $1
            AND e.student_user_id = $2
            AND s.status IN ('published_to_hod', 'submitted_to_principal')
        ),
        subject_totals AS (
          SELECT
            subject_offering_id,
            subject_name,
            max(subject_code) AS subject_code,
            count(*) AS total_classes,
            count(*) FILTER (WHERE status = 'present') AS present_classes,
            count(*) FILTER (WHERE status = 'absent') AS absent_classes,
            count(*) FILTER (WHERE status = 'od') AS on_duty_classes,
            count(*) FILTER (WHERE status = 'leave') AS leave_classes
          FROM student_records
          GROUP BY subject_offering_id, subject_name
        )
        SELECT jsonb_build_object(
          'studentUserId', $2::text,
          'totalClasses', (SELECT count(*) FROM student_records),
          'attendedClasses', (SELECT count(*) FROM student_records WHERE status IN ('present', 'od')),
          'presentClasses', (SELECT count(*) FROM student_records WHERE status = 'present'),
          'absences', (SELECT count(*) FROM student_records WHERE status = 'absent'),
          'onDutyClasses', (SELECT count(*) FROM student_records WHERE status = 'od'),
          'leaveClasses', (SELECT count(*) FROM student_records WHERE status = 'leave'),
          'percentage', CASE
            WHEN (SELECT count(*) FROM student_records) = 0 THEN 0
            ELSE round(
              (SELECT count(*) FROM student_records WHERE status IN ('present', 'od'))::numeric
              * 100 / (SELECT count(*) FROM student_records), 2
            )::float8
          END,
          'records', COALESCE((
            SELECT jsonb_agg(jsonb_build_object(
              'sessionId', session_id,
              'subjectOfferingId', subject_offering_id,
              'subjectCode', subject_code,
              'subjectName', subject_name,
              'heldOn', held_on,
              'periodLabel', period_label,
              'status', status
            ) ORDER BY held_on DESC, created_at DESC)
            FROM student_records
          ), '[]'::jsonb),
          'bySubject', COALESCE((
            SELECT jsonb_agg(jsonb_build_object(
              'subjectOfferingId', subject_offering_id,
              'subjectCode', subject_code,
              'subjectName', subject_name,
              'totalClasses', total_classes,
              'attendedClasses', present_classes + on_duty_classes,
              'presentClasses', present_classes,
              'absentClasses', absent_classes,
              'onDutyClasses', on_duty_classes,
              'leaveClasses', leave_classes,
              'percentage', CASE WHEN total_classes = 0 THEN 0 ELSE
                round((present_classes + on_duty_classes)::numeric * 100 / total_classes, 2)::float8
              END
            ) ORDER BY subject_name)
            FROM subject_totals
          ), '[]'::jsonb)
        )
        "#,
    )
    .bind(tenant)
    .bind(&target)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(value)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReportRequest {
    title: String,
    period_start: NaiveDate,
    period_end: NaiveDate,
    department_id: Option<Uuid>,
}
async fn attendance_reports(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require_any(
        &access,
        &["attendance.reports.create", "attendance.reports.publish"],
    )?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let rows=sqlx::query_scalar::<_,Value>("SELECT COALESCE(jsonb_agg(jsonb_build_object('id',id,'title',title,'periodStart',period_start,'periodEnd',period_end,'departmentId',department_id,'status',status,'summary',summary,'generatedBy',generated_by,'submittedAt',submitted_at,'createdAt',created_at) ORDER BY created_at DESC),'[]'::jsonb) FROM campus_ops.attendance_reports WHERE tenant_id=$1").bind(tenant).fetch_one(db.pool()).await?;
    Ok(Json(ApiResponse::new(json!({"reports":rows}))))
}
async fn create_attendance_report(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<ReportRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "attendance.reports.create")?;
    if input.period_end < input.period_start {
        return Err(ApiError::BadRequest(
            "Report end date must follow start date".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let summary=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('sessions',count(DISTINCT s.id),'entries',count(e.*),'present',count(e.*) FILTER(WHERE e.status='present'),'absent',count(e.*) FILTER(WHERE e.status='absent')) FROM campus_ops.attendance_sessions s LEFT JOIN campus_ops.attendance_entries e ON e.tenant_id=s.tenant_id AND e.session_id=s.id WHERE s.tenant_id=$1 AND s.held_on BETWEEN $2 AND $3 AND s.status='published_to_hod'").bind(tenant).bind(input.period_start).bind(input.period_end).fetch_one(db.pool()).await?;
    let value=sqlx::query_scalar::<_,Value>("INSERT INTO campus_ops.attendance_reports(tenant_id,title,period_start,period_end,department_id,generated_by,summary) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING jsonb_build_object('id',id,'title',title,'periodStart',period_start,'periodEnd',period_end,'status',status,'summary',summary)").bind(tenant).bind(input.title.trim()).bind(input.period_start).bind(input.period_end).bind(input.department_id).bind(&principal.student.id).bind(summary).fetch_one(db.pool()).await?;
    emit(
        &state,
        &principal.student.tenant_id,
        db.pool(),
        tenant,
        "attendance",
        "report",
        value["id"].as_str().unwrap_or_default(),
        "report.created",
        &principal.student.id,
        &value,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(value))))
}
async fn submit_attendance_report(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(report_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "attendance.reports.publish")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let value=sqlx::query_scalar::<_,Value>("UPDATE campus_ops.attendance_reports SET status='submitted_to_principal',submitted_at=now() WHERE tenant_id=$1 AND id=$2 AND status='draft' RETURNING jsonb_build_object('id',id,'title',title,'status',status,'submittedAt',submitted_at)").bind(tenant).bind(report_id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::Conflict("Report cannot be submitted".into()))?;
    notify_tx(
        &mut tx,
        tenant,
        None,
        Some("principal"),
        "attendance",
        "Attendance report submitted",
        "A HOD attendance report is ready for review",
        &value,
    )
    .await?;
    emit_tx(
        &mut tx,
        tenant,
        "attendance",
        "report",
        &report_id.to_string(),
        "report.submitted_to_principal",
        &principal.student.id,
        &value,
    )
    .await?;
    tx.commit().await?;
    publish_operation_change(
        &state,
        &principal.student.tenant_id,
        "attendance",
        "report",
        &report_id.to_string(),
        "report.submitted_to_principal",
    );
    Ok(Json(ApiResponse::new(value)))
}

pub(crate) fn require(access: &EffectiveAccess, permission: &str) -> ApiResult<()> {
    if access.allows(permission) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn authorized_change_modules(access: &EffectiveAccess) -> Vec<String> {
    const MODULES: [&str; 3] = ["attendance", "canteen", "gatepass"];
    MODULES
        .into_iter()
        .filter(|module| {
            access.permissions.iter().any(|permission| {
                permission == "*"
                    || permission == *module
                    || permission == &format!("{module}.*")
                    || permission.starts_with(&format!("{module}."))
            })
        })
        .map(str::to_owned)
        .collect()
}

pub(crate) fn require_any(access: &EffectiveAccess, permissions: &[&str]) -> ApiResult<()> {
    if permissions.iter().any(|p| access.allows(p)) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}
pub(crate) async fn tenant_id(pool: &sqlx::PgPool, slug: &str) -> ApiResult<Uuid> {
    sqlx::query_scalar("SELECT id FROM platform.tenants WHERE slug=$1")
        .bind(slug)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::NotFound("Tenant not found".into()))
}
pub(crate) fn token_hash(value: &str) -> String {
    let mut h = Sha256::new();
    h.update(value.as_bytes());
    format!("{:x}", h.finalize())
}
#[allow(clippy::too_many_arguments)]
async fn emit(
    state: &AppState,
    tenant_slug: &str,
    pool: &sqlx::PgPool,
    tenant: Uuid,
    module: &str,
    aggregate: &str,
    id: &str,
    event: &str,
    actor: &str,
    payload: &Value,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO campus_ops.events(tenant_id,module_key,aggregate_type,aggregate_id,event_type,actor_user_id,payload) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tenant).bind(module).bind(aggregate).bind(id).bind(event).bind(actor).bind(payload).execute(pool).await?;
    publish_operation_change(state, tenant_slug, module, aggregate, id, event);
    Ok(())
}

fn publish_operation_change(
    state: &AppState,
    tenant_slug: &str,
    module: &str,
    aggregate: &str,
    id: &str,
    event: &str,
) {
    state.publish_realtime(RealtimePublication::tenant(
        tenant_slug,
        format!("{module}.{event}"),
        json!({
            "module": module,
            "resource": aggregate,
            "resourceId": id,
            "operation": event,
            "invalidate": true,
        }),
    ));
}
#[allow(clippy::too_many_arguments)]
async fn emit_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    module: &str,
    aggregate: &str,
    id: &str,
    event: &str,
    actor: &str,
    payload: &Value,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO campus_ops.events(tenant_id,module_key,aggregate_type,aggregate_id,event_type,actor_user_id,payload) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tenant).bind(module).bind(aggregate).bind(id).bind(event).bind(actor).bind(payload).execute(&mut **tx).await?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn notify_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    user: Option<&str>,
    role: Option<&str>,
    category: &str,
    title: &str,
    body: &str,
    data: &Value,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO campus_ops.notifications(tenant_id,recipient_user_id,recipient_role,category,title,body,data) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tenant).bind(user).bind(role).bind(category).bind(title).bind(body).bind(data).execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn access(portal_family: &str, permissions: &[&str]) -> EffectiveAccess {
        EffectiveAccess {
            roles: Vec::new(),
            portal_families: vec![portal_family.into()],
            permissions: permissions.iter().map(|value| (*value).into()).collect(),
            scopes: HashMap::new(),
        }
    }

    #[test]
    fn change_feed_exposes_only_modules_granted_on_the_active_surface() {
        let student = access("student", &["canteen.menu.read", "attendance.records.read"]);
        assert_eq!(
            authorized_change_modules(&student),
            vec!["attendance".to_owned(), "canteen".to_owned()]
        );

        let unassigned = access("student", &[]);
        assert!(authorized_change_modules(&unassigned).is_empty());

        let admin = access("admin", &["*"]);
        assert_eq!(authorized_change_modules(&admin).len(), 3);
    }

    #[test]
    fn advisor_assessment_validation_accepts_manual_tests() {
        let value = validate_advisor_assessment(AdvisorAssessmentRequest {
            assessment_kind: "test".into(),
            title: "Weekly quiz 3".into(),
            semester: Some(2),
            marks_obtained: 17.5,
            maximum_marks: 20.0,
            notes: Some("Improved presentation".into()),
            assessed_on: None,
        })
        .expect("valid manual test");
        assert_eq!(value.assessment_kind, "test");
        assert_eq!(value.title, "Weekly quiz 3");
    }

    #[test]
    fn advisor_assessment_validation_rejects_impossible_marks() {
        let result = validate_advisor_assessment(AdvisorAssessmentRequest {
            assessment_kind: "internal".into(),
            title: "Internal 1".into(),
            semester: Some(1),
            marks_obtained: 41.0,
            maximum_marks: 40.0,
            notes: None,
            assessed_on: None,
        });
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn gps_accuracy_margin_keeps_an_inside_device_inside() {
        let without_margin =
            position_is_within_fence(13.0144, 80.2356, 13.0104, 80.2356, 400.0, 0.0);
        let with_margin = position_is_within_fence(13.0144, 80.2356, 13.0104, 80.2356, 400.0, 50.0);

        assert!(!without_margin);
        assert!(with_margin);
    }

    #[test]
    fn campus_codes_are_stable_and_url_safe() {
        assert_eq!(
            campus_code_from("Madras Engineering College"),
            "MADRAS-ENGINEERING"
        );
    }
}

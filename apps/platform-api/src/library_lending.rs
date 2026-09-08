use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::ApiResponse,
    operations::{require, tenant_id},
    state::{AppState, AuthPrincipal, EffectiveAccess},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/library/catalog", get(catalog))
        .route("/library/books/import", post(import_books))
        .route("/library/loans", get(loans).post(request_loan))
        .route("/library/loans/{loan_id}/decision", post(decide_loan))
        .route("/library/loans/{loan_id}/renew", post(renew_loan))
        .route("/library/loans/{loan_id}/return", post(return_loan))
        .route(
            "/library/favourites/{book_id}",
            post(add_favourite).delete(remove_favourite),
        )
        .route(
            "/library/lending-settings",
            get(settings).put(update_settings),
        )
}

fn user_id(principal: &AuthPrincipal) -> ApiResult<Uuid> {
    Uuid::parse_str(&principal.student.id).map_err(|_| ApiError::Unauthorized)
}

fn can_manage(access: &EffectiveAccess) -> bool {
    access.allows("library.catalog.manage") || access.roles.iter().any(|role| role == "librarian")
}

async fn catalog(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.catalog.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let viewer = user_id(&principal)?;
    let books = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'id', book.id, 'isbn', book.isbn, 'accessionNumber', book.accession_number,
             'title', book.title, 'author', book.author, 'category', book.category,
             'shelfCode', book.shelf_code, 'totalCopies', book.total_copies,
             'availableCopies', book.available_copies,
             'isFavourite', favourite.book_id IS NOT NULL
           ) ORDER BY lower(book.title)), '[]'::jsonb)
           FROM library.books book
           LEFT JOIN library.book_favourites favourite
             ON favourite.tenant_id=book.tenant_id AND favourite.book_id=book.id
            AND favourite.student_user_id=$2
           WHERE book.tenant_id=$1 AND book.active"#,
    )
    .bind(tenant)
    .bind(viewer)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({"books": books}))))
}

async fn loans(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.loan.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let viewer = user_id(&principal)?;
    let all_students = access.allows("library.loan.approve") || can_manage(&access);
    let rows = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object(
             'id', loan.id, 'bookId', loan.book_id, 'bookTitle', book.title,
             'author', book.author, 'studentUserId', loan.student_user_id,
             'studentName', loan.student_name, 'rollNumber', loan.roll_number,
             'status', loan.status, 'requestedAt', loan.requested_at,
             'approvedAt', loan.approved_at, 'dueAt', loan.due_at,
             'returnedAt', loan.returned_at, 'renewalCount', loan.renewal_count,
             'decisionNote', loan.decision_note,
             'overdueDays', CASE WHEN loan.status='approved' AND loan.due_at < now()
               THEN CEIL(EXTRACT(EPOCH FROM (now()-loan.due_at))/86400)::int ELSE 0 END,
             'fineAmount', CASE WHEN loan.status='approved' AND loan.due_at < now()
               THEN CEIL(EXTRACT(EPOCH FROM (now()-loan.due_at))/86400) * settings.fine_per_day ELSE 0 END
           ) ORDER BY loan.requested_at DESC), '[]'::jsonb)
           FROM library.book_loans loan
           JOIN library.books book ON book.id=loan.book_id AND book.tenant_id=loan.tenant_id
           JOIN library.lending_settings settings ON settings.tenant_id=loan.tenant_id
           WHERE loan.tenant_id=$1 AND ($3 OR loan.student_user_id=$2)"#,
    )
    .bind(tenant)
    .bind(viewer)
    .bind(all_students)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(json!({"loans": rows}))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoanRequest {
    book_id: Uuid,
}

async fn request_loan(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<LoanRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "library.loan.create")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let student = user_id(&principal)?;
    let roll = principal.student.roll.trim();
    if roll.is_empty() {
        return Err(ApiError::BadRequest(
            "Your account does not have a roll number".into(),
        ));
    }
    let row = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO library.book_loans
             (tenant_id, book_id, student_user_id, roll_number, student_name)
           SELECT $1, book.id, $3, $4, $5
           FROM library.books book
           WHERE book.tenant_id=$1 AND book.id=$2 AND book.active
             AND book.available_copies > 0
           RETURNING jsonb_build_object(
             'id', id, 'bookId', book_id, 'studentUserId', student_user_id,
             'studentName', student_name, 'rollNumber', roll_number,
             'status', status, 'requestedAt', requested_at)"#,
    )
    .bind(tenant)
    .bind(input.book_id)
    .bind(student)
    .bind(roll)
    .bind(&principal.student.name)
    .fetch_optional(db.pool())
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(|db| db.is_unique_violation())
        {
            ApiError::Conflict("You already have an open request for this book".into())
        } else {
            error.into()
        }
    })?
    .ok_or_else(|| ApiError::Conflict("This book is not currently available".into()))?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(row))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecisionRequest {
    decision: String,
    note: Option<String>,
}

async fn decide_loan(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(loan_id): Path<Uuid>,
    Json(input): Json<DecisionRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.loan.approve")?;
    if input.decision != "approved" && input.decision != "rejected" {
        return Err(ApiError::BadRequest(
            "Decision must be approved or rejected".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let decider = user_id(&principal)?;
    let mut tx = db.pool().begin().await?;
    let loan = sqlx::query_as::<_, (Uuid, Uuid, String)>(
        "SELECT id,book_id,status FROM library.book_loans WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(loan_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Borrow request not found".into()))?;
    if loan.2 != "requested" {
        return Err(ApiError::Conflict(
            "This borrow request has already been reviewed".into(),
        ));
    }
    if input.decision == "approved" {
        let available = sqlx::query_scalar::<_, i32>(
            r#"UPDATE library.books SET available_copies=available_copies-1,updated_at=now()
               WHERE tenant_id=$1 AND id=$2 AND active AND available_copies>0
               RETURNING available_copies"#,
        )
        .bind(tenant)
        .bind(loan.1)
        .fetch_optional(&mut *tx)
        .await?;
        if available.is_none() {
            return Err(ApiError::Conflict(
                "No copy of this book is available".into(),
            ));
        }
    }
    let row = sqlx::query_scalar::<_, Value>(
        r#"UPDATE library.book_loans loan SET
             status=$3, decided_by=$4, decision_note=$5, updated_at=now(),
             approved_at=CASE WHEN $3='approved' THEN now() ELSE approved_at END,
             due_at=CASE WHEN $3='approved' THEN now() +
               ((SELECT loan_days FROM library.lending_settings WHERE tenant_id=$1) * interval '1 day')
               ELSE due_at END
           FROM library.books book
           WHERE loan.tenant_id=$1 AND loan.id=$2 AND book.id=loan.book_id
           RETURNING jsonb_build_object(
             'id',loan.id,'bookId',loan.book_id,'bookTitle',book.title,
             'studentName',loan.student_name,'rollNumber',loan.roll_number,
             'status',loan.status,'requestedAt',loan.requested_at,
             'approvedAt',loan.approved_at,'dueAt',loan.due_at,
             'decisionNote',loan.decision_note,'renewalCount',loan.renewal_count)"#,
    )
    .bind(tenant)
    .bind(loan_id)
    .bind(&input.decision)
    .bind(decider)
    .bind(input.note.as_deref().map(str::trim).filter(|value| !value.is_empty()))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(row)))
}

async fn renew_loan(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(loan_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.loan.create")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let student = user_id(&principal)?;
    let row = sqlx::query_scalar::<_, Value>(
        r#"UPDATE library.book_loans loan SET
             due_at=due_at + (settings.loan_days * interval '1 day'),
             renewal_count=renewal_count+1, updated_at=now()
           FROM library.lending_settings settings, library.books book
           WHERE loan.tenant_id=$1 AND loan.id=$2 AND loan.student_user_id=$3
             AND loan.status='approved' AND loan.due_at >= now()
             AND settings.tenant_id=loan.tenant_id AND book.id=loan.book_id
           RETURNING jsonb_build_object(
             'id',loan.id,'bookId',loan.book_id,'bookTitle',book.title,
             'status',loan.status,'dueAt',loan.due_at,
             'renewalCount',loan.renewal_count,'overdueDays',0,'fineAmount',0)"#,
    )
    .bind(tenant)
    .bind(loan_id)
    .bind(student)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| ApiError::Conflict("Only a current, non-overdue loan can be renewed".into()))?;
    Ok(Json(ApiResponse::new(row)))
}

async fn return_loan(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(loan_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.loan.approve")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let mut tx = db.pool().begin().await?;
    let book_id = sqlx::query_scalar::<_, Uuid>(
        r#"UPDATE library.book_loans SET status='returned',returned_at=now(),updated_at=now()
           WHERE tenant_id=$1 AND id=$2 AND status='approved' RETURNING book_id"#,
    )
    .bind(tenant)
    .bind(loan_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::Conflict("Only an approved loan can be returned".into()))?;
    sqlx::query(
        "UPDATE library.books SET available_copies=LEAST(total_copies,available_copies+1),updated_at=now() WHERE tenant_id=$1 AND id=$2",
    )
    .bind(tenant)
    .bind(book_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(ApiResponse::new(
        json!({"id": loan_id, "status": "returned"}),
    )))
}

async fn add_favourite(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(book_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.catalog.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let student = user_id(&principal)?;
    let inserted = sqlx::query(
        r#"INSERT INTO library.book_favourites(tenant_id,student_user_id,book_id)
           SELECT $1,$2,id FROM library.books WHERE tenant_id=$1 AND id=$3 AND active
           ON CONFLICT DO NOTHING"#,
    )
    .bind(tenant)
    .bind(student)
    .bind(book_id)
    .execute(db.pool())
    .await?;
    if inserted.rows_affected() == 0 {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM library.books WHERE tenant_id=$1 AND id=$2 AND active)",
        )
        .bind(tenant)
        .bind(book_id)
        .fetch_one(db.pool())
        .await?;
        if !exists {
            return Err(ApiError::NotFound("Book not found".into()));
        }
    }
    Ok(Json(ApiResponse::new(
        json!({"bookId": book_id, "isFavourite": true}),
    )))
}

async fn remove_favourite(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Path(book_id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.catalog.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let student = user_id(&principal)?;
    sqlx::query(
        "DELETE FROM library.book_favourites WHERE tenant_id=$1 AND student_user_id=$2 AND book_id=$3",
    )
    .bind(tenant)
    .bind(student)
    .bind(book_id)
    .execute(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(
        json!({"bookId": book_id, "isFavourite": false}),
    )))
}

async fn settings(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.catalog.read")?;
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let row = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO library.lending_settings(tenant_id) VALUES($1)
           ON CONFLICT(tenant_id) DO UPDATE SET tenant_id=EXCLUDED.tenant_id
           RETURNING jsonb_build_object('loanDays',loan_days,'finePerDay',fine_per_day,'updatedAt',updated_at)"#,
    )
    .bind(tenant)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(row)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsRequest {
    loan_days: i32,
    fine_per_day: f64,
}

async fn update_settings(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<SettingsRequest>,
) -> ApiResult<Json<ApiResponse<Value>>> {
    require(&access, "library.catalog.manage")?;
    if !(1..=365).contains(&input.loan_days) || !(0.0..=10_000.0).contains(&input.fine_per_day) {
        return Err(ApiError::BadRequest(
            "Loan days or daily fine is outside the allowed range".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let updater = user_id(&principal)?;
    let row = sqlx::query_scalar::<_, Value>(
        r#"INSERT INTO library.lending_settings(tenant_id,loan_days,fine_per_day,updated_by)
           VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id) DO UPDATE SET
             loan_days=EXCLUDED.loan_days,fine_per_day=EXCLUDED.fine_per_day,
             updated_by=EXCLUDED.updated_by,updated_at=now()
           RETURNING jsonb_build_object('loanDays',loan_days,'finePerDay',fine_per_day,'updatedAt',updated_at)"#,
    )
    .bind(tenant)
    .bind(input.loan_days)
    .bind(input.fine_per_day)
    .bind(updater)
    .fetch_one(db.pool())
    .await?;
    Ok(Json(ApiResponse::new(row)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportRequest {
    books: Vec<BookInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BookInput {
    title: String,
    #[serde(default)]
    author: String,
    isbn: Option<String>,
    accession_number: Option<String>,
    #[serde(default)]
    category: String,
    #[serde(default)]
    shelf_code: String,
    total_copies: i32,
}

async fn import_books(
    State(state): State<AppState>,
    Extension(principal): Extension<AuthPrincipal>,
    Extension(access): Extension<EffectiveAccess>,
    Json(input): Json<ImportRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<Value>>)> {
    require(&access, "library.catalog.manage")?;
    if input.books.is_empty() || input.books.len() > 1_000 {
        return Err(ApiError::BadRequest(
            "Upload between 1 and 1,000 books at a time".into(),
        ));
    }
    let db = state.tenant_database(&principal.student.tenant_id).await?;
    let tenant = tenant_id(db.pool(), &principal.student.tenant_id).await?;
    let creator = user_id(&principal)?;
    let mut tx = db.pool().begin().await?;
    let mut imported = 0;
    for book in input.books {
        let title = book.title.trim();
        if title.is_empty() || book.total_copies < 1 {
            return Err(ApiError::BadRequest(
                "Every book needs a title and at least one copy".into(),
            ));
        }
        let isbn = clean(book.isbn);
        let accession = clean(book.accession_number);
        if isbn.is_none() && accession.is_none() {
            return Err(ApiError::BadRequest(format!(
                "{title} needs an ISBN or accession number"
            )));
        }
        sqlx::query(
            r#"INSERT INTO library.books
                 (tenant_id,isbn,accession_number,title,author,category,shelf_code,total_copies,available_copies,created_by)
               VALUES($1,$2,$3,$4,$5,$6,$7,$8,$8,$9)
               ON CONFLICT (tenant_id, catalog_key)
               DO UPDATE SET title=EXCLUDED.title,author=EXCLUDED.author,category=EXCLUDED.category,
                 shelf_code=EXCLUDED.shelf_code,total_copies=EXCLUDED.total_copies,
                 available_copies=GREATEST(0,EXCLUDED.total_copies-(library.books.total_copies-library.books.available_copies)),
                 active=true,updated_at=now()"#,
        )
        .bind(tenant)
        .bind(isbn)
        .bind(accession)
        .bind(title)
        .bind(book.author.trim())
        .bind(book.category.trim())
        .bind(book.shelf_code.trim())
        .bind(book.total_copies)
        .bind(creator)
        .execute(&mut *tx)
        .await?;
        imported += 1;
    }
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::new(json!({"imported": imported}))),
    ))
}

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

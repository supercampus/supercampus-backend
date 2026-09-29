//! Support for saving a Student Master record from the admin console.
//!
//! Production tenant databases can hold older table shapes than a fresh
//! database: `CREATE TABLE IF NOT EXISTS` kept the first shape and the sqlx
//! migrator is stuck, so the guardian columns the save relies on are ensured
//! here at runtime. Database refusals are translated into messages an
//! administrator can act on instead of a generic failure.

use crate::error::ApiError;

/// Failure while saving a student: a request problem already phrased for the
/// administrator, or a database error still to be translated.
#[derive(Debug)]
pub(crate) enum StudentSaveError {
    Api(ApiError),
    Db(sqlx::Error),
}

impl From<ApiError> for StudentSaveError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<sqlx::Error> for StudentSaveError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl From<anyhow::Error> for StudentSaveError {
    fn from(error: anyhow::Error) -> Self {
        Self::Api(ApiError::from(error))
    }
}

impl From<StudentSaveError> for ApiError {
    fn from(error: StudentSaveError) -> Self {
        match error {
            StudentSaveError::Api(error) => error,
            StudentSaveError::Db(error) => student_save_database_error(error),
        }
    }
}

/// Translates a database refusal while saving a student into a 4xx the
/// administrator can fix. Anything that is not about the submitted values
/// (connectivity, a query bug) keeps the standard handling.
pub(crate) fn student_save_database_error(error: sqlx::Error) -> ApiError {
    let translated = match &error {
        sqlx::Error::Database(database) => translate_database_refusal(
            database.code().as_deref().unwrap_or_default(),
            database.constraint().unwrap_or_default(),
            database.message(),
        ),
        _ => None,
    };
    match translated {
        Some(api_error) => {
            tracing::warn!(error = ?error, "student save refused by the database");
            api_error
        }
        None => ApiError::from(error),
    }
}

fn translate_database_refusal(code: &str, constraint: &str, message: &str) -> Option<ApiError> {
    let subject = format!("{constraint} {message}").to_ascii_lowercase();
    let about = |needle: &str| subject.contains(needle);
    match code {
        // unique_violation
        "23505" => Some(ApiError::Conflict(
            if about("student_number") || about("tenant_number") {
                "That roll number already belongs to another student"
            } else if about("email") {
                "That email address already belongs to another account"
            } else if about("guardians_primary") {
                "This student already has a different primary parent or guardian. Refresh and try again"
            } else if about("guardian") {
                "That parent or guardian is already linked to another account"
            } else if about("admission") {
                "That admission is already linked to another student"
            } else {
                "Another record already uses one of these details"
            }
            .into(),
        )),
        // check_violation
        "23514" => Some(ApiError::BadRequest(
            if about("phone") {
                "Enter the WhatsApp number with its country code, for example +919876543210"
            } else if about("relationship") {
                "Enter the relationship as Father, Mother or Guardian"
            } else if about("status") {
                "That account status is not allowed for this student"
            } else if about("year") {
                "Year of study must be between 1 and 6"
            } else if about("email") {
                "Enter a valid email address"
            } else {
                "One of the student details is not in an accepted format"
            }
            .into(),
        )),
        // not_null_violation
        "23502" => Some(ApiError::BadRequest(
            "A required student detail is missing. Fill in every required field".into(),
        )),
        // foreign_key_violation
        "23503" => Some(ApiError::BadRequest(
            "The selected department or section no longer exists. Choose it again".into(),
        )),
        // string_data_right_truncation
        "22001" => Some(ApiError::BadRequest(
            "One of the student details is too long".into(),
        )),
        // invalid_text_representation, invalid datetime, numeric out of range
        "22P02" | "22007" | "22008" | "22003" => Some(ApiError::BadRequest(
            "One of the student details is not in the expected format".into(),
        )),
        _ => None,
    }
}

/// Checks a parent/guardian WhatsApp number: a country code and 8–15 digits,
/// allowing the usual separators an administrator types.
pub fn validate_guardian_phone(phone: &str) -> Result<(), &'static str> {
    let phone = phone.trim();
    if phone.is_empty() {
        return Err("Enter the parent or guardian WhatsApp number");
    }
    let allowed = phone.chars().enumerate().all(|(index, character)| {
        character.is_ascii_digit()
            || matches!(character, ' ' | '-' | '(' | ')' | '.')
            || (character == '+' && index == 0)
    });
    let digits = phone.chars().filter(char::is_ascii_digit).count();
    if !allowed || !(8..=15).contains(&digits) {
        return Err("Enter the WhatsApp number with its country code, for example +919876543210");
    }
    Ok(())
}

/// Tidies a free-form relationship ("mother", " FATHER ") into the form the
/// rest of the app shows, defaulting to "Parent".
pub fn normalize_guardian_relationship(value: Option<&str>) -> String {
    let value = value.map(str::trim).unwrap_or_default();
    if value.is_empty() {
        return "Parent".into();
    }
    match value.to_ascii_lowercase().as_str() {
        "father" | "dad" => "Father".into(),
        "mother" | "mom" | "mum" => "Mother".into(),
        "guardian" => "Guardian".into(),
        "parent" => "Parent".into(),
        _ => {
            let mut characters = value.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect(),
                None => "Parent".into(),
            }
        }
    }
}

/// The guardian columns and link table the student save writes, mirroring
/// migrations/runtime/0036 and 0062 for databases the stuck migrator did not
/// bring forward. Every statement is idempotent.
const STUDENT_GUARDIAN_SCHEMA: &str = r#"
ALTER TABLE core.guardians
    ADD COLUMN IF NOT EXISTS student_id uuid,
    ADD COLUMN IF NOT EXISTS relationship text,
    ADD COLUMN IF NOT EXISTS is_primary boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS profile jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT now();

CREATE INDEX IF NOT EXISTS guardians_student_idx
    ON core.guardians (tenant_id, student_id)
    WHERE student_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS core.student_guardians (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_id uuid NOT NULL,
    guardian_id uuid NOT NULL,
    relationship text NOT NULL,
    is_primary boolean NOT NULL DEFAULT false,
    permissions jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, student_id, guardian_id)
);

ALTER TABLE core.student_guardians
    ADD COLUMN IF NOT EXISTS is_primary boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS permissions jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS created_at timestamptz NOT NULL DEFAULT now();
"#;

/// Brings the guardian tables up to the shape the student save needs. DDL
/// takes locks, so it runs once per tenant per process; a failure is logged
/// and the save still runs (and reports any resulting error clearly).
pub(crate) async fn ensure_student_guardian_schema(pool: &sqlx::PgPool, tenant_key: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static READY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let ready = READY.get_or_init(|| Mutex::new(HashSet::new()));
    if ready
        .lock()
        .map(|set| set.contains(tenant_key))
        .unwrap_or(false)
    {
        return;
    }
    match sqlx::raw_sql(STUDENT_GUARDIAN_SCHEMA).execute(pool).await {
        Ok(_) => {
            if let Ok(mut set) = ready.lock() {
                set.insert(tenant_key.to_owned());
            }
        }
        Err(error) => {
            tracing::warn!(%error, tenant = tenant_key, "student guardian schema not applied");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse};

    fn status(error: Option<ApiError>) -> Option<StatusCode> {
        error.map(|error| error.into_response().status())
    }

    #[test]
    fn guardian_phone_accepts_international_formats() {
        assert!(validate_guardian_phone("+919000000086").is_ok());
        assert!(validate_guardian_phone("919000000086").is_ok());
        assert!(validate_guardian_phone("+91 90000 00086").is_ok());
        assert!(validate_guardian_phone("(044) 2345-6789").is_ok());
    }

    #[test]
    fn guardian_phone_rejects_short_long_or_lettered_numbers() {
        assert!(validate_guardian_phone("").is_err());
        assert!(validate_guardian_phone("12345").is_err());
        assert!(validate_guardian_phone("+91900000008612345").is_err());
        assert!(validate_guardian_phone("90000abc86").is_err());
        assert!(validate_guardian_phone("91+9000000086").is_err());
    }

    #[test]
    fn relationship_is_tidied_and_defaults_to_parent() {
        assert_eq!(normalize_guardian_relationship(Some(" mother ")), "Mother");
        assert_eq!(normalize_guardian_relationship(Some("FATHER")), "Father");
        assert_eq!(normalize_guardian_relationship(Some("uncle")), "Uncle");
        assert_eq!(normalize_guardian_relationship(Some("  ")), "Parent");
        assert_eq!(normalize_guardian_relationship(None), "Parent");
    }

    #[test]
    fn constraint_refusals_become_client_errors() {
        assert_eq!(
            status(translate_database_refusal(
                "23505",
                "students_tenant_number_idx",
                ""
            )),
            Some(StatusCode::CONFLICT)
        );
        assert_eq!(
            status(translate_database_refusal(
                "23505",
                "guardians_primary_idx",
                ""
            )),
            Some(StatusCode::CONFLICT)
        );
        assert_eq!(
            status(translate_database_refusal(
                "23514",
                "guardians_phone_check",
                ""
            )),
            Some(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            status(translate_database_refusal(
                "23502",
                "",
                "null value in column"
            )),
            Some(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            status(translate_database_refusal(
                "23503",
                "students_department_fkey",
                ""
            )),
            Some(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            status(translate_database_refusal(
                "22P02",
                "",
                "invalid input syntax for type uuid"
            )),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn query_bugs_are_not_blamed_on_the_administrator() {
        assert!(translate_database_refusal("42P01", "", "missing FROM-clause entry").is_none());
        assert!(translate_database_refusal("42703", "", "column does not exist").is_none());
    }
}

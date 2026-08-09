//! Student number generation.
//!
//! The format is configuration, not code. The *sequence* is the part that must
//! be safe: two operators activating students at the same moment must never
//! receive the same number. That guarantee belongs to a transactional allocator
//! (see `infrastructure::postgres`); `format_student_number` is the pure part.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NumberToken {
    Prefix,
    Year,
    Department,
    Program,
    Sequence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentNumberFormat {
    /// Ordered tokens, e.g. `[Year, Department, Sequence]`.
    pub pattern: Vec<NumberToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    /// Zero-padding width for the sequence component.
    pub sequence_width: usize,
}

impl Default for StudentNumberFormat {
    /// `2026CSE001`.
    fn default() -> Self {
        Self {
            pattern: vec![
                NumberToken::Year,
                NumberToken::Department,
                NumberToken::Sequence,
            ],
            prefix: None,
            separator: Some(String::new()),
            sequence_width: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudentNumberInput {
    pub year: String,
    pub department_code: Option<String>,
    pub program_code: Option<String>,
    pub sequence: u64,
}

/// `2026CSE001` with the default format; `SC/26/CS/001` with a separator.
pub fn format_student_number(input: &StudentNumberInput, format: &StudentNumberFormat) -> String {
    let parts: Vec<String> = format
        .pattern
        .iter()
        .map(|token| match token {
            NumberToken::Prefix => format.prefix.clone().unwrap_or_default(),
            NumberToken::Year => input.year.clone(),
            NumberToken::Department => input.department_code.clone().unwrap_or_default(),
            NumberToken::Program => input.program_code.clone().unwrap_or_default(),
            NumberToken::Sequence => {
                format!("{:0width$}", input.sequence, width = format.sequence_width)
            }
        })
        .filter(|part| !part.is_empty())
        .collect();

    parts.join(format.separator.as_deref().unwrap_or(""))
}

/// Sequences restart per tenant, year and department.
pub fn sequence_scope(tenant_id: &str, year: &str, department_code: &str) -> String {
    let department = if department_code.is_empty() {
        "GEN"
    } else {
        department_code
    };
    format!("{tenant_id}:{year}:{department}")
}

/// Normalise a department id such as `dept-cse` into a number component `CSE`.
pub fn department_code(department_id: Option<&str>) -> String {
    department_id
        .map(|value| value.trim_start_matches("dept-").to_uppercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GEN".into())
}

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use super::CrmError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryStage {
    Enquiry,
    ContactAttempted,
    Contacted,
    Nurture,
    Qualified,
    Application,
    ApplicationStatus,
    OfferStatus,
    Archived,
}

impl PrimaryStage {
    pub const ALL: [Self; 9] = [
        Self::Enquiry,
        Self::ContactAttempted,
        Self::Contacted,
        Self::Nurture,
        Self::Qualified,
        Self::Application,
        Self::ApplicationStatus,
        Self::OfferStatus,
        Self::Archived,
    ];

    pub const fn order(self) -> u8 {
        match self {
            Self::Enquiry => 1,
            Self::ContactAttempted => 2,
            Self::Contacted => 3,
            Self::Nurture => 4,
            Self::Qualified => 5,
            Self::Application => 6,
            Self::ApplicationStatus => 7,
            Self::OfferStatus => 8,
            Self::Archived => 9,
        }
    }

    pub const fn default_substate(self) -> &'static str {
        match self {
            Self::Enquiry => "new",
            Self::ContactAttempted => "contacted",
            Self::Contacted => "nurture",
            Self::Nurture => "qualified",
            Self::Qualified => "converted",
            Self::Application => "to_do",
            Self::ApplicationStatus => "awaiting_decision",
            Self::OfferStatus => "to_do",
            Self::Archived => "closed",
        }
    }

    pub fn substates(self) -> &'static [&'static str] {
        match self {
            Self::Enquiry => &[
                "new",
                "contact_attempted",
                "contacted",
                "nurture",
                "qualified",
                "converted",
            ],
            Self::ContactAttempted => &["contacted", "nurture", "qualified", "converted"],
            Self::Contacted => &["nurture", "qualified", "converted"],
            Self::Nurture => &["qualified", "converted"],
            Self::Qualified => &["converted"],
            Self::Application => &[
                "to_do",
                "application_in_progress",
                "documents_required",
                "application_fee_pending",
                "application_not_open",
                "technical_issue",
                "application_submitted",
            ],
            Self::ApplicationStatus => &[
                "awaiting_decision",
                "documents_required",
                "interview_to_be_scheduled",
                "interview_scheduled",
                "waitlisted",
                "unconditional_offer",
            ],
            Self::OfferStatus => &["to_do", "accepted", "rejected"],
            Self::Archived => &["closed"],
        }
    }

    pub fn validate_substate(self, substate: &str) -> Result<(), CrmError> {
        if self.substates().contains(&substate) {
            Ok(())
        } else {
            Err(CrmError::Validation(format!(
                "{substate} is not a valid substate for {self}"
            )))
        }
    }
}

impl fmt::Display for PrimaryStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Enquiry => "enquiry",
            Self::ContactAttempted => "contact_attempted",
            Self::Contacted => "contacted",
            Self::Nurture => "nurture",
            Self::Qualified => "qualified",
            Self::Application => "application",
            Self::ApplicationStatus => "application_status",
            Self::OfferStatus => "offer_status",
            Self::Archived => "archived",
        };
        formatter.write_str(value)
    }
}

impl FromStr for PrimaryStage {
    type Err = CrmError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "enquiry" => Ok(Self::Enquiry),
            "contact_attempted" => Ok(Self::ContactAttempted),
            "contacted" => Ok(Self::Contacted),
            "nurture" => Ok(Self::Nurture),
            "qualified" => Ok(Self::Qualified),
            "application" => Ok(Self::Application),
            "application_status" => Ok(Self::ApplicationStatus),
            "offer_status" => Ok(Self::OfferStatus),
            "archived" | "archive" => Ok(Self::Archived),
            _ => Err(CrmError::Validation(format!("unknown CRM stage: {value}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalStatus {
    Prospect,
    Deferred,
    OnHold,
    Archive,
}

impl fmt::Display for GlobalStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Prospect => "prospect",
            Self::Deferred => "deferred",
            Self::OnHold => "on_hold",
            Self::Archive => "archive",
        })
    }
}

pub fn validate_transition(
    from_stage: PrimaryStage,
    from_substate: &str,
    to_stage: PrimaryStage,
    to_substate: &str,
) -> Result<(), CrmError> {
    to_stage.validate_substate(to_substate)?;
    if from_stage == PrimaryStage::Archived {
        return Err(CrmError::Validation(
            "archived leads can only move through the unarchive action".into(),
        ));
    }
    if from_stage == to_stage {
        return validate_substate_transition(from_stage, from_substate, to_substate);
    }

    if to_stage.order() < from_stage.order() {
        return Ok(());
    }

    let valid = match (from_stage, to_stage) {
        (
            PrimaryStage::Enquiry,
            PrimaryStage::ContactAttempted | PrimaryStage::Contacted | PrimaryStage::Nurture,
        ) => true,
        (PrimaryStage::ContactAttempted, PrimaryStage::Contacted | PrimaryStage::Nurture) => true,
        (PrimaryStage::Contacted, PrimaryStage::Nurture | PrimaryStage::Qualified) => true,
        (PrimaryStage::Nurture, PrimaryStage::Qualified) => true,
        (PrimaryStage::Qualified, PrimaryStage::Application) => to_substate == "to_do",
        (PrimaryStage::Application, PrimaryStage::ApplicationStatus) => {
            to_substate == "awaiting_decision"
        }
        (PrimaryStage::ApplicationStatus, PrimaryStage::OfferStatus) => to_substate == "to_do",
        (_, PrimaryStage::Archived) => true,
        _ => false,
    };

    if valid {
        Ok(())
    } else {
        Err(CrmError::Validation(format!(
            "invalid CRM transition: {from_stage}/{from_substate} -> {to_stage}/{to_substate}"
        )))
    }
}

fn validate_substate_transition(stage: PrimaryStage, from: &str, to: &str) -> Result<(), CrmError> {
    if from == to {
        return Ok(());
    }
    let allowed: &[&str] = match (stage, from) {
        (PrimaryStage::Enquiry, "new") => &["contact_attempted", "contacted", "nurture"],
        (PrimaryStage::Enquiry, "contact_attempted") => &["contacted", "nurture"],
        (PrimaryStage::Enquiry, "contacted") => &["nurture", "qualified"],
        (PrimaryStage::Enquiry, "nurture") => &["qualified", "converted"],
        (PrimaryStage::Enquiry, "qualified") => &["converted"],
        (PrimaryStage::ContactAttempted, "contacted") => &["nurture", "qualified", "converted"],
        (PrimaryStage::ContactAttempted, "nurture") => &["qualified", "converted"],
        (PrimaryStage::ContactAttempted, "qualified") => &["converted"],
        (PrimaryStage::Contacted, "nurture") => &["qualified", "converted"],
        (PrimaryStage::Contacted, "qualified") => &["converted"],
        (PrimaryStage::Nurture, "qualified") => &["converted"],
        (PrimaryStage::Application, "to_do") => &[
            "application_in_progress",
            "documents_required",
            "application_fee_pending",
            "application_not_open",
            "technical_issue",
        ],
        (PrimaryStage::Application, "application_in_progress") => &[
            "documents_required",
            "application_fee_pending",
            "application_submitted",
        ],
        (PrimaryStage::Application, "documents_required" | "application_fee_pending") => {
            &["application_submitted"]
        }
        (PrimaryStage::Application, "application_not_open" | "technical_issue") => &["to_do"],
        (PrimaryStage::ApplicationStatus, "awaiting_decision") => &[
            "documents_required",
            "interview_to_be_scheduled",
            "waitlisted",
            "unconditional_offer",
        ],
        (PrimaryStage::ApplicationStatus, "documents_required") => &["awaiting_decision"],
        (PrimaryStage::ApplicationStatus, "interview_to_be_scheduled") => &["interview_scheduled"],
        (PrimaryStage::ApplicationStatus, "interview_scheduled") => {
            &["awaiting_decision", "unconditional_offer"]
        }
        (PrimaryStage::ApplicationStatus, "waitlisted") => &["unconditional_offer"],
        (PrimaryStage::OfferStatus, "to_do") => &["accepted", "rejected"],
        _ => &[],
    };
    if allowed.contains(&to) {
        Ok(())
    } else {
        Err(CrmError::Validation(format!(
            "invalid {stage} substate transition: {from} -> {to}"
        )))
    }
}

pub fn normalize(value: &str) -> String {
    value.trim().to_lowercase().replace([' ', '-', '/'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_submission_can_advance_to_review() {
        assert!(
            validate_transition(
                PrimaryStage::Application,
                "application_submitted",
                PrimaryStage::ApplicationStatus,
                "awaiting_decision"
            )
            .is_ok()
        );
    }

    #[test]
    fn archived_stage_is_terminal() {
        assert!(
            validate_transition(
                PrimaryStage::Archived,
                "closed",
                PrimaryStage::Enquiry,
                "new"
            )
            .is_err()
        );
    }
}

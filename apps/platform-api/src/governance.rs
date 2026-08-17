//! Separation-of-duty policy for sensitive tenant operations.
//!
//! Permission grants remain necessary. These rules are an additional boundary
//! that prevents configurable RBAC from assigning a reserved approval to the
//! wrong institutional authority.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GovernedCapability {
    FeeApproval,
    RefundPreparation,
    RefundApproval,
    ResultPublicationApproval,
    StudentSuspension,
    AdmissionApproval,
    AcademicAssignmentManagement,
    TimetableManagement,
    FacultySubstitutionApproval,
}

pub fn role_may_perform(role_key: &str, capability: GovernedCapability) -> bool {
    let role_key = role_key.trim().to_ascii_lowercase();

    match capability {
        GovernedCapability::FeeApproval
        | GovernedCapability::ResultPublicationApproval
        | GovernedCapability::StudentSuspension => role_key == "principal",
        GovernedCapability::RefundPreparation => {
            matches!(role_key.as_str(), "accountant" | "management")
        }
        GovernedCapability::RefundApproval => role_key == "management",
        GovernedCapability::AdmissionApproval => {
            matches!(role_key.as_str(), "admissions_officer" | "management")
        }
        GovernedCapability::AcademicAssignmentManagement => {
            matches!(role_key.as_str(), "principal" | "academic_administrator")
        }
        GovernedCapability::TimetableManagement => {
            matches!(role_key.as_str(), "principal" | "academic_administrator")
        }
        GovernedCapability::FacultySubstitutionApproval => role_key == "principal",
    }
}

pub fn any_role_may_perform<'a>(
    role_keys: impl IntoIterator<Item = &'a str>,
    capability: GovernedCapability,
) -> bool {
    role_keys
        .into_iter()
        .any(|role_key| role_may_perform(role_key, capability))
}

#[cfg(test)]
mod tests {
    use super::{GovernedCapability, any_role_may_perform, role_may_perform};

    #[test]
    fn principal_approvals_exclude_admissions_and_refunds() {
        assert!(role_may_perform(
            "principal",
            GovernedCapability::FeeApproval
        ));
        assert!(role_may_perform(
            "principal",
            GovernedCapability::ResultPublicationApproval
        ));
        assert!(role_may_perform(
            "principal",
            GovernedCapability::StudentSuspension
        ));
        assert!(!role_may_perform(
            "principal",
            GovernedCapability::AdmissionApproval
        ));
        assert!(!role_may_perform(
            "principal",
            GovernedCapability::RefundApproval
        ));
    }

    #[test]
    fn refund_preparation_and_approval_are_separated() {
        assert!(role_may_perform(
            "accountant",
            GovernedCapability::RefundPreparation
        ));
        assert!(!role_may_perform(
            "accountant",
            GovernedCapability::RefundApproval
        ));
        assert!(role_may_perform(
            "management",
            GovernedCapability::RefundApproval
        ));
    }

    #[test]
    fn academic_assignments_have_two_authorities() {
        assert!(any_role_may_perform(
            ["faculty", "academic_administrator"],
            GovernedCapability::AcademicAssignmentManagement,
        ));
        assert!(!any_role_may_perform(
            ["faculty", "hod"],
            GovernedCapability::AcademicAssignmentManagement,
        ));
    }

    #[test]
    fn timetable_configuration_and_substitution_decisions_are_separated() {
        assert!(role_may_perform(
            "academic_administrator",
            GovernedCapability::TimetableManagement,
        ));
        assert!(role_may_perform(
            "principal",
            GovernedCapability::FacultySubstitutionApproval,
        ));
        assert!(!role_may_perform(
            "academic_administrator",
            GovernedCapability::FacultySubstitutionApproval,
        ));
    }
}

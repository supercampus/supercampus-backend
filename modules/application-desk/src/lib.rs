#![forbid(unsafe_code)]

//! SuperCampus Application Desk.
//!
//! The bridge between Admissions and the operational system: it converts a
//! confirmed admitted applicant into a student, a user account and module
//! access. It owns exactly one entity — the `OnboardingCase` — and references
//! everything else by id.

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct ApplicationDeskModule;

/// Every action is gated on its own permission so verification, approval and
/// activation can sit with different teams.
pub const PERMISSIONS: [&str; 12] = [
    "application-desk.view",
    "application-desk.create",
    "application-desk.edit",
    "application-desk.verify",
    "application-desk.assign",
    "application-desk.approve",
    "application-desk.reject",
    "application-desk.hold",
    "application-desk.resume",
    "application-desk.activate",
    "application-desk.manage_settings",
    "application-desk.reports.read",
];

impl PlatformModule for ApplicationDeskModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "application-desk".into(),
            name: "Application Desk".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: PERMISSIONS.iter().map(|value| (*value).into()).collect(),
            capabilities: vec![
                "onboarding-orchestration".into(),
                "configurable-workflow".into(),
                "duplicate-protection".into(),
                "transactional-student-numbering".into(),
                "idempotent-provisioning".into(),
                "transactional-outbox".into(),
                "append-only-audit".into(),
            ],
        }
    }
}

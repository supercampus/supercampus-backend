#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct FeesModule;

impl PlatformModule for FeesModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "fees".into(),
            name: "Fees".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "fees.read".into(),
                "fees.create".into(),
                "fees.update".into(),
                "fees.configure".into(),
                "fees.reports.read".into(),
            ],
            capabilities: vec![
                "fee-plans".into(),
                "invoices".into(),
                "payments".into(),
                "refunds".into(),
                "scholarships".into(),
                "reports".into(),
            ],
        }
    }
}

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct GatepassModule;

impl PlatformModule for GatepassModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "gatepass".into(),
            name: "Gate Pass".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "gatepass.read".into(),
                "gatepass.create".into(),
                "gatepass.update".into(),
                "gatepass.configure".into(),
                "gatepass.reports.read".into(),
            ],
            capabilities: vec![
                "passes".into(),
                "approvals".into(),
                "qr".into(),
                "scans".into(),
                "geofences".into(),
                "overrides".into(),
            ],
        }
    }
}

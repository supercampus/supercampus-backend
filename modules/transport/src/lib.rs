#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct TransportModule;

impl PlatformModule for TransportModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "transport".into(),
            name: "Transport".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "transport.read".into(),
                "transport.create".into(),
                "transport.update".into(),
                "transport.configure".into(),
                "transport.reports.read".into(),
            ],
            capabilities: vec![
                "routes".into(),
                "vehicles".into(),
                "stops".into(),
                "assignments".into(),
                "tracking".into(),
                "maintenance".into(),
            ],
        }
    }
}

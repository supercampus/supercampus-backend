#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct PlacementModule;

impl PlatformModule for PlacementModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "placement".into(),
            name: "Placement".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "placement.read".into(),
                "placement.create".into(),
                "placement.update".into(),
                "placement.configure".into(),
                "placement.reports.read".into(),
            ],
            capabilities: vec![
                "employers".into(),
                "drives".into(),
                "jobs".into(),
                "applications".into(),
                "offers".into(),
                "reports".into(),
            ],
        }
    }
}

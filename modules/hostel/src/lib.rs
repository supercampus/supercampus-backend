#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct HostelModule;

impl PlatformModule for HostelModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "hostel".into(),
            name: "Hostel".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "hostel.read".into(),
                "hostel.create".into(),
                "hostel.update".into(),
                "hostel.configure".into(),
                "hostel.reports.read".into(),
            ],
            capabilities: vec![
                "buildings".into(),
                "rooms".into(),
                "allocations".into(),
                "residents".into(),
                "visitors".into(),
                "maintenance".into(),
            ],
        }
    }
}

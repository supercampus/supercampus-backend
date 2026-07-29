#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct AcademicsModule;

impl PlatformModule for AcademicsModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "academics".into(),
            name: "Academics".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "academics.read".into(),
                "academics.create".into(),
                "academics.update".into(),
                "academics.configure".into(),
                "academics.reports.read".into(),
            ],
            capabilities: vec![
                "programs".into(),
                "courses".into(),
                "curriculum".into(),
                "classes".into(),
                "faculty".into(),
                "timetable".into(),
            ],
        }
    }
}

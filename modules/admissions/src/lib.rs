#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct AdmissionsModule;

impl PlatformModule for AdmissionsModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "admissions".into(),
            name: "Admissions".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "admissions.read".into(),
                "admissions.create".into(),
                "admissions.update".into(),
                "admissions.configure".into(),
                "admissions.reports.read".into(),
            ],
            capabilities: vec![
                "applications".into(),
                "applicants".into(),
                "programs".into(),
                "intakes".into(),
                "counseling".into(),
                "offers".into(),
                "enrollment".into(),
            ],
        }
    }
}

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct ExaminationsModule;

impl PlatformModule for ExaminationsModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "examinations".into(),
            name: "Examinations".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "examinations.read".into(),
                "examinations.create".into(),
                "examinations.update".into(),
                "examinations.configure".into(),
                "examinations.reports.read".into(),
            ],
            capabilities: vec![
                "assessments".into(),
                "schedules".into(),
                "grading".into(),
                "results".into(),
                "transcripts".into(),
            ],
        }
    }
}

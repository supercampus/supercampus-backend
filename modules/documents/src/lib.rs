#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct DocumentsModule;

impl PlatformModule for DocumentsModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "documents".into(),
            name: "Documents".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "documents.read".into(),
                "documents.create".into(),
                "documents.update".into(),
                "documents.configure".into(),
                "documents.reports.read".into(),
            ],
            capabilities: vec![
                "files".into(),
                "folders".into(),
                "templates".into(),
                "verification".into(),
                "retention".into(),
            ],
        }
    }
}

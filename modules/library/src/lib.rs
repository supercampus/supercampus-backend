#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct LibraryModule;

impl PlatformModule for LibraryModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "library".into(),
            name: "Library".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "library.read".into(),
                "library.create".into(),
                "library.update".into(),
                "library.configure".into(),
                "library.reports.read".into(),
            ],
            capabilities: vec![
                "catalog".into(),
                "copies".into(),
                "loans".into(),
                "holds".into(),
                "members".into(),
                "fines".into(),
            ],
        }
    }
}

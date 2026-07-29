#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct AttendanceModule;

impl PlatformModule for AttendanceModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "attendance".into(),
            name: "Attendance".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "attendance.read".into(),
                "attendance.create".into(),
                "attendance.update".into(),
                "attendance.configure".into(),
                "attendance.reports.read".into(),
            ],
            capabilities: vec![
                "sessions".into(),
                "records".into(),
                "policies".into(),
                "exceptions".into(),
                "reports".into(),
            ],
        }
    }
}

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod jobs;

use supercampus_module_sdk::{ModuleManifest, PlatformModule};

pub struct CrmModule;

impl PlatformModule for CrmModule {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            key: "crm".into(),
            name: "SuperCampus CRM".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            permissions: vec![
                "crm.leads.read".into(),
                "crm.leads.create".into(),
                "crm.leads.update".into(),
                "crm.pipelines.configure".into(),
                "crm.reports.read".into(),
            ],
            capabilities: vec![
                "leads".into(),
                "contacts".into(),
                "organizations".into(),
                "pipelines".into(),
                "opportunities".into(),
                "activities".into(),
                "tasks".into(),
                "communications".into(),
                "campaigns".into(),
                "automations".into(),
                "dashboards".into(),
                "reports".into(),
            ],
        }
    }
}

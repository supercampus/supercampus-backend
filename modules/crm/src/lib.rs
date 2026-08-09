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
                "crm.leads.import".into(),
                "crm.leads.update".into(),
                "crm.leads.delete".into(),
                "crm.leads.assign".into(),
                "crm.leads.claim".into(),
                "crm.leads.stage.move".into(),
                "crm.leads.stage.request".into(),
                "crm.leads.stage.approve".into(),
                "crm.leads.stage.override".into(),
                "crm.leads.stage.backward".into(),
                "crm.leads.hold".into(),
                "crm.leads.hold.release".into(),
                "crm.leads.archive".into(),
                "crm.leads.unarchive".into(),
                "crm.forms.read".into(),
                "crm.forms.create".into(),
                "crm.forms.update".into(),
                "crm.forms.delete".into(),
                "crm.forms.publish".into(),
                "crm.forms.submit".into(),
                "crm.forms.submissions.read".into(),
                "crm.communications.send".into(),
                "crm.templates.read".into(),
                "crm.templates.create".into(),
                "crm.templates.update".into(),
                "crm.assignment.read".into(),
                "crm.assignment.create".into(),
                "crm.assignment.update".into(),
                "crm.configuration.read".into(),
                "crm.configuration.create".into(),
                "crm.configuration.update".into(),
                "crm.dashboard.read".into(),
                "crm.erp.handoff".into(),
                "crm.reports.read".into(),
                "crm.campaigns.read".into(),
                "crm.campaigns.create".into(),
                "crm.campaigns.update".into(),
            ],
            capabilities: vec![
                "lead-capture".into(),
                "weighted-assignment".into(),
                "pre-admission-pipeline".into(),
                "kanban-dashboard".into(),
                "dynamic-forms".into(),
                "communications".into(),
                "archive-and-hold".into(),
                "erp-handoff".into(),
                "role-scoped-access".into(),
                "tenant-workflow-configuration".into(),
                "transactional-outbox".into(),
                "permission-audit".into(),
            ],
        }
    }
}

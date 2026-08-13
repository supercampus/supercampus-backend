use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};

use chrono::Utc;
use serde_json::{Value, json};
use supercampus_database::Database;
use uuid::Uuid;

use crate::{
    api::dto::{
        ArchiveRequest, AssignLeadRequest, AutomationToggleRequest, BulkImportLeadResult,
        BulkImportLeadsRequest, BulkImportLeadsResponse, ClaimLeadRequest,
        CounselorCapacityRequest, CreateCampaignRequest, CreateFormRequest, CreateLeadNoteRequest,
        CreateLeadRequest, CreateLeadTaskRequest, CreateTemplateRequest, HoldRequest,
        IntakeStatusRequest, LeadFilters, MoveRequestDecision, MoveStageRequest, ReasonRequest,
        SendCommunicationRequest, SubmitFormRequest, TransferLeadRequest, UnarchiveRequest,
        UpdateFormRequest, UpdateLeadRequest, WorkflowToggleRequest,
    },
    domain::{
        Campaign, CrmError, FormDefinition, Lead, LeadMoveRequest, PipelineTransferCandidate,
        PrimaryStage, canonical_lead_source, validate_transition,
    },
    infrastructure::postgres::{LeadPatch, NewLead, PostgresCrmRepository},
};

#[derive(Debug, Clone)]
pub struct ActorContext {
    pub user_id: String,
    pub roles: Vec<String>,
    pub permissions: HashSet<String>,
    pub permission_scopes: HashMap<String, String>,
    pub public: bool,
    pub ip_address: Option<String>,
}

impl ActorContext {
    pub fn primary_role(&self) -> &str {
        self.roles.first().map_or(
            if self.public { "public" } else { "unassigned" },
            String::as_str,
        )
    }

    pub fn has(&self, permission: &str) -> bool {
        self.permissions.contains("*") || self.permissions.contains(permission)
    }

    pub fn has_any(&self, permissions: &[&str]) -> bool {
        permissions.iter().any(|permission| self.has(permission))
    }

    pub fn is_administrator(&self) -> bool {
        self.permissions.contains("*")
            || self.roles.iter().any(|role| {
                let normalized = role.to_ascii_lowercase().replace('_', "-");
                normalized == "admin"
                    || normalized.ends_with("-admin")
                    || normalized.ends_with("-administrator")
            })
    }

    pub fn scope_for(&self, permission: &str) -> &str {
        if self.permissions.contains("*") {
            "all"
        } else {
            self.permission_scopes
                .get(permission)
                .map_or("all", String::as_str)
        }
    }

    pub fn has_all_scope(&self, permission: &str) -> bool {
        self.has(permission) && self.scope_for(permission) == "all"
    }

    pub fn can_access_assigned(&self, permission: &str, assigned_to: Option<&str>) -> bool {
        self.has(permission)
            && (self.scope_for(permission) == "all" || assigned_to == Some(self.user_id.as_str()))
    }
}

#[derive(Clone)]
pub struct CrmService {
    repository: Option<PostgresCrmRepository>,
}

#[derive(Default)]
struct SourcePerformance {
    leads: usize,
    applications: usize,
    budget: f64,
    spent: f64,
    attributed_revenue: f64,
}

impl CrmService {
    pub fn new(database: Option<Database>) -> Self {
        Self {
            repository: database.map(PostgresCrmRepository::new),
        }
    }

    pub async fn create_lead(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: CreateLeadRequest,
    ) -> Result<Lead, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.create",
            actor.has("crm.leads.create"),
            None,
        )
        .await?;
        if request.student.name.trim().is_empty() {
            return Err(CrmError::Validation("student name is required".into()));
        }
        if request.student.phone.is_none() && request.student.email.is_none() {
            return Err(CrmError::Validation(
                "at least one of student phone or email is required".into(),
            ));
        }
        let priority = request.priority.unwrap_or_else(|| "medium".into());
        validate_priority(&priority)?;
        let source = canonical_lead_source(&request.source)?;
        self.repo()?
            .create_lead(
                tenant,
                &actor.user_id,
                actor.primary_role(),
                NewLead {
                    source,
                    source_detail: request.source_detail,
                    full_name: request.student.name,
                    email: request.student.email,
                    phone: request.student.phone,
                    whatsapp: request.student.whatsapp,
                    parent_name: request.student.parent_name,
                    parent_phone: request.student.parent_phone,
                    academic: request.academic,
                    interest: request.interest,
                    priority,
                    follow_up_at: request.follow_up_at,
                    preferred_channel: request.communication.preferred_channel,
                    consent_given: request.communication.consent_given,
                    custom_fields: request.custom_fields,
                },
            )
            .await
    }

    pub async fn import_leads(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: BulkImportLeadsRequest,
    ) -> Result<BulkImportLeadsResponse, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.import",
            actor.has("crm.leads.import"),
            None,
        )
        .await?;
        if request.rows.is_empty() {
            return Err(CrmError::Validation(
                "at least one import row is required".into(),
            ));
        }
        if request.rows.len() > 1_000 {
            return Err(CrmError::Validation(
                "a single import is limited to 1000 rows".into(),
            ));
        }
        let duplicate_strategy = request
            .duplicate_strategy
            .as_deref()
            .unwrap_or("skip")
            .trim()
            .to_ascii_lowercase();
        if !matches!(duplicate_strategy.as_str(), "skip" | "flag") {
            return Err(CrmError::Validation(
                "duplicateStrategy must be skip or flag".into(),
            ));
        }

        let repository = self.repo()?;
        let total = request.rows.len();
        let mut created = 0;
        let mut skipped = 0;
        let mut failed = 0;
        let mut results = Vec::with_capacity(total);
        let mut imported_contacts = HashMap::<String, usize>::new();

        for row in request.rows {
            let row_number = row.row_number;
            let mut lead = row.lead;
            lead.student.name = lead.student.name.trim().to_owned();
            lead.student.email = normalized_optional(lead.student.email, true);
            lead.student.phone = normalized_optional(lead.student.phone, false);
            lead.student.whatsapp = normalized_optional(lead.student.whatsapp, false);
            lead.student.parent_name = normalized_optional(lead.student.parent_name, false);
            lead.student.parent_phone = normalized_optional(lead.student.parent_phone, false);
            lead.source = lead.source.trim().to_owned();

            if lead.student.name.is_empty()
                || (lead.student.phone.is_none() && lead.student.email.is_none())
            {
                failed += 1;
                results.push(BulkImportLeadResult {
                    row_number,
                    status: "failed".into(),
                    lead_id: None,
                    duplicate_of: None,
                    message: Some("student name and phone or email are required".into()),
                });
                continue;
            }
            let priority = lead.priority.unwrap_or_else(|| "medium".into());
            if let Err(error) = validate_priority(&priority) {
                failed += 1;
                results.push(BulkImportLeadResult {
                    row_number,
                    status: "failed".into(),
                    lead_id: None,
                    duplicate_of: None,
                    message: Some(error.to_string()),
                });
                continue;
            }
            let source = if lead.source.is_empty() {
                "Other".to_owned()
            } else {
                match canonical_lead_source(&lead.source) {
                    Ok(source) => source,
                    Err(error) => {
                        failed += 1;
                        results.push(BulkImportLeadResult {
                            row_number,
                            status: "failed".into(),
                            lead_id: None,
                            duplicate_of: None,
                            message: Some(error.to_string()),
                        });
                        continue;
                    }
                }
            };

            let contact_keys =
                import_contact_keys(lead.student.phone.as_deref(), lead.student.email.as_deref());
            let duplicate_row = contact_keys
                .iter()
                .find_map(|key| imported_contacts.get(key).copied());
            if duplicate_strategy == "skip"
                && let Some(original_row) = duplicate_row
            {
                skipped += 1;
                results.push(BulkImportLeadResult {
                    row_number,
                    status: "skipped".into(),
                    lead_id: None,
                    duplicate_of: None,
                    message: Some(format!("duplicate of CSV row {original_row}")),
                });
                continue;
            }

            let duplicate_of = repository
                .find_duplicate_lead(
                    tenant,
                    lead.student.phone.as_deref(),
                    lead.student.email.as_deref(),
                )
                .await?;
            if duplicate_strategy == "skip" && duplicate_of.is_some() {
                skipped += 1;
                results.push(BulkImportLeadResult {
                    row_number,
                    status: "skipped".into(),
                    lead_id: None,
                    duplicate_of,
                    message: Some("an existing tenant lead has the same phone or email".into()),
                });
                continue;
            }

            let new_lead = NewLead {
                source,
                source_detail: lead.source_detail,
                full_name: lead.student.name,
                email: lead.student.email,
                phone: lead.student.phone,
                whatsapp: lead.student.whatsapp,
                parent_name: lead.student.parent_name,
                parent_phone: lead.student.parent_phone,
                academic: lead.academic,
                interest: lead.interest,
                priority,
                follow_up_at: lead.follow_up_at,
                preferred_channel: lead.communication.preferred_channel,
                consent_given: lead.communication.consent_given,
                custom_fields: lead.custom_fields,
            };
            match repository
                .create_lead(tenant, &actor.user_id, actor.primary_role(), new_lead)
                .await
            {
                Ok(created_lead) => {
                    for key in contact_keys {
                        imported_contacts.entry(key).or_insert(row_number);
                    }
                    created += 1;
                    results.push(BulkImportLeadResult {
                        row_number,
                        status: "created".into(),
                        lead_id: Some(created_lead.id),
                        duplicate_of: created_lead.duplicate_of,
                        message: created_lead
                            .duplicate_of
                            .map(|_| "imported and flagged as a duplicate".into()),
                    });
                }
                Err(error) => {
                    tracing::error!(row_number, error = %error, "CRM lead import row failed");
                    failed += 1;
                    results.push(BulkImportLeadResult {
                        row_number,
                        status: "failed".into(),
                        lead_id: None,
                        duplicate_of: None,
                        message: Some("the row could not be stored".into()),
                    });
                }
            }
        }

        Ok(BulkImportLeadsResponse {
            total,
            created,
            skipped,
            failed,
            rows: results,
        })
    }

    pub async fn list_leads(
        &self,
        tenant: &str,
        actor: &ActorContext,
        filters: &LeadFilters,
    ) -> Result<Vec<Lead>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.read",
            actor.has("crm.leads.read"),
            None,
        )
        .await?;
        let owner_scope =
            (!actor.has_all_scope("crm.leads.read")).then_some(actor.user_id.as_str());
        self.repo()?
            .list_leads(tenant, filters, owner_scope, false)
            .await
    }

    pub async fn unassigned_leads(
        &self,
        tenant: &str,
        actor: &ActorContext,
        mut filters: LeadFilters,
    ) -> Result<Vec<Lead>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.claim",
            actor.has("crm.leads.claim") && actor.has("crm.leads.read"),
            None,
        )
        .await?;
        filters.stage = Some("enquiry".into());
        filters.owner = None;
        filters.unassigned = Some(true);
        self.repo()?.list_leads(tenant, &filters, None, false).await
    }

    pub async fn get_lead(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        let allowed = actor.can_access_assigned("crm.leads.read", lead.assigned_to.as_deref());
        self.require(tenant, actor, "crm.leads.read", allowed, Some(lead_id))
            .await?;
        Ok(lead)
    }

    pub async fn update_lead(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: UpdateLeadRequest,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        let allowed = actor.can_access_assigned("crm.leads.update", lead.assigned_to.as_deref());
        self.require(tenant, actor, "crm.leads.update", allowed, Some(lead_id))
            .await?;
        if let Some(priority) = request.priority.as_deref() {
            validate_priority(priority)?;
        }
        let source = request
            .source
            .as_deref()
            .map(canonical_lead_source)
            .transpose()?;
        self.repo()?
            .update_lead(
                tenant,
                lead_id,
                LeadPatch {
                    source,
                    full_name: request.full_name,
                    email: request.email,
                    phone: request.phone,
                    whatsapp: request.whatsapp,
                    parent_name: request.parent_name,
                    parent_phone: request.parent_phone,
                    academic: request.academic,
                    interest: request.interest,
                    priority: request.priority,
                    follow_up_at: request.follow_up_at,
                    fee_payment_confirmed: request.fee_payment_confirmed,
                    documents_verified: request.documents_verified,
                    scholarship_status: request.scholarship_status,
                    custom_fields: request.custom_fields,
                },
            )
            .await
    }

    pub async fn delete_lead(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
    ) -> Result<(), CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        self.require(
            tenant,
            actor,
            "crm.leads.delete",
            actor.can_access_assigned("crm.leads.delete", lead.assigned_to.as_deref()),
            Some(lead_id),
        )
        .await?;
        self.repo()?.soft_delete(tenant, lead_id).await
    }

    pub async fn assign(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: AssignLeadRequest,
        reassignment: bool,
    ) -> Result<Lead, CrmError> {
        self.repo()?.find_lead(tenant, lead_id).await?;
        self.require(
            tenant,
            actor,
            "crm.leads.assign",
            actor.has("crm.leads.assign"),
            Some(lead_id),
        )
        .await?;
        if reassignment && request.reason.as_deref().unwrap_or("").trim().is_empty() {
            return Err(CrmError::Validation(
                "reassignment reason is required".into(),
            ));
        }
        self.repo()?
            .assign(
                tenant,
                lead_id,
                &request.user_id,
                if reassignment {
                    "reassignment"
                } else {
                    "manual"
                },
                request.reason.as_deref(),
                &actor.user_id,
            )
            .await
    }

    pub async fn claim(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        _request: ClaimLeadRequest,
    ) -> Result<Lead, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.claim",
            actor.has("crm.leads.claim"),
            Some(lead_id),
        )
        .await?;
        self.repo()?.find_lead(tenant, lead_id).await?;
        Err(CrmError::Conflict(
            "move the Enquiry to its next stage to claim it".into(),
        ))
    }

    pub async fn transfer_candidates(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<PipelineTransferCandidate>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.read",
            actor.has("crm.leads.read"),
            None,
        )
        .await?;
        self.repo()?
            .pipeline_transfer_candidates(tenant, &actor.user_id)
            .await
    }

    pub async fn transfer_lead(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: TransferLeadRequest,
    ) -> Result<Lead, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.read",
            actor.has("crm.leads.read"),
            Some(lead_id),
        )
        .await?;
        let reason = request.reason.trim();
        if reason.is_empty() {
            return Err(CrmError::Validation("transfer reason is required".into()));
        }
        let new_owner = Uuid::parse_str(request.user_id.trim())
            .map_err(|_| CrmError::Validation("invalid transfer user".into()))?;
        self.repo()?
            .transfer_lead(tenant, lead_id, &actor.user_id, new_owner, reason)
            .await
    }

    pub async fn move_stage(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: MoveStageRequest,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        let is_owner = lead.assigned_to.as_deref() == Some(actor.user_id.as_str());
        let is_first_move = lead.assigned_to.is_none() && lead.stage_key == "enquiry";
        let can_override = actor.has("crm.leads.stage.override");
        let can_update =
            actor.has("crm.leads.stage.move") && (is_owner || is_first_move || can_override);
        let target = PrimaryStage::from_str(&request.to_stage)?;
        let target_substate = request
            .to_substate
            .unwrap_or_else(|| target.default_substate().into());
        if target == PrimaryStage::OfferStatus && target_substate == "accepted" {
            self.require(
                tenant,
                actor,
                "crm.erp.handoff",
                actor.has("crm.erp.handoff"),
                Some(lead_id),
            )
            .await?;
        }
        let current = PrimaryStage::from_str(&lead.stage_key)?;
        if target.order() < current.order()
            && !(actor.is_administrator()
                && (actor.has("crm.leads.stage.backward") || can_override))
        {
            return Err(CrmError::Validation(
                "only an administrator can move a lead backward".into(),
            ));
        }
        validate_transition(current, &lead.substate_key, target, &target_substate)?;
        let allowed = can_update;
        self.require(
            tenant,
            actor,
            "crm.leads.stage.move",
            allowed,
            Some(lead_id),
        )
        .await?;
        self.ensure_toggle_allows(tenant, actor, current, target)
            .await?;

        let repository = self.repo()?;
        let automation_trigger =
            if target == PrimaryStage::OfferStatus && target_substate == "accepted" {
                "offer_accepted"
            } else {
                "on_enter"
            };
        let automation_template = repository
            .enabled_communication_template(tenant, &target.to_string(), automation_trigger)
            .await?;
        let mut moved = repository
            .transition(
                tenant,
                lead_id,
                &target.to_string(),
                &target_substate,
                &actor.user_id,
                actor.primary_role(),
                request.reason.as_deref(),
                request.notes.as_deref(),
                actor.ip_address.as_deref(),
                automation_template.as_deref(),
                true,
                can_override,
            )
            .await?;

        if target == PrimaryStage::Application && target_substate == "application_submitted" {
            moved = repository
                .transition(
                    tenant,
                    lead_id,
                    "application_status",
                    "awaiting_decision",
                    "system",
                    "system",
                    Some("automatic_after_application_submission"),
                    None,
                    None,
                    Some("application_submitted"),
                    false,
                    true,
                )
                .await?;
        } else if target == PrimaryStage::ApplicationStatus
            && target_substate == "unconditional_offer"
        {
            moved = repository
                .transition(
                    tenant,
                    lead_id,
                    "offer_status",
                    "to_do",
                    "system",
                    "system",
                    Some("automatic_after_unconditional_offer"),
                    None,
                    None,
                    Some("offer_issued"),
                    false,
                    true,
                )
                .await?;
        } else if target == PrimaryStage::OfferStatus && target_substate == "rejected" {
            moved = repository
                .archive(
                    tenant,
                    lead_id,
                    "No Offer",
                    Some("automatic after offer rejection"),
                    "system",
                    "system",
                )
                .await?;
        }
        Ok(moved)
    }

    pub async fn request_stage_move(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: MoveStageRequest,
    ) -> Result<LeadMoveRequest, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.stage.request",
            actor.has("crm.leads.stage.request"),
            Some(lead_id),
        )
        .await?;
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        let current = PrimaryStage::from_str(&lead.stage_key)?;
        let target = PrimaryStage::from_str(&request.to_stage)?;
        let target_substate = request
            .to_substate
            .unwrap_or_else(|| target.default_substate().into());
        if target.order() < current.order()
            && !(actor.is_administrator() && actor.has("crm.leads.stage.backward"))
        {
            return Err(CrmError::Validation(
                "only an administrator can request a backward lead movement".into(),
            ));
        }
        validate_transition(current, &lead.substate_key, target, &target_substate)?;
        self.ensure_toggle_allows(tenant, actor, current, target)
            .await?;
        self.repo()?
            .create_move_request(
                tenant,
                lead_id,
                &actor.user_id,
                &target.to_string(),
                &target_substate,
                request.reason.as_deref(),
                request.notes.as_deref(),
            )
            .await
    }

    pub async fn move_requests(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<LeadMoveRequest>, CrmError> {
        let allowed = actor.has_any(&[
            "crm.leads.stage.request",
            "crm.leads.stage.approve",
            "crm.leads.stage.override",
        ]);
        self.require(tenant, actor, "crm.leads.stage.request", allowed, None)
            .await?;
        self.repo()?
            .list_move_requests(
                tenant,
                &actor.user_id,
                actor.has("crm.leads.stage.override"),
            )
            .await
    }

    pub async fn decide_stage_move(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request_id: Uuid,
        approve: bool,
        decision: MoveRequestDecision,
    ) -> Result<LeadMoveRequest, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.leads.stage.approve",
            actor.has("crm.leads.stage.approve"),
            None,
        )
        .await?;
        let requests = self
            .repo()?
            .list_move_requests(tenant, &actor.user_id, false)
            .await?;
        let movement = requests
            .into_iter()
            .find(|item| item.id == request_id)
            .ok_or(CrmError::NotFound)?;
        if approve {
            let current = PrimaryStage::from_str(&movement.from_stage)?;
            let target = PrimaryStage::from_str(&movement.to_stage)?;
            if target == PrimaryStage::OfferStatus && movement.to_substate == "accepted" {
                self.require(
                    tenant,
                    actor,
                    "crm.erp.handoff",
                    actor.has("crm.erp.handoff"),
                    Some(movement.lead_id),
                )
                .await?;
            }
            validate_transition(
                current,
                &movement.from_substate,
                target,
                &movement.to_substate,
            )?;
            self.ensure_toggle_allows(tenant, actor, current, target)
                .await?;
        }
        self.repo()?
            .decide_move_request(
                tenant,
                request_id,
                &actor.user_id,
                approve,
                decision.reason.as_deref(),
                actor.primary_role(),
            )
            .await
    }

    pub async fn prospect_or_defer(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        status: &str,
        request: IntakeStatusRequest,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        let allowed = actor.can_access_assigned("crm.leads.hold", lead.assigned_to.as_deref());
        self.require(tenant, actor, "crm.leads.hold", allowed, Some(lead_id))
            .await?;
        if status == "prospect" && !self.repo()?.has_reached_qualified(tenant, lead_id).await? {
            return Err(CrmError::Validation(
                "prospect status requires the lead to have reached Qualified".into(),
            ));
        }
        self.repo()?
            .set_intake_status(
                tenant,
                lead_id,
                status,
                json!({
                    "intakeYear": request.intake_year,
                    "intakeMonth": request.intake_month,
                    "programId": request.program_id,
                    "reason": request.reason
                }),
                &actor.user_id,
            )
            .await
    }

    pub async fn hold(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: HoldRequest,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        let allowed = actor.can_access_assigned("crm.leads.hold", lead.assigned_to.as_deref());
        self.require(tenant, actor, "crm.leads.hold", allowed, Some(lead_id))
            .await?;
        self.repo()?
            .hold(
                tenant,
                lead_id,
                &request.reason,
                request.hold_until,
                request.reminder_date,
                &actor.user_id,
            )
            .await
    }

    pub async fn release_hold(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: ReasonRequest,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        let allowed =
            actor.can_access_assigned("crm.leads.hold.release", lead.assigned_to.as_deref());
        self.require(
            tenant,
            actor,
            "crm.leads.hold.release",
            allowed,
            Some(lead_id),
        )
        .await?;
        self.repo()?
            .release_hold(tenant, lead_id, &request.reason, &actor.user_id)
            .await
    }

    pub async fn archive(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: ArchiveRequest,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        self.require(
            tenant,
            actor,
            "crm.leads.archive",
            actor.can_access_assigned("crm.leads.archive", lead.assigned_to.as_deref()),
            Some(lead_id),
        )
        .await?;
        if !ARCHIVE_REASONS.contains(&request.archive_reason.as_str()) {
            return Err(CrmError::Validation("invalid archive reason".into()));
        }
        self.repo()?
            .archive(
                tenant,
                lead_id,
                &request.archive_reason,
                request.notes.as_deref(),
                &actor.user_id,
                actor.primary_role(),
            )
            .await
    }

    pub async fn unarchive(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: UnarchiveRequest,
    ) -> Result<Lead, CrmError> {
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        self.require(
            tenant,
            actor,
            "crm.leads.unarchive",
            actor.can_access_assigned("crm.leads.unarchive", lead.assigned_to.as_deref()),
            Some(lead_id),
        )
        .await?;
        let stage = PrimaryStage::from_str(&request.restore_to_stage)?;
        if stage == PrimaryStage::Archived {
            return Err(CrmError::Validation(
                "restore stage cannot be Archived".into(),
            ));
        }
        let substate = request
            .restore_to_substate
            .unwrap_or_else(|| stage.default_substate().into());
        stage.validate_substate(&substate)?;
        self.repo()?
            .unarchive(
                tenant,
                lead_id,
                &stage.to_string(),
                &substate,
                &request.reason,
                &actor.user_id,
                actor.primary_role(),
            )
            .await
    }

    pub async fn board(
        &self,
        tenant: &str,
        actor: &ActorContext,
        mut filters: LeadFilters,
    ) -> Result<Value, CrmError> {
        filters.limit = Some(500);
        // Archived leads remain visible in the final pipeline column as a recoverable,
        // audited dustbin. Archiving is not the destructive-delete operation.
        filters.include_archived = Some(true);
        self.require(
            tenant,
            actor,
            "crm.leads.read",
            actor.has("crm.leads.read"),
            None,
        )
        .await?;
        // Enquiry is the shared intake queue for users whose read permission is
        // owner-scoped. Tenant-wide readers (for example Tenant Admin) must see
        // the complete tenant pipeline so their operational overview is accurate.
        let owner_scope =
            (!actor.has_all_scope("crm.leads.read")).then_some(actor.user_id.as_str());
        let leads = self
            .repo()?
            .list_leads(tenant, &filters, owner_scope, owner_scope.is_some())
            .await?;
        let stages: Vec<Value> = PrimaryStage::ALL
            .into_iter()
            .map(|stage| {
                let stage_key = stage.to_string();
                let cards: Vec<&Lead> = leads
                    .iter()
                    .filter(|lead| lead.stage_key == stage_key)
                    .collect();
                json!({
                    "key": stage_key,
                    "order": stage.order(),
                    "substates": stage.substates(),
                    "count": cards.len(),
                    "leads": cards
                })
            })
            .collect();
        Ok(json!({
            "pipeline": { "key": "pre-admission", "name": "Pre-Admission Pipeline" },
            "scope": if owner_scope.is_some() { "shared_enquiry_and_owned" } else { "tenant" },
            "stages": stages,
            "total": leads.len()
        }))
    }

    pub async fn my_board(
        &self,
        tenant: &str,
        actor: &ActorContext,
        mut filters: LeadFilters,
    ) -> Result<Value, CrmError> {
        filters.owner = Some(actor.user_id.clone());
        let mut board = self.board(tenant, actor, filters).await?;
        board["scope"] = json!("assigned");
        Ok(board)
    }

    pub async fn operations_dashboard(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Value, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.dashboard.read",
            actor.has("crm.dashboard.read"),
            None,
        )
        .await?;

        let owner_scope =
            (!actor.has_all_scope("crm.leads.read")).then_some(actor.user_id.as_str());
        let filters = LeadFilters {
            include_archived: Some(true),
            limit: Some(500),
            ..LeadFilters::default()
        };
        let leads = self
            .repo()?
            .list_leads(tenant, &filters, owner_scope, false)
            .await?;
        let configuration = self.repo()?.list_configuration(tenant).await?;
        let counselors = self.repo()?.list_counselors(tenant).await?;
        let campaigns = if actor.has("crm.campaigns.read") || actor.has("crm.reports.read") {
            self.repo()?.list_campaigns(tenant).await?
        } else {
            Vec::new()
        };
        let post_qualified_whatsapp = self
            .repo()?
            .count_post_qualified_whatsapp(tenant, owner_scope)
            .await?;

        let now = Utc::now();
        let active: Vec<&Lead> = leads
            .iter()
            .filter(|lead| {
                lead.global_status.as_deref() != Some("archive") && lead.stage_key != "archived"
            })
            .collect();
        let scheduled_follow_ups: Vec<&Lead> = active
            .iter()
            .copied()
            .filter(|lead| lead.follow_up_at.is_some())
            .collect();
        let follow_ups_due = scheduled_follow_ups
            .iter()
            .filter(|lead| lead.follow_up_at.is_some_and(|due| due <= now))
            .count();
        let counselor_sla = percentage(
            scheduled_follow_ups.len().saturating_sub(follow_ups_due),
            scheduled_follow_ups.len(),
        );
        let applications = active
            .iter()
            .filter(|lead| {
                matches!(
                    lead.stage_key.as_str(),
                    "application" | "application_status" | "offer_status"
                )
            })
            .count();
        let accepted = active
            .iter()
            .filter(|lead| {
                json_string(&lead.custom_fields, &["offerDecision", "offer_decision"]).as_deref()
                    == Some("accepted")
            })
            .count();
        let qualified_plus = active
            .iter()
            .filter(|lead| {
                matches!(
                    lead.stage_key.as_str(),
                    "qualified" | "application" | "application_status" | "offer_status"
                )
            })
            .count();

        let counselor_names: HashMap<String, String> = counselors
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("userId")?.as_str()?.to_owned(),
                    entry.get("displayName")?.as_str()?.to_owned(),
                ))
            })
            .collect();
        let mut priority_leads = active.clone();
        priority_leads.sort_by_key(|lead| {
            (
                priority_rank(&lead.priority),
                lead.follow_up_at
                    .as_ref()
                    .map_or(i64::MAX, chrono::DateTime::timestamp),
            )
        });
        let priority_queue: Vec<Value> = priority_leads
            .into_iter()
            .take(6)
            .map(|lead| json!({
                "leadId": lead.id,
                "fullName": lead.full_name,
                "course": json_string(&lead.interest, &["programName", "program_name", "programId", "program_id"]),
                "city": json_string(&lead.custom_fields, &["city"]),
                "source": lead.source,
                "assignedTo": lead.assigned_to.as_ref().and_then(|id| counselor_names.get(id)).cloned().or_else(|| lead.assigned_to.clone()),
                "priority": lead.priority,
                "followUpAt": lead.follow_up_at,
            }))
            .collect();

        let mut source_performance: HashMap<String, SourcePerformance> = HashMap::new();
        for lead in &active {
            let source = source_performance.entry(lead.source.clone()).or_default();
            source.leads += 1;
            if matches!(
                lead.stage_key.as_str(),
                "application" | "application_status" | "offer_status"
            ) {
                source.applications += 1;
            }
        }
        for campaign in &campaigns {
            let source = source_performance
                .entry(campaign.source.clone())
                .or_default();
            source.budget += campaign.budget;
            source.spent += campaign.spent;
            source.attributed_revenue += campaign.attributed_revenue;
        }
        let mut source_rows: Vec<(String, SourcePerformance)> =
            source_performance.into_iter().collect();
        source_rows.sort_by(|(left_name, left), (right_name, right)| {
            right
                .leads
                .cmp(&left.leads)
                .then_with(|| left_name.cmp(right_name))
        });
        let source_roi: Vec<Value> = source_rows
            .into_iter()
            .map(|(source, metrics)| json!({
                "source": source,
                "leads": metrics.leads,
                "applications": metrics.applications,
                "budget": metrics.budget,
                "spent": metrics.spent,
                "attributedRevenue": metrics.attributed_revenue,
                "costPerLead": (metrics.leads > 0 && metrics.spent > 0.0).then(|| round_one(metrics.spent / metrics.leads as f64)),
                "roi": (metrics.spent > 0.0).then(|| round_one(metrics.attributed_revenue / metrics.spent)),
            }))
            .collect();

        let total_budget: f64 = campaigns.iter().map(|campaign| campaign.budget).sum();
        let total_spent: f64 = campaigns.iter().map(|campaign| campaign.spent).sum();
        let total_revenue: f64 = campaigns
            .iter()
            .map(|campaign| campaign.attributed_revenue)
            .sum();
        let campaign_roi = if total_spent > 0.0 {
            round_one(total_revenue / total_spent)
        } else {
            0.0
        };
        let budget_used = if total_budget > 0.0 {
            (total_spent / total_budget * 100.0).round() as i64
        } else {
            0
        };
        let landing_pages: i32 = campaigns
            .iter()
            .map(|campaign| campaign.landing_pages)
            .sum();
        let active_utm = campaigns
            .iter()
            .filter(|campaign| {
                campaign.status == "active"
                    && campaign
                        .utm_code
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
            })
            .count();

        let complete_records = active
            .iter()
            .filter(|lead| {
                lead.assigned_to.is_some() && lead.follow_up_at.is_some() && has_source(lead)
            })
            .count();
        let unique_records = active
            .iter()
            .filter(|lead| lead.duplicate_of.is_none())
            .count();
        let attributed_records = active.iter().filter(|lead| has_source(lead)).count();

        let automations: Vec<Value> = configuration
            .get("automationToggles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|automation| json!({
                "id": automation.get("id"),
                "label": automation.get("conditions").and_then(|value| value.get("label")).and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| humanize_key(automation.get("action").and_then(Value::as_str).unwrap_or("automation"))),
                "stage": automation.get("stage"),
                "triggerName": automation.get("triggerName"),
                "action": automation.get("action"),
                "templateKey": automation.get("templateKey"),
                "enabled": automation.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            }))
            .collect();

        let mut case_counts = HashMap::from([
            ("prospect", 0usize),
            ("deferred", 0usize),
            ("on_hold", 0usize),
            ("archive", 0usize),
        ]);
        let mut case_leads: Vec<&Lead> = leads
            .iter()
            .filter(|lead| lead.global_status.is_some())
            .collect();
        for lead in &case_leads {
            if let Some(status) = lead.global_status.as_deref() {
                *case_counts.entry(status).or_default() += 1;
            }
        }
        case_leads.sort_by_key(|lead| std::cmp::Reverse(lead.updated_at));
        let case_items: Vec<Value> = case_leads
            .into_iter()
            .take(3)
            .map(|lead| json!({
                "leadId": lead.id,
                "fullName": lead.full_name,
                "status": lead.global_status.as_deref().map(humanize_key),
                "reason": json_string(&lead.global_status_data, &["reason"]),
                "due": json_string(&lead.global_status_data, &["reminderDate", "holdUntil", "intakeMonth", "intakeYear"]),
            }))
            .collect();
        let open_cases: usize = case_counts.values().sum();

        Ok(json!({
            "scope": actor.scope_for("crm.leads.read"),
            "headline": {
                "leadIntake": active.len(),
                "followUpsDue": follow_ups_due,
                "campaignRoi": campaign_roi,
                "counselorSla": counselor_sla,
            },
            "operations": {
                "newLeads": active.iter().filter(|lead| lead.stage_key == "enquiry" && lead.substate_key == "new").count(),
                "contactDue": follow_ups_due,
                "qualified": qualified_plus,
                "applications": applications,
                "accepted": accepted,
                "priorityQueue": priority_queue,
            },
            "automations": automations,
            "sourceRoi": source_roi,
            "campaignSummary": {
                "budgetUsedPercent": budget_used,
                "landingPages": landing_pages,
                "activeUtm": active_utm,
            },
            "health": {
                "score": percentage(complete_records, active.len()),
                "duplicateDetection": percentage(unique_records, active.len()),
                "sourceAttribution": percentage(attributed_records, active.len()),
                "postQualifiedWhatsapp": percentage(post_qualified_whatsapp as usize, qualified_plus),
            },
            "cases": {
                "open": open_cases,
                "counts": case_counts,
                "items": case_items,
            }
        }))
    }

    pub fn stage_catalog(&self) -> Value {
        json!(
            PrimaryStage::ALL
                .into_iter()
                .map(|stage| json!({
                    "key": stage.to_string(), "order": stage.order(),
                    "defaultSubstate": stage.default_substate(), "substates": stage.substates()
                }))
                .collect::<Vec<_>>()
        )
    }

    pub async fn realtime_events(
        &self,
        tenant: &str,
        actor: &ActorContext,
        cursor: i64,
    ) -> Result<Vec<Value>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.dashboard.read",
            actor.has("crm.dashboard.read"),
            None,
        )
        .await?;
        self.repo()?.events_after(tenant, cursor.max(0)).await
    }
    pub(crate) async fn realtime_events_raw(
        &self,
        tenant: &str,
        cursor: i64,
    ) -> Result<Vec<Value>, CrmError> {
        self.repo()?.events_after(tenant, cursor.max(0)).await
    }
    pub async fn recent_activity(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<Value>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.dashboard.read",
            actor.has("crm.dashboard.read"),
            None,
        )
        .await?;
        let owner_scope =
            (!actor.has_all_scope("crm.leads.read")).then_some(actor.user_id.as_str());
        self.repo()?.recent_activity(tenant, owner_scope, 25).await
    }
    pub async fn timeline(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
    ) -> Result<Value, CrmError> {
        self.get_lead(tenant, actor, lead_id).await?;
        self.repo()?.timeline(tenant, lead_id).await
    }

    pub async fn add_lead_note(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: CreateLeadNoteRequest,
    ) -> Result<crate::domain::Communication, CrmError> {
        let content = request.content.trim();
        if content.is_empty() {
            return Err(CrmError::Validation("note content is required".into()));
        }
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        // Notes are safe collaboration metadata. Let an authorised user add the
        // first context to a legacy/unclaimed card without weakening ownership
        // checks for cards that already have an owner.
        let allowed = (lead.assigned_to.is_none() && actor.has("crm.leads.update"))
            || actor.can_access_assigned("crm.leads.update", lead.assigned_to.as_deref());
        self.require(tenant, actor, "crm.leads.update", allowed, Some(lead_id))
            .await?;
        self.repo()?
            .send_communication(
                tenant,
                lead_id,
                "note",
                None,
                Some("Lead note"),
                json!({ "text": content }),
                None,
                &actor.user_id,
            )
            .await
    }

    pub async fn add_lead_task(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
        request: CreateLeadTaskRequest,
    ) -> Result<Value, CrmError> {
        let title = request.title.trim();
        if title.is_empty() {
            return Err(CrmError::Validation("task title is required".into()));
        }
        let priority = request.priority.as_deref().unwrap_or("medium");
        if !matches!(priority, "low" | "medium" | "high" | "urgent") {
            return Err(CrmError::Validation("invalid task priority".into()));
        }
        let lead = self.repo()?.find_lead(tenant, lead_id).await?;
        // Follow-up tasks follow the same unclaimed-card rule as notes. Once a
        // card has an owner, assigned-scope users still need to be that owner.
        let allowed = (lead.assigned_to.is_none() && actor.has("crm.leads.update"))
            || actor.can_access_assigned("crm.leads.update", lead.assigned_to.as_deref());
        self.require(tenant, actor, "crm.leads.update", allowed, Some(lead_id))
            .await?;
        self.repo()?
            .create_lead_task(
                tenant,
                lead_id,
                &actor.user_id,
                title,
                request.due_at,
                priority,
            )
            .await
    }
    pub async fn application_link(
        &self,
        tenant: &str,
        actor: &ActorContext,
        lead_id: Uuid,
    ) -> Result<Value, CrmError> {
        self.get_lead(tenant, actor, lead_id).await?;
        Ok(self
            .repo()?
            .application_link(tenant, lead_id)
            .await?
            .unwrap_or(Value::Null))
    }

    pub async fn create_form(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: CreateFormRequest,
    ) -> Result<FormDefinition, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.forms.create",
            actor.has("crm.forms.create"),
            None,
        )
        .await?;
        if request.name.trim().is_empty() || request.form_type.trim().is_empty() {
            return Err(CrmError::Validation(
                "form name and type are required".into(),
            ));
        }
        self.repo()?
            .create_form(
                tenant,
                &request.name,
                &request.form_type,
                request.program_id.as_deref(),
                request.intake_year,
                request.schema,
                &actor.user_id,
            )
            .await
    }

    pub async fn list_forms(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<FormDefinition>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.forms.read",
            actor.has("crm.forms.read"),
            None,
        )
        .await?;
        self.repo()?.list_forms(tenant).await
    }

    pub async fn published_lead_capture_form(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<FormDefinition, CrmError> {
        let allowed = actor.has_any(&["crm.forms.read", "crm.leads.create"]);
        self.require(tenant, actor, "crm.forms.read", allowed, None)
            .await?;
        self.repo()?.find_published_lead_capture_form(tenant).await
    }

    /// Returns the published form of a given type, whatever the administrator named it.
    pub async fn published_form_by_type(
        &self,
        tenant: &str,
        actor: &ActorContext,
        form_type: &str,
    ) -> Result<FormDefinition, CrmError> {
        let allowed = actor.has_any(&["crm.forms.read", "crm.leads.create", "crm.forms.submit"]);
        self.require(tenant, actor, "crm.forms.read", allowed, None)
            .await?;
        self.repo()?
            .find_published_form_by_type(tenant, form_type)
            .await
    }

    /// Lists every published form so the workspace can render whatever exists.
    pub async fn published_forms(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<FormDefinition>, CrmError> {
        let allowed = actor.has_any(&["crm.forms.read", "crm.leads.create", "crm.forms.submit"]);
        self.require(tenant, actor, "crm.forms.read", allowed, None)
            .await?;
        self.repo()?.list_published_forms(tenant).await
    }

    pub async fn get_form(
        &self,
        tenant: &str,
        actor: &ActorContext,
        form_id: Uuid,
    ) -> Result<FormDefinition, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.forms.read",
            actor.has("crm.forms.read"),
            None,
        )
        .await?;
        self.repo()?.find_form(tenant, form_id).await
    }

    pub async fn update_form(
        &self,
        tenant: &str,
        actor: &ActorContext,
        form_id: Uuid,
        request: UpdateFormRequest,
    ) -> Result<FormDefinition, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.forms.update",
            actor.has("crm.forms.update"),
            None,
        )
        .await?;
        if request
            .form_type
            .as_deref()
            .is_some_and(|form_type| form_type.trim().is_empty())
        {
            return Err(CrmError::Validation("form type cannot be empty".into()));
        }
        let repository = self.repo()?;
        let existing = repository.find_form(tenant, form_id).await?;
        if request.form_type.as_deref().is_some_and(|requested| {
            requested.trim().to_ascii_lowercase().replace('-', "_")
                != existing
                    .form_type
                    .trim()
                    .to_ascii_lowercase()
                    .replace('-', "_")
        }) {
            return Err(CrmError::Validation(
                "form purpose cannot be changed after creation; create a separate form instead"
                    .into(),
            ));
        }
        repository
            .update_form(
                tenant,
                form_id,
                request.name.as_deref(),
                request.form_type.as_deref(),
                request.schema,
                &actor.user_id,
            )
            .await
    }

    pub async fn set_form_status(
        &self,
        tenant: &str,
        actor: &ActorContext,
        form_id: Uuid,
        status: &str,
    ) -> Result<FormDefinition, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.forms.publish",
            actor.has("crm.forms.publish"),
            None,
        )
        .await?;
        let repository = self.repo()?;
        if status == "published" {
            let form = repository.find_form(tenant, form_id).await?;
            validate_form_for_publish(&form.form_type, &form.schema)?;
        }
        repository
            .set_form_status(tenant, form_id, status, &actor.user_id)
            .await
    }

    pub async fn delete_form(
        &self,
        tenant: &str,
        actor: &ActorContext,
        form_id: Uuid,
    ) -> Result<(), CrmError> {
        self.require(
            tenant,
            actor,
            "crm.forms.delete",
            actor.has("crm.forms.delete"),
            None,
        )
        .await?;
        self.repo()?.delete_form(tenant, form_id).await
    }

    pub async fn submit_form(
        &self,
        tenant: &str,
        actor: &ActorContext,
        form_id: Uuid,
        request: SubmitFormRequest,
    ) -> Result<Value, CrmError> {
        let repository = self.repo()?;
        let form = repository.find_form(tenant, form_id).await?;
        if form.status != "published" && !actor.has("crm.forms.update") {
            return Err(CrmError::Forbidden(
                "only published forms accept submissions".into(),
            ));
        }
        validate_form_submission(tenant, &form.schema, &request.data)?;
        let normalized_form_type = form.form_type.to_ascii_lowercase().replace('-', "_");
        let creates_lead =
            normalized_form_type.contains("enquiry") || normalized_form_type == "lead_capture";
        if !creates_lead && !actor.has_any(&["crm.forms.submit", "crm.forms.update"]) {
            return Err(CrmError::Forbidden(
                "this internal form can only be submitted by admission staff".into(),
            ));
        }

        let campaign_id = request.campaign_id;
        let idempotency_key = request
            .idempotency_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if idempotency_key
            .as_deref()
            .is_some_and(|value| value.len() > 200)
        {
            return Err(CrmError::Validation(
                "idempotencyKey must not exceed 200 characters".into(),
            ));
        }
        let mut lead_id = request.lead_id;
        let mut created_lead_id = None;
        if creates_lead && lead_id.is_none() {
            let student = request.data.get("student").unwrap_or(&request.data);
            let full_name = json_text(student, "name")
                .ok_or_else(|| CrmError::Validation("enquiry form requires name".into()))?;
            let email = json_text(student, "email");
            let whatsapp = json_text(student, "whatsapp");
            let phone = json_text(student, "phone").or_else(|| whatsapp.clone());
            if email.is_none() && phone.is_none() {
                return Err(CrmError::Validation(
                    "enquiry form requires student phone or email".into(),
                ));
            }
            let source = canonical_lead_source(
                request
                    .data
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("Institution Website"),
            )?;
            let lead = repository
                .create_lead(
                    tenant,
                    &actor.user_id,
                    actor.primary_role(),
                    NewLead {
                        source,
                        source_detail: json!({
                            "formId": form_id,
                            "formVersion": form.version,
                            "campaignId": campaign_id,
                            "idempotencyKey": idempotency_key
                        }),
                        full_name,
                        email,
                        phone,
                        whatsapp,
                        parent_name: json_text(student, "parentName"),
                        parent_phone: json_text(student, "parentPhone"),
                        academic: request
                            .data
                            .get("academic")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                        interest: request
                            .data
                            .get("interest")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                        priority: request
                            .data
                            .get("priority")
                            .and_then(Value::as_str)
                            .filter(|priority| {
                                matches!(*priority, "low" | "medium" | "high" | "urgent")
                            })
                            .unwrap_or("medium")
                            .to_owned(),
                        follow_up_at: None,
                        preferred_channel: request
                            .data
                            .get("preferredChannel")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        consent_given: request
                            .data
                            .get("consentGiven")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        custom_fields: request.data.clone(),
                    },
                )
                .await?;
            lead_id = Some(lead.id);
            created_lead_id = Some(lead.id);
        }

        match repository
            .submit_form(
                tenant,
                form_id,
                lead_id,
                campaign_id,
                idempotency_key.as_deref(),
                request.data,
                &actor.user_id,
            )
            .await
        {
            Ok(mut submission) => {
                let replayed = submission
                    .get("replayed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if replayed {
                    if let Some(id) = created_lead_id {
                        let _ = repository.soft_delete(tenant, id).await;
                    }
                } else if let (Some(object), Some(id)) =
                    (submission.as_object_mut(), created_lead_id)
                {
                    object.insert("createdLeadId".into(), json!(id));
                }
                Ok(submission)
            }
            Err(error) => {
                if let Some(id) = created_lead_id {
                    let _ = repository.soft_delete(tenant, id).await;
                }
                Err(error)
            }
        }
    }

    pub async fn list_submissions(
        &self,
        tenant: &str,
        actor: &ActorContext,
        form_id: Uuid,
    ) -> Result<Vec<Value>, CrmError> {
        let allowed = actor.has("crm.forms.submissions.read");
        self.require(tenant, actor, "crm.forms.submissions.read", allowed, None)
            .await?;
        self.repo()?.list_submissions(tenant, form_id).await
    }

    /// Returns the newest application snapshot for the Admission Desk handoff
    /// that runs after an offer is accepted.
    pub async fn latest_application_submission(
        &self,
        tenant: &str,
        lead_id: Uuid,
    ) -> Result<Option<Value>, CrmError> {
        self.repo()?
            .latest_application_submission(tenant, lead_id)
            .await
    }

    pub async fn send_communication(
        &self,
        tenant: &str,
        actor: &ActorContext,
        channel: &str,
        request: SendCommunicationRequest,
    ) -> Result<crate::domain::Communication, CrmError> {
        let channel = channel.to_ascii_lowercase();
        if !matches!(channel.as_str(), "whatsapp" | "email" | "call") {
            return Err(CrmError::Validation(
                "channel must be whatsapp, email, or call".into(),
            ));
        }
        let lead = self.get_lead(tenant, actor, request.lead_id).await?;
        let allowed =
            actor.can_access_assigned("crm.communications.send", lead.assigned_to.as_deref());
        self.require(
            tenant,
            actor,
            "crm.communications.send",
            allowed,
            Some(request.lead_id),
        )
        .await?;
        if channel == "whatsapp"
            && !self
                .repo()?
                .has_reached_qualified(tenant, request.lead_id)
                .await?
        {
            return Err(CrmError::Validation(
                "WhatsApp communication is available after the lead reaches Qualified".into(),
            ));
        }
        if channel == "call" && request.outcome.as_deref().unwrap_or("").trim().is_empty() {
            return Err(CrmError::Validation("call outcome is required".into()));
        }
        self.repo()?
            .send_communication(
                tenant,
                request.lead_id,
                &channel,
                request.template_key.as_deref(),
                request.subject.as_deref(),
                request.content,
                request.outcome.as_deref(),
                &actor.user_id,
            )
            .await
    }

    pub async fn list_templates(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<Value>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.templates.read",
            actor.has("crm.templates.read"),
            None,
        )
        .await?;
        self.repo()?.list_templates(tenant).await
    }

    pub async fn create_template(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: CreateTemplateRequest,
    ) -> Result<Value, CrmError> {
        let repository = self.repo()?;
        let language = request.language.as_deref().unwrap_or("en");
        let exists = repository
            .list_templates(tenant)
            .await?
            .iter()
            .any(|template| {
                template.get("templateKey").and_then(Value::as_str)
                    == Some(request.template_key.as_str())
                    && template.get("language").and_then(Value::as_str) == Some(language)
            });
        let permission = if exists {
            "crm.templates.update"
        } else {
            "crm.templates.create"
        };
        self.require(tenant, actor, permission, actor.has(permission), None)
            .await?;
        repository
            .create_template(
                tenant,
                &request.template_key,
                &request.channel,
                &request.name,
                &request.content,
                language,
                &actor.user_id,
            )
            .await
    }

    pub async fn counselors(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<Value>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.assignment.read",
            actor.has("crm.assignment.read"),
            None,
        )
        .await?;
        self.repo()?.list_counselors(tenant).await
    }

    pub async fn upsert_counselor(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: CounselorCapacityRequest,
    ) -> Result<Value, CrmError> {
        let repository = self.repo()?;
        let exists = repository
            .list_counselors(tenant)
            .await?
            .iter()
            .any(|counselor| {
                counselor.get("userId").and_then(Value::as_str) == Some(request.user_id.as_str())
            });
        let permission = if exists {
            "crm.assignment.update"
        } else {
            "crm.assignment.create"
        };

        self.require(tenant, actor, permission, actor.has(permission), None)
            .await?;
        repository.upsert_counselor(tenant, &request).await
    }

    pub async fn campaigns(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Vec<Campaign>, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.campaigns.read",
            actor.has_any(&["crm.campaigns.read", "crm.reports.read"]),
            None,
        )
        .await?;
        self.repo()?.list_campaigns(tenant).await
    }

    pub async fn upsert_campaign(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: CreateCampaignRequest,
    ) -> Result<Campaign, CrmError> {
        let repository = self.repo()?;
        let exists = repository
            .list_campaigns(tenant)
            .await?
            .iter()
            .any(|campaign| campaign.name == request.name.trim());
        let permission = if exists {
            "crm.campaigns.update"
        } else {
            "crm.campaigns.create"
        };

        self.require(tenant, actor, permission, actor.has(permission), None)
            .await?;
        if request.name.trim().is_empty() || request.source.trim().is_empty() {
            return Err(CrmError::Validation(
                "campaign name and source are required".into(),
            ));
        }
        if [request.budget, request.spent, request.attributed_revenue]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite() || value < 0.0)
            || request.landing_pages.is_some_and(|value| value < 0)
        {
            return Err(CrmError::Validation(
                "campaign amounts and landing page count must be non-negative".into(),
            ));
        }
        if !matches!(
            request.status.as_deref().unwrap_or("draft"),
            "draft" | "active" | "paused" | "completed"
        ) {
            return Err(CrmError::Validation("invalid campaign status".into()));
        }
        if request
            .starts_on
            .zip(request.ends_on)
            .is_some_and(|(start, end)| end < start)
        {
            return Err(CrmError::Validation(
                "campaign end date cannot be before start date".into(),
            ));
        }
        repository
            .upsert_campaign(tenant, &actor.user_id, &request)
            .await
    }

    pub async fn configuration(
        &self,
        tenant: &str,
        actor: &ActorContext,
    ) -> Result<Value, CrmError> {
        self.require(
            tenant,
            actor,
            "crm.configuration.read",
            actor.has("crm.configuration.read"),
            None,
        )
        .await?;
        self.repo()?.list_configuration(tenant).await
    }

    pub async fn upsert_workflow_toggle(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: WorkflowToggleRequest,
    ) -> Result<Value, CrmError> {
        let from = PrimaryStage::from_str(&request.from_stage)?;
        let to = PrimaryStage::from_str(&request.to_stage)?;
        let from_stage = from.to_string();
        let to_stage = to.to_string();
        let repository = self.repo()?;
        let configuration = repository.list_configuration(tenant).await?;
        let exists = configuration["workflowToggles"]
            .as_array()
            .is_some_and(|toggles| {
                toggles.iter().any(|toggle| {
                    toggle.get("fromStage").and_then(Value::as_str) == Some(from_stage.as_str())
                        && toggle.get("toStage").and_then(Value::as_str) == Some(to_stage.as_str())
                })
            });
        let permission = if exists {
            "crm.configuration.update"
        } else {
            "crm.configuration.create"
        };

        self.require(tenant, actor, permission, actor.has(permission), None)
            .await?;
        repository
            .upsert_workflow_toggle(
                tenant,
                &from_stage,
                &to_stage,
                request.allowed_roles,
                request.requires_approval.unwrap_or(false),
                request.approval_role.as_deref(),
                request.enabled.unwrap_or(true),
                &actor.user_id,
            )
            .await
    }

    pub async fn upsert_automation_toggle(
        &self,
        tenant: &str,
        actor: &ActorContext,
        request: AutomationToggleRequest,
    ) -> Result<Value, CrmError> {
        let stage = PrimaryStage::from_str(&request.stage)?;
        let stage_key = stage.to_string();
        let repository = self.repo()?;
        let configuration = repository.list_configuration(tenant).await?;
        let exists = configuration["automationToggles"]
            .as_array()
            .is_some_and(|toggles| {
                toggles.iter().any(|toggle| {
                    toggle.get("stage").and_then(Value::as_str) == Some(stage_key.as_str())
                        && toggle.get("triggerName").and_then(Value::as_str)
                            == Some(request.trigger_name.as_str())
                        && toggle.get("action").and_then(Value::as_str)
                            == Some(request.action.as_str())
                })
            });
        let permission = if exists {
            "crm.configuration.update"
        } else {
            "crm.configuration.create"
        };

        self.require(tenant, actor, permission, actor.has(permission), None)
            .await?;
        repository
            .upsert_automation_toggle(
                tenant,
                &stage_key,
                &request.trigger_name,
                &request.action,
                request.template_key.as_deref(),
                request.conditions,
                request.enabled.unwrap_or(true),
                &actor.user_id,
            )
            .await
    }

    pub async fn roles(&self, tenant: &str, actor: &ActorContext) -> Result<Value, CrmError> {
        self.require(
            tenant,
            actor,
            "authorization.roles.read",
            actor.has("authorization.roles.read"),
            None,
        )
        .await?;
        self.repo()?.list_authorization_roles(tenant).await
    }

    pub fn effective_permissions(&self, actor: &ActorContext) -> Value {
        let mut permissions = actor.permissions.iter().cloned().collect::<Vec<_>>();
        permissions.sort();
        json!({
            "userId": actor.user_id,
            "roles": actor.roles,
            "primaryRole": actor.primary_role(),
            "permissions": permissions,
            "scopes": actor.permission_scopes,
        })
    }

    async fn ensure_toggle_allows(
        &self,
        tenant: &str,
        actor: &ActorContext,
        from: PrimaryStage,
        to: PrimaryStage,
    ) -> Result<(), CrmError> {
        let configuration = self.repo()?.list_configuration(tenant).await?;
        let Some(toggles) = configuration
            .get("workflowToggles")
            .and_then(Value::as_array)
        else {
            return Ok(());
        };
        let matching = toggles.iter().find(|toggle| {
            toggle.get("fromStage").and_then(Value::as_str) == Some(from.to_string().as_str())
                && toggle.get("toStage").and_then(Value::as_str) == Some(to.to_string().as_str())
        });
        let Some(toggle) = matching else {
            return Ok(());
        };
        if toggle.get("enabled").and_then(Value::as_bool) == Some(false) {
            return Err(CrmError::Forbidden(
                "this transition is disabled by tenant configuration".into(),
            ));
        }
        if let Some(roles) = toggle.get("allowedRoles").and_then(Value::as_array)
            && !roles.is_empty()
            && !roles
                .iter()
                .filter_map(Value::as_str)
                .any(|role| actor.roles.iter().any(|actor_role| actor_role == role))
        {
            return Err(CrmError::Forbidden(
                "role is not enabled for this configured transition".into(),
            ));
        }
        if toggle.get("requiresApproval").and_then(Value::as_bool) == Some(true) {
            let approval_role = toggle.get("approvalRole").and_then(Value::as_str);
            if approval_role.is_none_or(|role| !actor.roles.iter().any(|item| item == role)) {
                return Err(CrmError::Forbidden(format!(
                    "transition requires approval by {}",
                    approval_role.unwrap_or("an authorized approver")
                )));
            }
        }
        Ok(())
    }

    async fn require(
        &self,
        tenant: &str,
        actor: &ActorContext,
        action: &str,
        allowed: bool,
        entity_id: Option<Uuid>,
    ) -> Result<(), CrmError> {
        if actor.user_id.trim().is_empty() {
            return Err(CrmError::Unauthorized);
        }
        let repository = self.repo()?;
        repository
            .audit_permission(
                tenant,
                &actor.user_id,
                actor.primary_role(),
                action,
                "crm_lead",
                entity_id.map(|id| id.to_string()),
                allowed,
                (!allowed).then_some("role or ownership policy denied the action"),
            )
            .await?;
        if !allowed {
            return Err(CrmError::Forbidden(action.into()));
        }
        Ok(())
    }

    fn repo(&self) -> Result<&PostgresCrmRepository, CrmError> {
        self.repository.as_ref().ok_or(CrmError::Unavailable)
    }
}

fn percentage(numerator: usize, denominator: usize) -> i64 {
    if denominator == 0 {
        0
    } else {
        ((numerator as f64 / denominator as f64) * 100.0).round() as i64
    }
}

fn round_one(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "urgent" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

fn has_source(lead: &Lead) -> bool {
    !lead.source.trim().is_empty() && !lead.source.eq_ignore_ascii_case("unknown")
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let candidate = value.get(*key)?;
        candidate
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .or_else(|| candidate.as_i64().map(|number| number.to_string()))
    })
}

fn humanize_key(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalized_optional(value: Option<String>, lowercase: bool) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        if value.is_empty() {
            None
        } else if lowercase {
            Some(value.to_ascii_lowercase())
        } else {
            Some(value.to_owned())
        }
    })
}

fn import_contact_keys(phone: Option<&str>, email: Option<&str>) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    if let Some(phone) = phone {
        let digits = phone
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>();
        if !digits.is_empty() {
            keys.push(format!("phone:{digits}"));
        }
    }
    if let Some(email) = email {
        keys.push(format!("email:{}", email.trim().to_ascii_lowercase()));
    }
    keys
}

fn validate_priority(priority: &str) -> Result<(), CrmError> {
    if matches!(priority, "low" | "medium" | "high" | "urgent") {
        Ok(())
    } else {
        Err(CrmError::Validation(
            "priority must be low, medium, high, or urgent".into(),
        ))
    }
}

pub const ARCHIVE_REASONS: &[&str] = &[
    "Academic Ineligibility",
    "Age Criteria Not Met",
    "Calls Not Answered",
    "Duplicate Lead",
    "Education Gap",
    "Education Loan Rejected",
    "Fake Documents",
    "Financial Ineligibility",
    "Full Scholarship Required",
    "Health Issues",
    "Insufficient Documents",
    "Intake Deadline Passed",
    "Interview No Show",
    "Invalid Number",
    "Lost to Competitor",
    "Low Score",
    "No Offer",
    "No Offer from Preferred Choice",
    "No Revenue Potential",
    "Not Happy with Service",
    "Not Interested in Engineering",
    "Not Reachable",
    "Not Satisfied with Offering",
    "Offer Expired",
    "Others",
    "Program Full/Closed",
    "Program Not Available",
    "Program Not Offered",
    "Refund Initiated",
    "Spam",
    "Student Opted Out",
];

fn json_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn validate_form_submission(tenant: &str, schema: &Value, data: &Value) -> Result<(), CrmError> {
    let sections = schema
        .get("sections")
        .and_then(Value::as_array)
        .or_else(|| schema.as_array());
    let Some(sections) = sections else {
        return Ok(());
    };
    let values = data.get("values").unwrap_or(data);
    for field in sections
        .iter()
        .filter_map(|section| section.get("fields").and_then(Value::as_array))
        .flatten()
    {
        let key = field
            .get("key")
            .and_then(Value::as_str)
            .or_else(|| field.get("label").and_then(Value::as_str))
            .unwrap_or_default();
        let label = field.get("label").and_then(Value::as_str).unwrap_or(key);
        let candidate = values.get(key);
        let present = candidate.is_some_and(|value| match value {
            Value::Null => false,
            Value::String(value) => !value.trim().is_empty(),
            Value::Array(value) => !value.is_empty(),
            _ => true,
        });
        if field.get("required").and_then(Value::as_bool) == Some(true) && !present {
            return Err(CrmError::Validation(format!(
                "required form field is missing: {label}"
            )));
        }
        if !present {
            continue;
        }

        let field_type = field
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if field_type.eq_ignore_ascii_case("upload")
            || field_type.eq_ignore_ascii_case("image upload")
        {
            validate_uploaded_media(tenant, candidate.expect("present value"), label)?;
            continue;
        }

        let allowed = field
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        option.as_str().or_else(|| {
                            option
                                .get("value")
                                .and_then(Value::as_str)
                                .or_else(|| option.get("label").and_then(Value::as_str))
                        })
                    })
                    .map(str::trim)
                    .filter(|option| !option.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if allowed.is_empty() {
            continue;
        }

        let selected = match candidate {
            Some(Value::Array(values)) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>(),
            Some(Value::String(value)) if field_type.eq_ignore_ascii_case("multi select") => value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>(),
            Some(Value::String(value)) => vec![value.trim()],
            _ => Vec::new(),
        };
        if selected.is_empty() || selected.iter().any(|value| !allowed.contains(value)) {
            return Err(CrmError::Validation(format!(
                "invalid option selected for {label}"
            )));
        }
    }
    Ok(())
}

fn validate_uploaded_media(tenant: &str, value: &Value, label: &str) -> Result<(), CrmError> {
    let object = value.as_object().ok_or_else(|| {
        CrmError::Validation(format!("{label} must be uploaded before submission"))
    })?;
    let storage = object.get("storage").and_then(Value::as_str);
    let secure_url = object.get("secureUrl").and_then(Value::as_str);
    let public_id = object.get("publicId").and_then(Value::as_str);
    let file_name = object.get("fileName").and_then(Value::as_str);
    let expected_prefix = format!("supercampus/{tenant}/media/");
    if storage != Some("cloudinary")
        || !secure_url.is_some_and(|url| url.starts_with("https://"))
        || !public_id.is_some_and(|id| id.starts_with(&expected_prefix))
        || !file_name.is_some_and(|name| !name.trim().is_empty())
    {
        return Err(CrmError::Validation(format!(
            "{label} does not contain a valid tenant Cloudinary upload"
        )));
    }
    Ok(())
}

fn validate_form_for_publish(form_type: &str, schema: &Value) -> Result<(), CrmError> {
    let Some(sections) = schema
        .get("sections")
        .and_then(Value::as_array)
        .or_else(|| schema.as_array())
    else {
        return Ok(());
    };
    let fields = sections
        .iter()
        .filter_map(|section| section.get("fields").and_then(Value::as_array))
        .flatten()
        .collect::<Vec<_>>();
    for field in &fields {
        let field_type = field
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(field_type, "Dropdown" | "Multi select" | "Radio group") {
            continue;
        }
        let has_option = field
            .get("options")
            .and_then(Value::as_array)
            .is_some_and(|options| {
                options.iter().any(|option| {
                    option
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty())
                        || option
                            .get("value")
                            .and_then(Value::as_str)
                            .is_some_and(|value| !value.trim().is_empty())
                })
            });
        if !has_option {
            let label = field
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("choice field");
            return Err(CrmError::Validation(format!(
                "at least one option is required before publishing: {label}"
            )));
        }
    }
    if form_type.to_ascii_lowercase().replace('-', "_") == "application" {
        let mut document_types = std::collections::HashSet::new();
        for field in fields {
            let field_type = field
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !field_type.eq_ignore_ascii_case("upload")
                && !field_type.eq_ignore_ascii_case("image upload")
            {
                continue;
            }
            let Some(config) = field.get("documentConfig").and_then(Value::as_object) else {
                continue;
            };
            if config.get("enabled").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let label = field
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("Upload");
            let document_type = config
                .get("documentType")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    CrmError::Validation(format!(
                        "Admission Desk document type is required for {label}"
                    ))
                })?;
            if !document_types.insert(document_type.to_owned()) {
                return Err(CrmError::Validation(format!(
                    "Admission Desk document type is mapped more than once: {document_type}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod form_submission_tests {
    use super::*;

    fn actor(role: &str, permissions: &[&str]) -> ActorContext {
        ActorContext {
            user_id: "user-1".into(),
            roles: vec![role.into()],
            permissions: permissions.iter().map(|value| (*value).into()).collect(),
            permission_scopes: HashMap::new(),
            public: false,
            ip_address: None,
        }
    }

    fn choice_schema() -> Value {
        json!({
            "sections": [{
                "section": "Primary details",
                "fields": [
                    { "key": "course", "label": "Course", "type": "Dropdown", "required": true, "options": ["B.Tech CSE", "MBA"] },
                    { "key": "campuses", "label": "Campuses", "type": "Multi select", "options": ["Main", "City"] }
                ]
            }]
        })
    }

    #[test]
    fn accepts_configured_single_and_multi_select_options() {
        let data = json!({ "values": { "course": "MBA", "campuses": "Main, City" } });
        assert!(validate_form_submission("tenant-local", &choice_schema(), &data).is_ok());
    }

    #[test]
    fn rejects_values_outside_the_published_choice_catalog() {
        let data = json!({ "values": { "course": "Unpublished course" } });
        let error = validate_form_submission("tenant-local", &choice_schema(), &data).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid option selected for Course")
        );
    }

    #[test]
    fn upload_fields_require_a_tenant_scoped_cloudinary_reference() {
        let schema = json!({
            "sections": [{
                "fields": [{ "key": "marksheet", "label": "Marksheet", "type": "Upload", "required": true }]
            }]
        });
        let valid = json!({ "values": { "marksheet": {
            "storage": "cloudinary",
            "fileName": "marksheet.pdf",
            "secureUrl": "https://res.cloudinary.com/example/raw/upload/marksheet.pdf",
            "publicId": "supercampus/tenant-local/media/marksheet"
        } } });
        assert!(validate_form_submission("tenant-local", &schema, &valid).is_ok());

        let wrong_tenant = json!({ "values": { "marksheet": {
            "storage": "cloudinary",
            "fileName": "marksheet.pdf",
            "secureUrl": "https://res.cloudinary.com/example/raw/upload/marksheet.pdf",
            "publicId": "supercampus/another-tenant/media/marksheet"
        } } });
        assert!(validate_form_submission("tenant-local", &schema, &wrong_tenant).is_err());
        assert!(
            validate_form_submission(
                "tenant-local",
                &schema,
                &json!({ "values": { "marksheet": "marksheet.pdf" } })
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_to_publish_choice_fields_without_options() {
        let schema = json!({
            "sections": [{
                "fields": [{ "key": "course", "label": "Course", "type": "Dropdown" }]
            }]
        });
        let error = validate_form_for_publish("lead_capture", &schema).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("at least one option is required")
        );
    }

    #[test]
    fn application_document_mappings_must_be_unique() {
        let schema = json!({
            "sections": [{
                "fields": [
                    { "key": "certificate_10", "label": "10th Certificate", "type": "Upload", "documentConfig": { "enabled": true, "documentType": "certificate-10" } },
                    { "key": "duplicate", "label": "Another Certificate", "type": "Upload", "documentConfig": { "enabled": true, "documentType": "certificate-10" } }
                ]
            }]
        });
        let error = validate_form_for_publish("application", &schema).unwrap_err();
        assert!(error.to_string().contains("mapped more than once"));
        assert!(validate_form_for_publish("enquiry", &schema).is_ok());
    }

    #[test]
    fn administrator_detection_does_not_treat_a_permissioned_counselor_as_admin() {
        assert!(!actor("counselor", &["crm.leads.stage.backward"]).is_administrator());
        assert!(actor("tenant_admin", &["crm.leads.stage.backward"]).is_administrator());
        assert!(actor("counselor", &["*"]).is_administrator());
    }
}

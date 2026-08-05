use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction, postgres::PgRow};
use supercampus_database::Database;
use uuid::Uuid;

use crate::{
    api::dto::{CounselorCapacityRequest, CreateCampaignRequest, LeadFilters},
    domain::{Campaign, Communication, CrmError, FormDefinition, Lead, StageHistoryEntry},
};

#[derive(Debug, Clone)]
pub struct NewLead {
    pub source: String,
    pub source_detail: Value,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub whatsapp: Option<String>,
    pub parent_name: Option<String>,
    pub parent_phone: Option<String>,
    pub academic: Value,
    pub interest: Value,
    pub priority: String,
    pub follow_up_at: Option<DateTime<Utc>>,
    pub preferred_channel: Option<String>,
    pub consent_given: bool,
    pub custom_fields: Value,
}

#[derive(Debug, Clone, Default)]
pub struct LeadPatch {
    pub full_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub whatsapp: Option<String>,
    pub parent_name: Option<String>,
    pub parent_phone: Option<String>,
    pub academic: Option<Value>,
    pub interest: Option<Value>,
    pub priority: Option<String>,
    pub follow_up_at: Option<DateTime<Utc>>,
    pub fee_payment_confirmed: Option<bool>,
    pub documents_verified: Option<bool>,
    pub scholarship_status: Option<String>,
    pub custom_fields: Option<Value>,
}

#[derive(Clone)]
pub struct PostgresCrmRepository {
    database: Database,
}

#[allow(clippy::too_many_arguments)]
impl PostgresCrmRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn pool(&self) -> &PgPool {
        self.database.pool()
    }

    pub async fn find_duplicate_lead(
        &self,
        tenant_slug: &str,
        phone: Option<&str>,
        email: Option<&str>,
    ) -> Result<Option<Uuid>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let duplicate = sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM crm.leads
               WHERE tenant_id = $1 AND deleted_at IS NULL
                 AND (($2::text IS NOT NULL
                       AND regexp_replace(phone, '\\D', '', 'g') = regexp_replace($2, '\\D', '', 'g'))
                   OR ($3::text IS NOT NULL AND lower(email) = lower($3)))
               ORDER BY created_at ASC LIMIT 1"#,
        )
        .bind(tenant_id)
        .bind(phone)
        .bind(email)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(duplicate)
    }

    pub async fn create_lead(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        actor_role: &str,
        input: NewLead,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let duplicate_of = sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM crm.leads
               WHERE tenant_id = $1 AND deleted_at IS NULL
                 AND (($2::text IS NOT NULL
                       AND regexp_replace(phone, '\\D', '', 'g') = regexp_replace($2, '\\D', '', 'g'))
                   OR ($3::text IS NOT NULL AND lower(email) = lower($3)))
               ORDER BY created_at ASC LIMIT 1"#,
        )
        .bind(tenant_id)
        .bind(&input.phone)
        .bind(&input.email)
        .fetch_optional(&mut *transaction)
        .await?;

        let assignment = if is_digital_source(&input.source) {
            self.select_counselor(&mut transaction, tenant_id).await?
        } else {
            None
        };
        let assignment_type = assignment.as_ref().map(|_| "auto");
        let lead_id = Uuid::new_v4();
        let row = sqlx::query(
            r#"INSERT INTO crm.leads (
                   id, tenant_id, full_name, email, phone, whatsapp, parent_name, parent_phone,
                   source, source_detail, academic, interest, assigned_to, assigned_by,
                   assignment_type, priority, follow_up_at, preferred_channel, consent_given,
                   duplicate_of, custom_fields, created_by
               ) VALUES (
                   $1, $2, $3, $4, $5, $6, $7, $8,
                   $9, $10, $11, $12, $13, $14,
                   $15, $16, $17, $18, $19,
                   $20, $21, $22
               )
               RETURNING *"#,
        )
        .bind(lead_id)
        .bind(tenant_id)
        .bind(&input.full_name)
        .bind(&input.email)
        .bind(&input.phone)
        .bind(&input.whatsapp)
        .bind(&input.parent_name)
        .bind(&input.parent_phone)
        .bind(&input.source)
        .bind(&input.source_detail)
        .bind(&input.academic)
        .bind(&input.interest)
        .bind(&assignment)
        .bind(assignment.as_ref().map(|_| actor_id))
        .bind(assignment_type)
        .bind(&input.priority)
        .bind(input.follow_up_at)
        .bind(&input.preferred_channel)
        .bind(input.consent_given)
        .bind(duplicate_of)
        .bind(&input.custom_fields)
        .bind(actor_id)
        .fetch_one(&mut *transaction)
        .await?;

        self.insert_stage_history(
            &mut transaction,
            tenant_id,
            lead_id,
            None,
            None,
            "enquiry",
            "new",
            actor_id,
            actor_role,
            Some("lead_created"),
            None,
        )
        .await?;
        self.insert_event(
            &mut transaction,
            tenant_id,
            lead_id,
            "lead.created",
            json!({ "leadId": lead_id, "source": input.source }),
        )
        .await?;
        if let Some(counselor) = &assignment {
            self.insert_assignment_history(
                &mut transaction,
                tenant_id,
                lead_id,
                None,
                counselor,
                "auto",
                Some("digital_source_assignment"),
                actor_id,
            )
            .await?;
            self.insert_event(
                &mut transaction,
                tenant_id,
                lead_id,
                "lead.assigned",
                json!({ "leadId": lead_id, "toUserId": counselor, "assignmentType": "auto" }),
            )
            .await?;
        }
        if let Some(original_id) = duplicate_of {
            self.insert_event(
                &mut transaction,
                tenant_id,
                lead_id,
                "lead.duplicate_detected",
                json!({ "originalId": original_id, "duplicateId": lead_id }),
            )
            .await?;
        }
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn list_leads(
        &self,
        tenant_slug: &str,
        filters: &LeadFilters,
        scope_owner: Option<&str>,
    ) -> Result<Vec<Lead>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let mut builder =
            QueryBuilder::<Postgres>::new("SELECT l.* FROM crm.leads l WHERE l.tenant_id = ");
        builder.push_bind(tenant_id);
        builder.push(" AND l.deleted_at IS NULL");
        if !filters.include_archived.unwrap_or(false) {
            builder.push(" AND l.stage_key <> 'archived'");
        }
        if let Some(stage) = filters.stage.as_deref() {
            builder.push(" AND l.stage_key = ").push_bind(stage);
        }
        if let Some(substate) = filters.substate.as_deref() {
            builder.push(" AND l.substate_key = ").push_bind(substate);
        }
        let owner = scope_owner.or(filters.owner.as_deref());
        if let Some(owner) = owner {
            builder.push(" AND l.assigned_to = ").push_bind(owner);
        }
        if let Some(source) = filters.source.as_deref() {
            builder.push(" AND l.source = ").push_bind(source);
        }
        if let Some(status) = filters.global_status.as_deref() {
            builder.push(" AND l.global_status = ").push_bind(status);
        }
        if let Some(priority) = filters.priority.as_deref() {
            builder.push(" AND l.priority = ").push_bind(priority);
        }
        if let Some(program_id) = filters.program_id.as_deref() {
            builder
                .push(" AND l.interest->>'program_id' = ")
                .push_bind(program_id);
        }
        if let Some(created_from) = filters.created_from {
            builder
                .push(" AND l.created_at >= ")
                .push_bind(created_from);
        }
        if let Some(created_to) = filters.created_to {
            builder.push(" AND l.created_at <= ").push_bind(created_to);
        }
        if let Some(search) = filters.search.as_deref() {
            let pattern = format!("%{search}%");
            builder
                .push(" AND (l.full_name ILIKE ")
                .push_bind(pattern.clone())
                .push(" OR coalesce(l.email, '') ILIKE ")
                .push_bind(pattern.clone())
                .push(" OR coalesce(l.phone, '') ILIKE ")
                .push_bind(pattern)
                .push(" OR l.id::text = ")
                .push_bind(search)
                .push(")");
        }
        let limit = filters.limit.unwrap_or(100).clamp(1, 500);
        let offset = filters.offset.unwrap_or(0).max(0);
        builder
            .push(" ORDER BY l.stage_entered_at ASC, l.created_at ASC LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        let rows = builder.build().fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        rows.iter()
            .map(|row| row_to_lead(row, tenant_slug))
            .collect()
    }

    pub async fn find_lead(&self, tenant_slug: &str, lead_id: Uuid) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            "SELECT * FROM crm.leads WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn update_lead(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        patch: LeadPatch,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"UPDATE crm.leads SET
                   full_name = coalesce($3, full_name), email = coalesce($4, email),
                   phone = coalesce($5, phone), whatsapp = coalesce($6, whatsapp),
                   parent_name = coalesce($7, parent_name), parent_phone = coalesce($8, parent_phone),
                   academic = coalesce($9, academic), interest = coalesce($10, interest),
                   priority = coalesce($11, priority), follow_up_at = coalesce($12, follow_up_at),
                   fee_payment_confirmed = coalesce($13, fee_payment_confirmed),
                   documents_verified = coalesce($14, documents_verified),
                   scholarship_status = coalesce($15, scholarship_status),
                   custom_fields = coalesce($16, custom_fields), updated_at = now()
               WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL
               RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(patch.full_name)
        .bind(patch.email)
        .bind(patch.phone)
        .bind(patch.whatsapp)
        .bind(patch.parent_name)
        .bind(patch.parent_phone)
        .bind(patch.academic)
        .bind(patch.interest)
        .bind(patch.priority)
        .bind(patch.follow_up_at)
        .bind(patch.fee_payment_confirmed)
        .bind(patch.documents_verified)
        .bind(patch.scholarship_status)
        .bind(patch.custom_fields)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn soft_delete(&self, tenant_slug: &str, lead_id: Uuid) -> Result<(), CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let result = sqlx::query(
            "UPDATE crm.leads SET deleted_at = now(), updated_at = now() WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(CrmError::NotFound);
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn assign(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        new_owner: &str,
        assignment_type: &str,
        reason: Option<&str>,
        actor_id: &str,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let old_owner: Option<String> = sqlx::query_scalar(
            "SELECT assigned_to FROM crm.leads WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_optional(&mut *transaction)
        .await?
        .flatten();
        let row = sqlx::query(
            r#"UPDATE crm.leads SET assigned_to = $3, assigned_by = $4,
                   assignment_type = $5, updated_at = now()
               WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(new_owner)
        .bind(actor_id)
        .bind(assignment_type)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        self.insert_assignment_history(
            &mut transaction,
            tenant_id,
            lead_id,
            old_owner.as_deref(),
            new_owner,
            assignment_type,
            reason,
            actor_id,
        )
        .await?;
        self.insert_event(
            &mut transaction,
            tenant_id,
            lead_id,
            if old_owner.is_some() { "lead.reassigned" } else { "lead.assigned" },
            json!({ "leadId": lead_id, "oldOwner": old_owner, "newOwner": new_owner, "reason": reason }),
        )
        .await?;
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }
    pub async fn transition(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        to_stage: &str,
        to_substate: &str,
        actor_id: &str,
        actor_role: &str,
        reason: Option<&str>,
        notes: Option<&str>,
        ip_address: Option<&str>,
        automation_template: Option<&str>,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let current = sqlx::query(
            "SELECT stage_key, substate_key, global_status FROM crm.leads WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        let from_stage: String = current.try_get("stage_key")?;
        let from_substate: String = current.try_get("substate_key")?;
        let global_status: Option<String> = current.try_get("global_status")?;
        if global_status.as_deref() == Some("on_hold") {
            return Err(CrmError::Conflict(
                "lead is on hold; release the hold before moving stages".into(),
            ));
        }

        let row = sqlx::query(
            r#"UPDATE crm.leads SET stage_key = $3, substate_key = $4,
                   global_status = CASE WHEN $3 = 'archived' THEN 'archive' ELSE global_status END,
                   stage_entered_at = CASE WHEN stage_key <> $3 THEN now() ELSE stage_entered_at END,
                   erp_status = CASE
                       WHEN $3 = 'offer_status' AND $4 = 'accepted' THEN 'queued'
                       ELSE erp_status
                   END,
                   updated_at = now()
               WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(to_stage)
        .bind(to_substate)
        .fetch_one(&mut *transaction)
        .await?;
        self.insert_stage_history(
            &mut transaction,
            tenant_id,
            lead_id,
            Some(&from_stage),
            Some(&from_substate),
            to_stage,
            to_substate,
            actor_id,
            actor_role,
            reason,
            notes,
        )
        .await?;
        if let Some(ip_address) = ip_address {
            sqlx::query(
                "UPDATE crm.stage_history SET ip_address = $3 WHERE tenant_id = $1 AND lead_id = $2 AND id = (SELECT id FROM crm.stage_history WHERE tenant_id = $1 AND lead_id = $2 ORDER BY created_at DESC LIMIT 1)",
            )
            .bind(tenant_id)
            .bind(lead_id)
            .bind(ip_address)
            .execute(&mut *transaction)
            .await?;
        }
        self.insert_event(
            &mut transaction,
            tenant_id,
            lead_id,
            if to_stage == "archived" {
                "lead.archived"
            } else {
                "lead.moved"
            },
            json!({
                "leadId": lead_id, "fromStage": from_stage, "fromSubstate": from_substate,
                "toStage": to_stage, "toSubstate": to_substate, "byUser": actor_id
            }),
        )
        .await?;
        if let Some(template) = automation_template {
            self.queue_communication(
                &mut transaction,
                tenant_id,
                lead_id,
                "whatsapp",
                Some(template),
                None,
                json!({ "automation": "stage_transition", "stage": to_stage, "substate": to_substate }),
                None,
                "system",
            )
            .await?;
        }
        if to_stage == "offer_status" && to_substate == "accepted" {
            let fee_paid: bool = row.try_get("fee_payment_confirmed")?;
            let documents_verified: bool = row.try_get("documents_verified")?;
            let scholarship_status: String = row.try_get("scholarship_status")?;
            if !(documents_verified && (fee_paid || scholarship_status == "approved")) {
                return Err(CrmError::Validation(
                    "offer acceptance requires verified documents and paid fees or an approved scholarship"
                        .into(),
                ));
            }
            self.insert_event(
                &mut transaction,
                tenant_id,
                lead_id,
                "erp.handoff_requested",
                erp_payload(&row),
            )
            .await?;
        }
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn set_intake_status(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        status: &str,
        data: Value,
        actor_id: &str,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"UPDATE crm.leads SET global_status = $3, global_status_data = $4,
                   stage_key = 'qualified', substate_key = 'converted',
                   stage_entered_at = CASE WHEN stage_key <> 'qualified' THEN now() ELSE stage_entered_at END,
                   updated_at = now()
               WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(status)
        .bind(&data)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        self.insert_event(
            &mut transaction,
            tenant_id,
            lead_id,
            if status == "prospect" {
                "lead.prospect"
            } else {
                "lead.deferred"
            },
            json!({ "leadId": lead_id, "status": status, "data": data, "actorId": actor_id }),
        )
        .await?;
        self.queue_communication(
            &mut transaction,
            tenant_id,
            lead_id,
            "whatsapp",
            Some(if status == "prospect" {
                "intake_registered"
            } else {
                "deferral_confirmed"
            }),
            None,
            json!({ "automation": status }),
            None,
            "system",
        )
        .await?;
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn hold(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        reason: &str,
        hold_until: Option<NaiveDate>,
        reminder_date: Option<NaiveDate>,
        actor_id: &str,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        sqlx::query(
            r#"INSERT INTO crm.holds
               (tenant_id, lead_id, reason, hold_until, reminder_date, placed_by)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(reason)
        .bind(hold_until)
        .bind(reminder_date)
        .bind(actor_id)
        .execute(&mut *transaction)
        .await?;
        let data =
            json!({ "reason": reason, "holdUntil": hold_until, "reminderDate": reminder_date });
        let row = sqlx::query(
            "UPDATE crm.leads SET global_status = 'on_hold', global_status_data = $3, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL RETURNING *",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(&data)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        self.insert_event(
            &mut transaction,
            tenant_id,
            lead_id,
            "lead.hold",
            json!({ "leadId": lead_id, "reason": reason, "holdUntil": hold_until }),
        )
        .await?;
        self.queue_communication(
            &mut transaction,
            tenant_id,
            lead_id,
            "whatsapp",
            Some("hold_notification"),
            None,
            data,
            None,
            "system",
        )
        .await?;
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn release_hold(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        reason: &str,
        actor_id: &str,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let result = sqlx::query(
            r#"UPDATE crm.holds SET released_by = $3, release_reason = $4, released_at = now()
               WHERE tenant_id = $1 AND lead_id = $2 AND released_at IS NULL"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(actor_id)
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() == 0 {
            return Err(CrmError::Conflict(
                "lead does not have an active hold".into(),
            ));
        }
        let row = sqlx::query(
            "UPDATE crm.leads SET global_status = NULL, global_status_data = '{}'::jsonb, updated_at = now() WHERE tenant_id = $1 AND id = $2 RETURNING *",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_one(&mut *transaction)
        .await?;
        self.insert_event(
            &mut transaction,
            tenant_id,
            lead_id,
            "lead.hold_released",
            json!({ "leadId": lead_id, "reason": reason, "actorId": actor_id }),
        )
        .await?;
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn archive(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        reason: &str,
        notes: Option<&str>,
        actor_id: &str,
        actor_role: &str,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let current = sqlx::query(
            "SELECT stage_key, substate_key FROM crm.leads WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        let previous_stage: String = current.try_get("stage_key")?;
        let previous_substate: String = current.try_get("substate_key")?;
        sqlx::query(
            r#"INSERT INTO crm.archive_records
               (tenant_id, lead_id, previous_stage, previous_substate, reason, notes, archived_by)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(&previous_stage)
        .bind(&previous_substate)
        .bind(reason)
        .bind(notes)
        .bind(actor_id)
        .execute(&mut *transaction)
        .await?;
        let row = sqlx::query(
            r#"UPDATE crm.leads SET stage_key = 'archived', substate_key = 'closed',
                   global_status = 'archive', global_status_data = jsonb_build_object('reason', $3),
                   stage_entered_at = now(), updated_at = now()
               WHERE tenant_id = $1 AND id = $2 RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(reason)
        .fetch_one(&mut *transaction)
        .await?;
        self.insert_stage_history(
            &mut transaction,
            tenant_id,
            lead_id,
            Some(&previous_stage),
            Some(&previous_substate),
            "archived",
            "closed",
            actor_id,
            actor_role,
            Some(reason),
            notes,
        )
        .await?;
        self.insert_event(
            &mut transaction,
            tenant_id,
            lead_id,
            "lead.archived",
            json!({ "leadId": lead_id, "reason": reason, "byUser": actor_id }),
        )
        .await?;
        if reason != "Spam" {
            self.queue_communication(
                &mut transaction,
                tenant_id,
                lead_id,
                "whatsapp",
                Some("application_closed"),
                None,
                json!({ "archiveReason": reason }),
                None,
                "system",
            )
            .await?;
        }
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn unarchive(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        stage: &str,
        substate: &str,
        reason: &str,
        actor_id: &str,
        actor_role: &str,
    ) -> Result<Lead, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        sqlx::query(
            r#"UPDATE crm.archive_records SET unarchived_by = $3, unarchive_reason = $4,
                   unarchived_at = now()
               WHERE tenant_id = $1 AND lead_id = $2 AND unarchived_at IS NULL"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(actor_id)
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
        let row = sqlx::query(
            r#"UPDATE crm.leads SET stage_key = $3, substate_key = $4,
                   global_status = NULL, global_status_data = '{}'::jsonb,
                   stage_entered_at = now(), updated_at = now()
               WHERE tenant_id = $1 AND id = $2 AND stage_key = 'archived' RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(stage)
        .bind(substate)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        self.insert_stage_history(
            &mut transaction,
            tenant_id,
            lead_id,
            Some("archived"),
            Some("closed"),
            stage,
            substate,
            actor_id,
            actor_role,
            Some(reason),
            Some("unarchive"),
        )
        .await?;
        transaction.commit().await?;
        row_to_lead(&row, tenant_slug)
    }

    pub async fn timeline(&self, tenant_slug: &str, lead_id: Uuid) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let history_rows = sqlx::query(
            "SELECT * FROM crm.stage_history WHERE tenant_id = $1 AND lead_id = $2 ORDER BY created_at DESC",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_all(&mut *transaction)
        .await?;
        let communication_rows = sqlx::query(
            "SELECT * FROM crm.communications WHERE tenant_id = $1 AND lead_id = $2 ORDER BY created_at DESC",
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        let history: Vec<StageHistoryEntry> = history_rows
            .iter()
            .map(row_to_stage_history)
            .collect::<Result<_, _>>()?;
        let communications: Vec<Communication> = communication_rows
            .iter()
            .map(row_to_communication)
            .collect::<Result<_, _>>()?;
        Ok(json!({ "stageHistory": history, "communications": communications }))
    }
    pub async fn create_form(
        &self,
        tenant_slug: &str,
        name: &str,
        form_type: &str,
        program_id: Option<&str>,
        intake_year: Option<i32>,
        schema: Value,
        actor_id: &str,
    ) -> Result<FormDefinition, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"INSERT INTO crm.forms
               (tenant_id, name, form_type, program_id, intake_year, schema, created_by, updated_by)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $7) RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(name)
        .bind(form_type)
        .bind(program_id)
        .bind(intake_year)
        .bind(schema)
        .bind(actor_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        row_to_form(&row)
    }

    pub async fn list_forms(&self, tenant_slug: &str) -> Result<Vec<FormDefinition>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let rows = sqlx::query(
            "SELECT * FROM crm.forms WHERE tenant_id = $1 AND deleted_at IS NULL ORDER BY updated_at DESC",
        )
        .bind(tenant_id)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        rows.iter().map(row_to_form).collect()
    }

    pub async fn find_published_lead_capture_form(
        &self,
        tenant_slug: &str,
    ) -> Result<FormDefinition, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"SELECT * FROM crm.forms
               WHERE tenant_id = $1 AND status = 'published' AND deleted_at IS NULL
                 AND replace(lower(form_type), '-', '_') IN ('lead_capture', 'enquiry')
               ORDER BY updated_at DESC
               LIMIT 1"#,
        )
        .bind(tenant_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        transaction.commit().await?;
        row_to_form(&row)
    }

    pub async fn find_form(
        &self,
        tenant_slug: &str,
        form_id: Uuid,
    ) -> Result<FormDefinition, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            "SELECT * FROM crm.forms WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(form_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        transaction.commit().await?;
        row_to_form(&row)
    }

    pub async fn update_form(
        &self,
        tenant_slug: &str,
        form_id: Uuid,
        name: Option<&str>,
        form_type: Option<&str>,
        schema: Value,
        actor_id: &str,
    ) -> Result<FormDefinition, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"UPDATE crm.forms SET name = coalesce($3, name),
                   form_type = coalesce($4, form_type), schema = $5,
                   version = version + 1, status = 'draft', updated_by = $6, updated_at = now()
               WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(form_id)
        .bind(name)
        .bind(form_type)
        .bind(schema)
        .bind(actor_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        transaction.commit().await?;
        row_to_form(&row)
    }

    pub async fn set_form_status(
        &self,
        tenant_slug: &str,
        form_id: Uuid,
        status: &str,
        actor_id: &str,
    ) -> Result<FormDefinition, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            "UPDATE crm.forms SET status = $3, updated_by = $4, updated_at = now() WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL RETURNING *",
        )
        .bind(tenant_id)
        .bind(form_id)
        .bind(status)
        .bind(actor_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        transaction.commit().await?;
        row_to_form(&row)
    }

    pub async fn delete_form(&self, tenant_slug: &str, form_id: Uuid) -> Result<(), CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let result = sqlx::query(
            "UPDATE crm.forms SET deleted_at = now(), updated_at = now() WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(form_id)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(CrmError::NotFound);
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn submit_form(
        &self,
        tenant_slug: &str,
        form_id: Uuid,
        lead_id: Option<Uuid>,
        data: Value,
        actor_id: &str,
    ) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let form_version: i32 = sqlx::query_scalar(
            "SELECT version FROM crm.forms WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(form_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(CrmError::NotFound)?;
        let row = sqlx::query(
            r#"INSERT INTO crm.form_submissions
               (tenant_id, form_id, form_version, lead_id, data, submitted_by)
               VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, created_at"#,
        )
        .bind(tenant_id)
        .bind(form_id)
        .bind(form_version)
        .bind(lead_id)
        .bind(&data)
        .bind(actor_id)
        .fetch_one(&mut *transaction)
        .await?;
        let submission_id: Uuid = row.try_get("id")?;
        if let Some(lead_id) = lead_id {
            self.insert_event(
                &mut transaction,
                tenant_id,
                lead_id,
                "form.submitted",
                json!({ "formId": form_id, "submissionId": submission_id, "leadId": lead_id }),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(json!({
            "id": submission_id,
            "formId": form_id,
            "formVersion": form_version,
            "leadId": lead_id,
            "data": data,
            "createdAt": row.try_get::<DateTime<Utc>, _>("created_at")?
        }))
    }

    pub async fn list_submissions(
        &self,
        tenant_slug: &str,
        form_id: Uuid,
    ) -> Result<Vec<Value>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let rows = sqlx::query(
            "SELECT id, form_id, form_version, lead_id, data, submitted_by, created_at FROM crm.form_submissions WHERE tenant_id = $1 AND form_id = $2 ORDER BY created_at DESC",
        )
        .bind(tenant_id)
        .bind(form_id)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        rows.iter()
            .map(|row| {
                Ok(json!({
                    "id": row.try_get::<Uuid, _>("id")?,
                    "formId": row.try_get::<Uuid, _>("form_id")?,
                    "formVersion": row.try_get::<i32, _>("form_version")?,
                    "leadId": row.try_get::<Option<Uuid>, _>("lead_id")?,
                    "data": row.try_get::<Value, _>("data")?,
                    "submittedBy": row.try_get::<String, _>("submitted_by")?,
                    "createdAt": row.try_get::<DateTime<Utc>, _>("created_at")?
                }))
            })
            .collect()
    }

    pub async fn send_communication(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
        channel: &str,
        template_key: Option<&str>,
        subject: Option<&str>,
        content: Value,
        outcome: Option<&str>,
        actor_id: &str,
    ) -> Result<Communication, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = self
            .queue_communication(
                &mut transaction,
                tenant_id,
                lead_id,
                channel,
                template_key,
                subject,
                content,
                outcome,
                actor_id,
            )
            .await?;
        transaction.commit().await?;
        row_to_communication(&row)
    }

    pub async fn list_templates(&self, tenant_slug: &str) -> Result<Vec<Value>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let rows = sqlx::query(
            "SELECT id, template_key, channel, name, content, language, status, updated_at FROM crm.communication_templates WHERE tenant_id = $1 ORDER BY template_key, language",
        )
        .bind(tenant_id)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        rows.iter().map(template_to_json).collect()
    }

    pub async fn create_template(
        &self,
        tenant_slug: &str,
        template_key: &str,
        channel: &str,
        name: &str,
        content: &str,
        language: &str,
        actor_id: &str,
    ) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"INSERT INTO crm.communication_templates
               (tenant_id, template_key, channel, name, content, language, created_by)
               VALUES ($1, $2, $3, $4, $5, $6, $7)
               ON CONFLICT (tenant_id, template_key, language) DO UPDATE
               SET channel = EXCLUDED.channel, name = EXCLUDED.name, content = EXCLUDED.content,
                   updated_at = now()
               RETURNING id, template_key, channel, name, content, language, status, updated_at"#,
        )
        .bind(tenant_id)
        .bind(template_key)
        .bind(channel)
        .bind(name)
        .bind(content)
        .bind(language)
        .bind(actor_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        template_to_json(&row)
    }

    pub async fn upsert_counselor(
        &self,
        tenant_slug: &str,
        request: &CounselorCapacityRequest,
    ) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"INSERT INTO crm.counselor_capacity
               (tenant_id, user_id, display_name, active, max_capacity, source_categories,
                program_ids, territories, average_response_minutes, conversion_rate)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
               ON CONFLICT (tenant_id, user_id) DO UPDATE SET
                   display_name = EXCLUDED.display_name, active = EXCLUDED.active,
                   max_capacity = EXCLUDED.max_capacity, source_categories = EXCLUDED.source_categories,
                   program_ids = EXCLUDED.program_ids, territories = EXCLUDED.territories,
                   average_response_minutes = EXCLUDED.average_response_minutes,
                   conversion_rate = EXCLUDED.conversion_rate, updated_at = now()
               RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(&request.user_id)
        .bind(&request.display_name)
        .bind(request.active.unwrap_or(true))
        .bind(request.max_capacity.unwrap_or(100))
        .bind(&request.source_categories)
        .bind(&request.program_ids)
        .bind(&request.territories)
        .bind(request.average_response_minutes.unwrap_or(60.0))
        .bind(request.conversion_rate.unwrap_or(0.0))
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        counselor_to_json(&row)
    }

    pub async fn list_counselors(&self, tenant_slug: &str) -> Result<Vec<Value>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let rows = sqlx::query(
            r#"SELECT c.*,
                      count(l.id) FILTER (WHERE l.deleted_at IS NULL AND l.stage_key <> 'archived') AS active_leads
               FROM crm.counselor_capacity c
               LEFT JOIN crm.leads l ON l.tenant_id = c.tenant_id AND l.assigned_to = c.user_id
               WHERE c.tenant_id = $1
               GROUP BY c.tenant_id, c.user_id
               ORDER BY c.display_name"#,
        )
        .bind(tenant_id)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        rows.iter().map(counselor_to_json).collect()
    }

    pub async fn list_campaigns(&self, tenant_slug: &str) -> Result<Vec<Campaign>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let rows = sqlx::query(
            "SELECT id, name, source, budget::double precision AS budget, spent::double precision AS spent, attributed_revenue::double precision AS attributed_revenue, landing_pages, utm_code, status, starts_on, ends_on, created_at, updated_at FROM crm.campaigns WHERE tenant_id = $1 ORDER BY updated_at DESC, name",
        )
            .bind(tenant_id)
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;
        rows.iter().map(row_to_campaign).collect()
    }

    pub async fn upsert_campaign(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        request: &CreateCampaignRequest,
    ) -> Result<Campaign, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"INSERT INTO crm.campaigns
               (tenant_id, name, source, budget, spent, attributed_revenue, landing_pages,
                utm_code, status, starts_on, ends_on, updated_by)
               VALUES ($1, $2, $3, CAST($4 AS numeric), CAST($5 AS numeric),
                       CAST($6 AS numeric), $7, $8, $9, $10, $11, $12)
               ON CONFLICT (tenant_id, name) DO UPDATE SET
                   source = EXCLUDED.source, budget = EXCLUDED.budget, spent = EXCLUDED.spent,
                   attributed_revenue = EXCLUDED.attributed_revenue,
                   landing_pages = EXCLUDED.landing_pages, utm_code = EXCLUDED.utm_code,
                   status = EXCLUDED.status, starts_on = EXCLUDED.starts_on, ends_on = EXCLUDED.ends_on,
                   updated_by = EXCLUDED.updated_by, updated_at = now()
               RETURNING id, name, source, budget::double precision AS budget,
                         spent::double precision AS spent,
                         attributed_revenue::double precision AS attributed_revenue,
                         landing_pages, utm_code, status, starts_on, ends_on, created_at, updated_at"#,
        )
        .bind(tenant_id)
        .bind(request.name.trim())
        .bind(request.source.trim())
        .bind(request.budget.unwrap_or(0.0))
        .bind(request.spent.unwrap_or(0.0))
        .bind(request.attributed_revenue.unwrap_or(0.0))
        .bind(request.landing_pages.unwrap_or(0))
        .bind(request.utm_code.as_deref())
        .bind(request.status.as_deref().unwrap_or("draft"))
        .bind(request.starts_on)
        .bind(request.ends_on)
        .bind(actor_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        row_to_campaign(&row)
    }

    pub async fn count_post_qualified_whatsapp(
        &self,
        tenant_slug: &str,
        owner_scope: Option<&str>,
    ) -> Result<i64, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let count = sqlx::query_scalar::<_, i64>(
            r#"SELECT count(DISTINCT lead.id)
               FROM crm.leads AS lead
               JOIN crm.communications AS communication
                 ON communication.tenant_id = lead.tenant_id AND communication.lead_id = lead.id
               WHERE lead.tenant_id = $1 AND lead.deleted_at IS NULL
                 AND lead.stage_key IN ('qualified', 'application', 'application_status', 'offer_status')
                 AND communication.channel = 'whatsapp'
                 AND ($2::text IS NULL OR lead.assigned_to = $2)"#,
        )
        .bind(tenant_id)
        .bind(owner_scope)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(count)
    }

    pub async fn list_configuration(&self, tenant_slug: &str) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let workflow = sqlx::query(
            "SELECT * FROM crm.workflow_toggles WHERE tenant_id = $1 ORDER BY from_stage, to_stage",
        )
        .bind(tenant_id)
        .fetch_all(&mut *transaction)
        .await?;
        let automations = sqlx::query(
            "SELECT * FROM crm.automation_toggles WHERE tenant_id = $1 ORDER BY stage, trigger_name",
        )
        .bind(tenant_id)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(json!({
            "workflowToggles": workflow.iter().map(workflow_toggle_to_json).collect::<Result<Vec<_>, CrmError>>()?,
            "automationToggles": automations.iter().map(automation_toggle_to_json).collect::<Result<Vec<_>, CrmError>>()?
        }))
    }

    pub async fn upsert_workflow_toggle(
        &self,
        tenant_slug: &str,
        from_stage: &str,
        to_stage: &str,
        allowed_roles: Value,
        requires_approval: bool,
        approval_role: Option<&str>,
        enabled: bool,
        actor_id: &str,
    ) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"INSERT INTO crm.workflow_toggles
               (tenant_id, from_stage, to_stage, allowed_roles, requires_approval, approval_role, enabled, updated_by)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (tenant_id, from_stage, to_stage) DO UPDATE SET
                   allowed_roles = EXCLUDED.allowed_roles, requires_approval = EXCLUDED.requires_approval,
                   approval_role = EXCLUDED.approval_role, enabled = EXCLUDED.enabled,
                   updated_by = EXCLUDED.updated_by, updated_at = now()
               RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(from_stage)
        .bind(to_stage)
        .bind(allowed_roles)
        .bind(requires_approval)
        .bind(approval_role)
        .bind(enabled)
        .bind(actor_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        workflow_toggle_to_json(&row)
    }

    pub async fn upsert_automation_toggle(
        &self,
        tenant_slug: &str,
        stage: &str,
        trigger_name: &str,
        action: &str,
        template_key: Option<&str>,
        conditions: Value,
        enabled: bool,
        actor_id: &str,
    ) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let row = sqlx::query(
            r#"INSERT INTO crm.automation_toggles
               (tenant_id, stage, trigger_name, action, template_key, conditions, enabled, updated_by)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (tenant_id, stage, trigger_name, action) DO UPDATE SET
                   template_key = EXCLUDED.template_key, conditions = EXCLUDED.conditions,
                   enabled = EXCLUDED.enabled, updated_by = EXCLUDED.updated_by, updated_at = now()
               RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(stage)
        .bind(trigger_name)
        .bind(action)
        .bind(template_key)
        .bind(conditions)
        .bind(enabled)
        .bind(actor_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        automation_toggle_to_json(&row)
    }

    pub async fn audit_permission(
        &self,
        tenant_slug: &str,
        actor_id: &str,
        actor_role: &str,
        action: &str,
        entity_type: &str,
        entity_id: Option<String>,
        allowed: bool,
        reason: Option<&str>,
    ) -> Result<(), CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        sqlx::query(
            r#"INSERT INTO crm.permission_audit
               (tenant_id, actor_id, actor_role, action, entity_type, entity_id, allowed, reason)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        )
        .bind(tenant_id)
        .bind(actor_id)
        .bind(actor_role)
        .bind(action)
        .bind(entity_type)
        .bind(entity_id)
        .bind(allowed)
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn list_authorization_roles(&self, tenant_slug: &str) -> Result<Value, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let rows = sqlx::query(
            r#"SELECT role.id, role.role_key, role.name, role.team, role.scope_description,
                      role.protected, role.active,
                      COALESCE((
                          SELECT jsonb_agg(jsonb_build_object(
                              'key', role_grant.permission_key,
                              'scope', role_grant.scope,
                              'constraints', role_grant.constraints
                          ) ORDER BY role_grant.permission_key)
                          FROM authz.role_permissions role_grant
                          WHERE role_grant.tenant_id = role.tenant_id AND role_grant.role_id = role.id
                      ), '[]'::jsonb) AS permissions
               FROM authz.roles role
               WHERE role.tenant_id = $1
               ORDER BY role.protected DESC, role.name"#,
        )
        .bind(tenant_id)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        let roles = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "id": row.try_get::<Uuid, _>("id")?,
                    "key": row.try_get::<String, _>("role_key")?,
                    "name": row.try_get::<String, _>("name")?,
                    "team": row.try_get::<String, _>("team")?,
                    "scope": row.try_get::<String, _>("scope_description")?,
                    "protected": row.try_get::<bool, _>("protected")?,
                    "active": row.try_get::<bool, _>("active")?,
                    "permissions": row.try_get::<Value, _>("permissions")?,
                }))
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        Ok(json!(roles))
    }

    async fn begin_tenant(
        &self,
        tenant_slug: &str,
    ) -> Result<(Uuid, Transaction<'_, Postgres>), CrmError> {
        let tenant_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO platform.tenants (slug, code, name)
               VALUES ($1, upper(replace($1, '-', '_')), $1)
               ON CONFLICT (slug) DO UPDATE SET updated_at = platform.tenants.updated_at
               RETURNING id"#,
        )
        .bind(tenant_slug)
        .fetch_one(self.database.pool())
        .await?;
        let mut transaction = self.database.pool().begin().await?;
        sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
            .bind(tenant_id.to_string())
            .execute(&mut *transaction)
            .await?;
        Ok((tenant_id, transaction))
    }

    async fn select_counselor(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        tenant_id: Uuid,
    ) -> Result<Option<String>, CrmError> {
        let counselor = sqlx::query_scalar::<_, String>(
            r#"SELECT c.user_id
               FROM crm.counselor_capacity c
               LEFT JOIN LATERAL (
                   SELECT count(*)::double precision AS active_count
                   FROM crm.leads l
                   WHERE l.tenant_id = c.tenant_id AND l.assigned_to = c.user_id
                     AND l.deleted_at IS NULL AND l.stage_key <> 'archived'
               ) load ON true
               WHERE c.tenant_id = $1 AND c.active
                 AND load.active_count < c.max_capacity
               ORDER BY (
                   0.4 / (load.active_count + 1)
                   + 0.3 / (c.average_response_minutes + 1)
                   + 0.3 * c.conversion_rate
               ) DESC, c.last_assigned_at NULLS FIRST, c.user_id
               FOR UPDATE OF c SKIP LOCKED LIMIT 1"#,
        )
        .bind(tenant_id)
        .fetch_optional(&mut **transaction)
        .await?;
        if let Some(user_id) = &counselor {
            sqlx::query(
                "UPDATE crm.counselor_capacity SET last_assigned_at = now(), updated_at = now() WHERE tenant_id = $1 AND user_id = $2",
            )
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut **transaction)
            .await?;
        }
        Ok(counselor)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_stage_history(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        tenant_id: Uuid,
        lead_id: Uuid,
        from_stage: Option<&str>,
        from_substate: Option<&str>,
        to_stage: &str,
        to_substate: &str,
        actor_id: &str,
        actor_role: &str,
        reason: Option<&str>,
        notes: Option<&str>,
    ) -> Result<(), CrmError> {
        sqlx::query(
            r#"INSERT INTO crm.stage_history
               (tenant_id, lead_id, from_stage, from_substate, to_stage, to_substate,
                actor_id, actor_role, reason, notes)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(from_stage)
        .bind(from_substate)
        .bind(to_stage)
        .bind(to_substate)
        .bind(actor_id)
        .bind(actor_role)
        .bind(reason)
        .bind(notes)
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_assignment_history(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        tenant_id: Uuid,
        lead_id: Uuid,
        old_owner: Option<&str>,
        new_owner: &str,
        assignment_type: &str,
        reason: Option<&str>,
        actor_id: &str,
    ) -> Result<(), CrmError> {
        sqlx::query(
            r#"INSERT INTO crm.assignment_history
               (tenant_id, lead_id, old_owner, new_owner, assignment_type, reason, actor_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(old_owner)
        .bind(new_owner)
        .bind(assignment_type)
        .bind(reason)
        .bind(actor_id)
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    async fn insert_event(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        tenant_id: Uuid,
        aggregate_id: Uuid,
        event_type: &str,
        payload: Value,
    ) -> Result<(), CrmError> {
        sqlx::query(
            "INSERT INTO crm.outbox_events (tenant_id, aggregate_id, event_type, payload) VALUES ($1, $2, $3, $4)",
        )
        .bind(tenant_id)
        .bind(aggregate_id)
        .bind(event_type)
        .bind(payload)
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn queue_communication(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        tenant_id: Uuid,
        lead_id: Uuid,
        channel: &str,
        template_key: Option<&str>,
        subject: Option<&str>,
        content: Value,
        outcome: Option<&str>,
        actor_id: &str,
    ) -> Result<PgRow, CrmError> {
        let row = sqlx::query(
            r#"INSERT INTO crm.communications
               (tenant_id, lead_id, channel, template_key, subject, content, outcome, actor_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .bind(channel)
        .bind(template_key)
        .bind(subject)
        .bind(content)
        .bind(outcome)
        .bind(actor_id)
        .fetch_one(&mut **transaction)
        .await?;
        self.insert_event(
            transaction,
            tenant_id,
            lead_id,
            "communication.queued",
            json!({ "leadId": lead_id, "channel": channel, "templateKey": template_key }),
        )
        .await?;
        Ok(row)
    }
    pub async fn events_after(
        &self,
        tenant_slug: &str,
        cursor: i64,
    ) -> Result<Vec<Value>, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let rows = sqlx::query(
            r#"SELECT sequence_id, event_type, aggregate_id, payload, created_at
               FROM crm.outbox_events
               WHERE tenant_id = $1 AND sequence_id > $2
               ORDER BY sequence_id
               LIMIT 100"#,
        )
        .bind(tenant_id)
        .bind(cursor)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        rows.iter()
            .map(|row| {
                Ok(json!({
                    "cursor": row.try_get::<i64, _>("sequence_id")?,
                    "eventType": row.try_get::<String, _>("event_type")?,
                    "aggregateId": row.try_get::<Uuid, _>("aggregate_id")?,
                    "payload": row.try_get::<Value, _>("payload")?,
                    "createdAt": row.try_get::<DateTime<Utc>, _>("created_at")?
                }))
            })
            .collect()
    }
    pub async fn has_reached_qualified(
        &self,
        tenant_slug: &str,
        lead_id: Uuid,
    ) -> Result<bool, CrmError> {
        let (tenant_id, mut transaction) = self.begin_tenant(tenant_slug).await?;
        let reached = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (
                   SELECT 1 FROM crm.stage_history
                   WHERE tenant_id = $1 AND lead_id = $2 AND to_stage = 'qualified'
               ) OR EXISTS (
                   SELECT 1 FROM crm.leads
                   WHERE tenant_id = $1 AND id = $2
                     AND stage_key IN ('qualified', 'application', 'application_status', 'offer_status')
               )"#,
        )
        .bind(tenant_id)
        .bind(lead_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(reached)
    }
}

impl From<sqlx::Error> for CrmError {
    fn from(error: sqlx::Error) -> Self {
        CrmError::Storage(error.to_string())
    }
}

fn row_to_lead(row: &PgRow, tenant_slug: &str) -> Result<Lead, CrmError> {
    Ok(Lead {
        id: row.try_get("id")?,
        tenant_id: tenant_slug.to_owned(),
        full_name: row.try_get("full_name")?,
        email: row.try_get("email")?,
        phone: row.try_get("phone")?,
        whatsapp: row.try_get("whatsapp")?,
        parent_name: row.try_get("parent_name")?,
        parent_phone: row.try_get("parent_phone")?,
        source: row.try_get("source")?,
        source_detail: row.try_get("source_detail")?,
        academic: row.try_get("academic")?,
        interest: row.try_get("interest")?,
        pipeline_key: row.try_get("pipeline_key")?,
        stage_key: row.try_get("stage_key")?,
        substate_key: row.try_get("substate_key")?,
        global_status: row.try_get("global_status")?,
        global_status_data: row.try_get("global_status_data")?,
        assigned_to: row.try_get("assigned_to")?,
        assigned_by: row.try_get("assigned_by")?,
        assignment_type: row.try_get("assignment_type")?,
        priority: row.try_get("priority")?,
        follow_up_at: row.try_get("follow_up_at")?,
        preferred_channel: row.try_get("preferred_channel")?,
        consent_given: row.try_get("consent_given")?,
        fee_payment_confirmed: row.try_get("fee_payment_confirmed")?,
        documents_verified: row.try_get("documents_verified")?,
        scholarship_status: row.try_get("scholarship_status")?,
        erp_status: row.try_get("erp_status")?,
        erp_student_id: row.try_get("erp_student_id")?,
        erp_enrollment_number: row.try_get("erp_enrollment_number")?,
        duplicate_of: row.try_get("duplicate_of")?,
        custom_fields: row.try_get("custom_fields")?,
        created_by: row.try_get("created_by")?,
        stage_entered_at: row.try_get("stage_entered_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn row_to_stage_history(row: &PgRow) -> Result<StageHistoryEntry, CrmError> {
    Ok(StageHistoryEntry {
        id: row.try_get("id")?,
        from_stage: row.try_get("from_stage")?,
        from_substate: row.try_get("from_substate")?,
        to_stage: row.try_get("to_stage")?,
        to_substate: row.try_get("to_substate")?,
        actor_id: row.try_get("actor_id")?,
        actor_role: row.try_get("actor_role")?,
        reason: row.try_get("reason")?,
        notes: row.try_get("notes")?,
        created_at: row.try_get("created_at")?,
    })
}

fn row_to_communication(row: &PgRow) -> Result<Communication, CrmError> {
    Ok(Communication {
        id: row.try_get("id")?,
        lead_id: row.try_get("lead_id")?,
        channel: row.try_get("channel")?,
        direction: row.try_get("direction")?,
        template_key: row.try_get("template_key")?,
        subject: row.try_get("subject")?,
        content: row.try_get("content")?,
        outcome: row.try_get("outcome")?,
        status: row.try_get("status")?,
        actor_id: row.try_get("actor_id")?,
        created_at: row.try_get("created_at")?,
    })
}

fn row_to_form(row: &PgRow) -> Result<FormDefinition, CrmError> {
    Ok(FormDefinition {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        form_type: row.try_get("form_type")?,
        program_id: row.try_get("program_id")?,
        intake_year: row.try_get("intake_year")?,
        version: row.try_get("version")?,
        status: row.try_get("status")?,
        schema: row.try_get("schema")?,
        created_by: row.try_get("created_by")?,
        updated_by: row.try_get("updated_by")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn row_to_campaign(row: &PgRow) -> Result<Campaign, CrmError> {
    Ok(Campaign {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        source: row.try_get("source")?,
        budget: row.try_get("budget")?,
        spent: row.try_get("spent")?,
        attributed_revenue: row.try_get("attributed_revenue")?,
        landing_pages: row.try_get("landing_pages")?,
        utm_code: row.try_get("utm_code")?,
        status: row.try_get("status")?,
        starts_on: row.try_get("starts_on")?,
        ends_on: row.try_get("ends_on")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn template_to_json(row: &PgRow) -> Result<Value, CrmError> {
    Ok(json!({
        "id": row.try_get::<Uuid, _>("id")?, "templateKey": row.try_get::<String, _>("template_key")?,
        "channel": row.try_get::<String, _>("channel")?, "name": row.try_get::<String, _>("name")?,
        "content": row.try_get::<String, _>("content")?, "language": row.try_get::<String, _>("language")?,
        "status": row.try_get::<String, _>("status")?, "updatedAt": row.try_get::<DateTime<Utc>, _>("updated_at")?
    }))
}

fn counselor_to_json(row: &PgRow) -> Result<Value, CrmError> {
    Ok(json!({
        "userId": row.try_get::<String, _>("user_id")?, "displayName": row.try_get::<String, _>("display_name")?,
        "active": row.try_get::<bool, _>("active")?, "maxCapacity": row.try_get::<i32, _>("max_capacity")?,
        "sourceCategories": row.try_get::<Value, _>("source_categories")?,
        "programIds": row.try_get::<Value, _>("program_ids")?, "territories": row.try_get::<Value, _>("territories")?,
        "averageResponseMinutes": row.try_get::<f64, _>("average_response_minutes")?,
        "conversionRate": row.try_get::<f64, _>("conversion_rate")?,
        "activeLeads": row.try_get::<i64, _>("active_leads").unwrap_or(0)
    }))
}

fn workflow_toggle_to_json(row: &PgRow) -> Result<Value, CrmError> {
    Ok(json!({
        "id": row.try_get::<Uuid, _>("id")?, "fromStage": row.try_get::<String, _>("from_stage")?,
        "toStage": row.try_get::<String, _>("to_stage")?, "allowedRoles": row.try_get::<Value, _>("allowed_roles")?,
        "requiresApproval": row.try_get::<bool, _>("requires_approval")?,
        "approvalRole": row.try_get::<Option<String>, _>("approval_role")?, "enabled": row.try_get::<bool, _>("enabled")?
    }))
}

fn automation_toggle_to_json(row: &PgRow) -> Result<Value, CrmError> {
    Ok(json!({
        "id": row.try_get::<Uuid, _>("id")?, "stage": row.try_get::<String, _>("stage")?,
        "triggerName": row.try_get::<String, _>("trigger_name")?, "action": row.try_get::<String, _>("action")?,
        "templateKey": row.try_get::<Option<String>, _>("template_key")?,
        "conditions": row.try_get::<Value, _>("conditions")?, "enabled": row.try_get::<bool, _>("enabled")?,
        "mandatory": row.try_get::<bool, _>("mandatory")?
    }))
}

fn erp_payload(row: &PgRow) -> Value {
    json!({
        "crmLeadId": row.try_get::<Uuid, _>("id").ok(),
        "studentName": row.try_get::<String, _>("full_name").ok(),
        "studentEmail": row.try_get::<Option<String>, _>("email").ok().flatten(),
        "studentPhone": row.try_get::<Option<String>, _>("phone").ok().flatten(),
        "programId": row.try_get::<Value, _>("interest").ok().and_then(|value| value.get("program_id").cloned()),
        "feePaymentConfirmed": row.try_get::<bool, _>("fee_payment_confirmed").unwrap_or(false),
        "documentsVerified": row.try_get::<bool, _>("documents_verified").unwrap_or(false),
        "scholarshipStatus": row.try_get::<String, _>("scholarship_status").ok()
    })
}

fn is_digital_source(source: &str) -> bool {
    const DIGITAL: &[&str] = &[
        "AI Search Engine",
        "Bing Search",
        "Google Search",
        "Google Ads",
        "Google My Business",
        "Facebook",
        "Instagram",
        "LinkedIn",
        "Youtube",
        "CollegeDekho",
        "Collegedunia",
        "Shiksha",
        "Careers360",
        "Jagran Josh",
        "MEC Website",
        "Other Aggregated Website",
        "Other Search Engines",
        "Quora Answers",
        "In-Bound Call",
        "In-Bound WhatsApp",
        "SMS Broadcast",
        "Whatsapp Broadcast",
        "Webinars",
        "TNEA Counselling",
    ];
    DIGITAL.iter().any(|item| item.eq_ignore_ascii_case(source))
}

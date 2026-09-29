//! One-shot release patches for deployments that skip the SQLx migrator.
//!
//! Production starts the API with `SKIP_STARTUP_MIGRATIONS=true` and replays a
//! list of runtime migrations on every boot as "compatibility patches". That is
//! harmless for permission catalogues, but the identity patches (seeded
//! accounts, the MEC identity restore) write display names, passwords, roles
//! and `active` flags. Replaying them on every deploy silently reverted every
//! rename an administrator had saved, reactivated deactivated accounts and
//! signed every MEC user out.
//!
//! Patches applied through [`apply_once`] run at most once per database and are
//! recorded in `platform.release_patch_ledger`. A database that already carries
//! a patch's effects (every production database, because the previous binary
//! replayed them on each boot) is recorded as `baseline` without running it
//! again.

use anyhow::Context;
use sqlx::PgPool;

/// A release patch that must not be replayed once applied.
pub struct OncePatch {
    /// Stable ledger key. Never reuse a key for different SQL.
    pub key: &'static str,
    pub sql: &'static str,
    /// Returns a single boolean: `true` when the database already holds the
    /// patch's effects, so it is recorded without running.
    pub already_applied_probe: &'static str,
}

/// Outcome of [`apply_once`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOutcome {
    /// The ledger already records the patch.
    Skipped,
    /// The database already held the patch's effects; recorded, not run.
    Baselined,
    /// The patch ran and was recorded.
    Applied,
}

pub const LEDGER_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS platform;
CREATE TABLE IF NOT EXISTS platform.release_patch_ledger (
    patch_key text PRIMARY KEY,
    mode text NOT NULL CHECK (mode IN ('applied', 'baseline')),
    applied_at timestamptz NOT NULL DEFAULT now()
);
"#;

/// Decides what to do with a patch given the ledger and the probe.
pub fn decide(recorded: bool, effects_present: bool) -> PatchOutcome {
    if recorded {
        PatchOutcome::Skipped
    } else if effects_present {
        PatchOutcome::Baselined
    } else {
        PatchOutcome::Applied
    }
}

/// Applies `patch` at most once for the database behind `pool`. Concurrent
/// replicas serialise on a transaction-scoped advisory lock.
pub async fn apply_once(pool: &PgPool, patch: &OncePatch) -> anyhow::Result<PatchOutcome> {
    sqlx::raw_sql(LEDGER_DDL)
        .execute(pool)
        .await
        .context("failed to create the release patch ledger")?;
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('supercampus.release_patch_ledger'))")
        .execute(&mut *transaction)
        .await?;
    let recorded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM platform.release_patch_ledger WHERE patch_key = $1)",
    )
    .bind(patch.key)
    .fetch_one(&mut *transaction)
    .await?;
    let effects_present = if recorded {
        true
    } else {
        sqlx::query_scalar::<_, bool>(patch.already_applied_probe)
            .fetch_one(&mut *transaction)
            .await
            .with_context(|| format!("release patch probe failed for {}", patch.key))?
    };
    let outcome = decide(recorded, effects_present);
    match outcome {
        PatchOutcome::Skipped => {}
        PatchOutcome::Baselined | PatchOutcome::Applied => {
            if outcome == PatchOutcome::Applied {
                sqlx::raw_sql(patch.sql)
                    .execute(&mut *transaction)
                    .await
                    .with_context(|| format!("failed to apply release patch {}", patch.key))?;
            }
            sqlx::query(
                r#"INSERT INTO platform.release_patch_ledger (patch_key, mode)
                   VALUES ($1, $2) ON CONFLICT (patch_key) DO NOTHING"#,
            )
            .bind(patch.key)
            .bind(if outcome == PatchOutcome::Applied {
                "applied"
            } else {
                "baseline"
            })
            .execute(&mut *transaction)
            .await?;
        }
    }
    transaction.commit().await?;
    if outcome != PatchOutcome::Skipped {
        tracing::info!(patch = patch.key, ?outcome, "release patch recorded");
    }
    Ok(outcome)
}

/// Registers `authorization.users.delete` (0147) for every tenant. Idempotent
/// and safe to replay: the role grant never overrides a removed grant.
pub async fn ensure_user_delete_permission(pool: &PgPool) {
    if let Err(error) = sqlx::raw_sql(include_str!(
        "../../../migrations/runtime/0147_user_delete_permission.sql"
    ))
    .execute(pool)
    .await
    {
        tracing::warn!(%error, "user delete permission not applied");
    }
}

/// Accountant identity rename (0073).
pub const MEC_ACCOUNTANT_IDENTITY: OncePatch = OncePatch {
    key: "0073_abhinaya_accountant_portal",
    sql: include_str!("../../../migrations/runtime/0073_abhinaya_accountant_portal.sql"),
    already_applied_probe: "SELECT EXISTS(SELECT 1 FROM identity.users \
        WHERE id = '4b273ab2-0a54-571d-b53d-904fecb013d4'::uuid)",
};

/// Gate security accounts (0074).
pub const MEC_GATE_SECURITY_ACCOUNTS: OncePatch = OncePatch {
    key: "0074_gate_security_portal",
    sql: include_str!("../../../migrations/runtime/0074_gate_security_portal.sql"),
    already_applied_probe: "SELECT EXISTS(SELECT 1 FROM identity.users \
        WHERE lower(email) = 'security@mec.local')",
};

/// Canteen captain accounts (0075).
pub const MEC_CANTEEN_CAPTAIN_ACCOUNTS: OncePatch = OncePatch {
    key: "0075_mec_canteen_captains",
    sql: include_str!("../../../migrations/runtime/0075_mec_canteen_captains.sql"),
    already_applied_probe: "SELECT EXISTS(SELECT 1 FROM identity.users \
        WHERE lower(email) = 'shashi@mec.local')",
};

/// Librarian and stationery accounts (0079).
pub const MEC_LIBRARIAN_STATIONERY_ACCOUNTS: OncePatch = OncePatch {
    key: "0079_mec_librarian_and_stationery_accounts",
    sql: include_str!("../../../migrations/runtime/0079_mec_librarian_and_stationery_accounts.sql"),
    already_applied_probe: "SELECT EXISTS(SELECT 1 FROM identity.users \
        WHERE lower(email) = 'stationary@mec.local')",
};

/// One-time restore of canonical MEC names (0095). The previous binary
/// replayed it on every boot, so any database with MEC members has it.
pub const MEC_IDENTITY_RESTORE: OncePatch = OncePatch {
    key: "0095_restore_mec_user_identities",
    sql: include_str!("../../../migrations/runtime/0095_restore_mec_user_identities.sql"),
    already_applied_probe: "SELECT EXISTS(SELECT 1 FROM identity.tenant_memberships membership \
        JOIN platform.tenants tenant ON tenant.id = membership.tenant_id \
        WHERE tenant.slug = 'mec')",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_patch_never_runs_again() {
        assert_eq!(decide(true, false), PatchOutcome::Skipped);
        assert_eq!(decide(true, true), PatchOutcome::Skipped);
    }

    #[test]
    fn existing_effects_are_baselined_not_replayed() {
        assert_eq!(decide(false, true), PatchOutcome::Baselined);
        assert_eq!(decide(false, false), PatchOutcome::Applied);
    }

    #[test]
    fn identity_patch_keys_are_unique() {
        let keys = [
            MEC_ACCOUNTANT_IDENTITY.key,
            MEC_GATE_SECURITY_ACCOUNTS.key,
            MEC_CANTEEN_CAPTAIN_ACCOUNTS.key,
            MEC_LIBRARIAN_STATIONERY_ACCOUNTS.key,
            MEC_IDENTITY_RESTORE.key,
        ];
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }
}

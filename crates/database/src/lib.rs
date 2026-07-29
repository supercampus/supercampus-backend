#![forbid(unsafe_code)]

//! PostgreSQL connection and migration capability for SuperCampus services.

use std::time::Duration;

use anyhow::Context;
use sqlx::{PgPool, postgres::PgPoolOptions};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations/runtime");

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(database_url)
            .await
            .context("failed to connect to PostgreSQL")?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        MIGRATOR
            .run(&self.pool)
            .await
            .context("failed to run PostgreSQL migrations")?;
        Ok(())
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("PostgreSQL readiness query failed")?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

pub const CRATE_NAME: &str = "supercampus-database";

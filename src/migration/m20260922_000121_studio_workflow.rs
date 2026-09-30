use sea_orm::DbBackend;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// ST-D1: studio canvas creation workbench — projects (graph documents),
/// templates, runs, steps, and asset references.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The entity maps `enabled` to a Rust bool. SQLite accepts `DEFAULT 1`
        // for a BOOLEAN-affinity column; PostgreSQL requires a boolean literal.
        let enabled_default = if manager.get_database_backend() == DbBackend::Sqlite {
            "1"
        } else {
            "true"
        };
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS studio_projects (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    graph_json TEXT NOT NULL,
                    version BIGINT NOT NULL DEFAULT 1,
                    template_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "CREATE TABLE IF NOT EXISTS studio_templates (
                    id TEXT PRIMARY KEY,
                    source TEXT NOT NULL,
                    name TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    graph_json TEXT NOT NULL,
                    params_json TEXT NOT NULL,
                    price_nano_usd_map TEXT NOT NULL DEFAULT '{{}}',
                    enabled BOOLEAN NOT NULL DEFAULT {enabled_default},
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )",
            ))
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS studio_runs (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    project_id TEXT,
                    api_key_id TEXT,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    finished_at TEXT
                )",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS studio_steps (
                    id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    node_id TEXT,
                    payload_json TEXT NOT NULL,
                    result_json TEXT,
                    error TEXT,
                    attempts BIGINT NOT NULL DEFAULT 0,
                    charge_nano_usd BIGINT NOT NULL DEFAULT 0,
                    refund_nano_usd BIGINT NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    finished_at TEXT
                )",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS studio_assets (
                    id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    run_id TEXT NOT NULL,
                    step_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    upstream_kind TEXT NOT NULL,
                    provider_id TEXT,
                    remote_ref_json TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    bytes BIGINT,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL
                )",
            )
            .await?;
        for index in [
            "CREATE INDEX IF NOT EXISTS idx_studio_projects_user ON studio_projects (user_id, updated_at)",
            "CREATE INDEX IF NOT EXISTS idx_studio_runs_user ON studio_runs (user_id, created_at)",
            "CREATE INDEX IF NOT EXISTS idx_studio_steps_run ON studio_steps (run_id)",
            "CREATE INDEX IF NOT EXISTS idx_studio_steps_status ON studio_steps (status)",
            "CREATE INDEX IF NOT EXISTS idx_studio_assets_user ON studio_assets (user_id, created_at)",
        ] {
            manager.get_connection().execute_unprepared(index).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "studio_assets",
            "studio_steps",
            "studio_runs",
            "studio_templates",
            "studio_projects",
        ] {
            manager
                .get_connection()
                .execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
                .await?;
        }
        Ok(())
    }
}

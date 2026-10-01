use sea_orm::{ConnectionTrait, DbBackend, TransactionTrait};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DbBackend::Postgres {
            return Ok(());
        }
        let tx = manager.get_connection().begin().await?;
        tx.execute_unprepared("SET LOCAL lock_timeout = '5s'").await?;
        // The historical INTEGER import truncated milliseconds; the original
        // RFC3339 timestamp survives and restores the exact millisecond instant.
        tx.execute_unprepared(
            "ALTER TABLE firewall_events ALTER COLUMN created_at_unix_ms TYPE BIGINT
             USING floor(extract(epoch FROM created_at::timestamptz) * 1000)::BIGINT",
        ).await?;
        tx.commit().await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "firewall timestamp repair cannot safely narrow milliseconds to INTEGER".into(),
        ))
    }
}

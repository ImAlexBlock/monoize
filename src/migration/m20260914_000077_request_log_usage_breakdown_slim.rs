use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// RL-S13: strips the stored upstream usage echo from historical request-log rows.
///
/// `build_usage_breakdown` used to embed the whole upstream usage `extra_body` under
/// `raw_usage_extra`. Anthropic attribution maps made that member grow with conversation
/// length: on production it averaged 24 KB per row and occupied 3.29 GB of a 3.8 GB
/// database while no dashboard surface read it. New rows no longer write the member
/// (RL15c); this migration rewrites rows that still carry it.
///
/// `instr` guards the JSON parse so untouched rows stay byte-identical, which also makes
/// the migration idempotent. File compaction is a deployment-time VACUUM, not this
/// migration, because SQLite returns freed pages to the file free list.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let sql = match backend {
            // `instr` + `json_valid` guard the SQLite parse so untouched rows stay
            // byte-identical, which also makes the migration idempotent.
            DbBackend::Sqlite => {
                "UPDATE request_logs
                 SET usage_breakdown_json = json_remove(usage_breakdown_json, '$.raw_usage_extra')
                 WHERE usage_breakdown_json IS NOT NULL
                   AND json_valid(usage_breakdown_json)
                   AND instr(usage_breakdown_json, '\"raw_usage_extra\"') > 0"
            }
            // PostgreSQL has no json_valid/json_remove; cast through jsonb and
            // pre-filter malformed rows with a shape regex so the cast cannot
            // abort the statement.
            _ => {
                "UPDATE request_logs
                 SET usage_breakdown_json = (usage_breakdown_json::jsonb - 'raw_usage_extra')::text
                 WHERE usage_breakdown_json IS NOT NULL
                   AND usage_breakdown_json ~ '^[[:space:]]*\\{.*\\}[[:space:]]*$'
                   AND usage_breakdown_json::jsonb ? 'raw_usage_extra'"
            }
        };
        manager
            .get_connection()
            .execute(Statement::from_string(backend, sql.to_string()))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropped upstream echo members cannot be reconstructed.
        Ok(())
    }
}

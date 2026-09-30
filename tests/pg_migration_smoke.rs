//! PostgreSQL full-migration smoke test (database-configuration.spec.md DB-T1).
//!
//! Runs the complete embedded migration chain against an empty PostgreSQL
//! database selected by `MONOIZE_TEST_POSTGRES_DSN`. The test resets the public
//! schema first, so point it at a disposable database:
//!
//! ```sh
//! MONOIZE_TEST_POSTGRES_DSN=postgres://postgres:pw@127.0.0.1:5432/monoize \
//!   cargo test --test pg_migration_smoke -- --nocapture
//! ```

use monoize::migration::Migrator;
use sea_orm::{ConnectionTrait, Database};
use sea_orm_migration::MigratorTrait;

#[tokio::test]
async fn postgres_full_migration_from_empty_schema() {
    let dsn = std::env::var("MONOIZE_TEST_POSTGRES_DSN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .expect("set MONOIZE_TEST_POSTGRES_DSN to a disposable PostgreSQL database");

    let admin = Database::connect(&dsn).await.expect("connect admin");
    admin
        .execute_unprepared("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .await
        .expect("reset schema");
    admin.close().await.expect("close admin");

    let db = Database::connect(&dsn).await.expect("connect");
    Migrator::up(&db, None)
        .await
        .expect("full migration chain applies on PostgreSQL");
    db.close().await.expect("close");
}

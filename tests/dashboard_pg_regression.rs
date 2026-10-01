use monoize::db::DbPool;
use monoize::migration::Migrator;
use monoize::store_billing::StoreBillingStore;
use monoize::users::{aggregate_revenue_day, list_persisted_revenue_days, persist_revenue_day};
use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;

#[tokio::test]
async fn postgres_store_catalog_and_revenue_reads() {
    let dsn = std::env::var("MONOIZE_TEST_POSTGRES_DSN")
        .expect("explicit disposable PostgreSQL DSN required");
    let db = DbPool::connect(&dsn).await.expect("connect test database");
    let table_count = db.read().query_one(db.stmt(
        "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema = 'public'",
        vec![],
    )).await.unwrap().unwrap().try_get::<i64>("", "n").unwrap();
    assert_eq!(table_count, 0, "refuse a populated database");
    Migrator::up(&*db.write().await, None).await.expect("migrate test database");
    let store = StoreBillingStore::new(db.clone());
    let stamp = "2026-10-01T00:30:00Z";
    db.write().await.execute(db.stmt(
        "INSERT INTO store_payment_channels
        (id, adapter_kind, name, icon_kind, icon_value, sort_order, enabled, revision, created_at, updated_at)
        VALUES ('pg-channel', 'epay', 'PG channel', 'builtin', 'wallet', 0, 1, 1, $1, $1)",
        vec![stamp.into()],
    )).await.unwrap();
    let channels = store.list_payment_channels_admin().await
        .expect("decode INTEGER channel revision");
    assert_eq!(channels[0].revision, 1);
    let catalog = store.catalog().await;
    assert!(catalog.is_ok(), "catalog must decode INTEGER revision: {catalog:?}");
    // Missing governance evidence must omit a channel, not fail the whole catalog.
    assert!(catalog.unwrap().payment_channels.is_empty());

    let user = "pg-revenue-user";
    let group = db.read().query_one(db.stmt(
        "SELECT id FROM monoize_groups WHERE is_default = 1", vec![],
    )).await.unwrap().unwrap().try_get::<String>("", "id").unwrap();
    db.write().await.execute(db.stmt(
        "INSERT INTO users (id,username,password_hash,role,created_at,updated_at,enabled,balance_nano_usd,balance_unlimited,group_id)
        VALUES ($1,'pg_revenue','not-a-login','user',$2,$2,1,'0',0,$3)",
        vec![user.into(),stamp.into(),group.into()],
    )).await.unwrap();
    for (id, input, output) in [("pg-log-1", Some(3_000_000_000i64), Some(4_000_000_000i64)),
                                ("pg-log-2", Some(7), Some(9)),
                                ("pg-log-3", None, None)] {
        db.write().await.execute(db.stmt(
            "INSERT INTO request_logs(id,user_id,model,is_stream,input_tokens,output_tokens,charge_nano_usd,status,created_at,created_at_unix_ms)
            VALUES($1,$2,'pg-model',0,$3,$4,'10','success',$5,1790814600000)",
            vec![id.into(),user.into(),input.into(),output.into(),stamp.into()],
        )).await.unwrap();
    }
    let revenue = aggregate_revenue_day(&db, "2026-10-01", &[]).await
        .expect("decode PostgreSQL SUM(bigint)").expect("day exists");
    assert_eq!(revenue.total_input_tokens, 3_000_000_007);
    assert_eq!(revenue.total_output_tokens, 4_000_000_009);
    assert_eq!(revenue.total_calls, 3);
    assert_eq!(revenue.total_charge_nano_usd, "30");
    persist_revenue_day(&db, "2026-10-01").await.expect("persist revenue");
    let days = list_persisted_revenue_days(&db, "2026-10-01", "2026-10-01")
        .await.expect("decode persisted INT4 call counts");
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].total_calls, 3);
    assert_eq!(days[0].models[0].calls, 3);
    assert_eq!(days[0].users[0].models[0].calls, 3);
    assert_eq!(days[0].total_input_tokens, 3_000_000_007);
    assert!(aggregate_revenue_day(&db, "2026-10-02", &[]).await.unwrap().is_none());
}

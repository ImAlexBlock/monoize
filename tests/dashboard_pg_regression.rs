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
    let old_count = Migrator::migrations().len() as u32 - 1;
    Migrator::up(&*db.write().await, Some(old_count)).await.expect("migrate historical schema");
    db.write().await.execute(db.stmt(
        "INSERT INTO firewall_events(id,endpoint,model,term,content,created_at,created_at_unix_ms)
         VALUES ('overflow-event','/v1/chat/completions','test','test','synthetic',
                 '2026-09-13T16:07:24.687080798+00:00',-1685717745)",
        vec![],
    )).await.unwrap();
    Migrator::up(&*db.write().await, None).await.expect("repair historical timestamps");
    let millis = db.read().query_one(db.stmt(
        "SELECT created_at_unix_ms FROM firewall_events WHERE id='overflow-event'", vec![],
    )).await.unwrap().unwrap().try_get::<i64>("", "created_at_unix_ms").unwrap();
    assert_eq!(millis, chrono::DateTime::parse_from_rfc3339("2026-09-13T16:07:24.687080798+00:00").unwrap().timestamp_millis());
    monoize::firewall_events::compute_stats(&db).await.expect("firewall stats decode");
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
    db.write().await.execute(db.stmt(
        "INSERT INTO billing_rate_records(id,source,pricing_profile,model_pattern,rate_kind,usage_class,unit,unit_price_nano,unit_price_currency,priority,enabled,updated_at)
         VALUES ('pg-profile-test','models_dev','pg-profile','test','token','input','per_token','1','USD',0,1,$1)",
        vec![stamp.into()],
    )).await.unwrap();
    let rates = monoize::billing_rate_store::BillingRateStore::new(db.clone()).await.unwrap();
    let profiles = rates.list_pricing_profile_summaries().await.expect("profile integer flag");
    assert!(profiles.iter().any(|p| p.pricing_profile=="pg-profile" && p.has_models_dev));
    db.write().await.execute(db.stmt(
        "INSERT INTO announcements(id,title,content,type,pinned,enabled,created_at,created_by)
         VALUES ('pg-announcement','Test','Synthetic','info',0,1,$1,$2)",
        vec![stamp.into(),user.into()],
    )).await.unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(16);
    let users = monoize::users::UserStore::new(db.clone(), sender).await.unwrap();
    let unread = users.list_announcements_for_user(user, 10).await.expect("unread BOOL compatibility");
    assert_eq!(unread.announcements[0].is_read, Some(false));
    db.write().await.execute(db.stmt(
        "INSERT INTO announcement_reads(user_id,announcement_id,read_at) VALUES ($1,'pg-announcement',$2)",
        vec![user.into(),stamp.into()],
    )).await.unwrap();
    let read = users.list_announcements_for_user(user, 10).await.expect("read BOOL compatibility");
    assert_eq!(read.announcements[0].is_read, Some(true));
}

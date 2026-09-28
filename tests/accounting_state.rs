use chrono::{Duration, TimeZone, Utc};
use monoize::accounting::money::{Currency, NanoMoney};
use monoize::accounting::state::{AccountingState, ActivationError};
use monoize::db::DbPool;
use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;

async fn database() -> DbPool {
    let db = DbPool::connect("sqlite::memory:").await.unwrap();
    monoize::migration::Migrator::up(db.read(), None)
        .await
        .unwrap();
    db
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 28, 3, 0, 0).unwrap()
}

async fn rate(db: &DbPool, value: &str) {
    db.write().await.execute(db.stmt(
        "INSERT INTO store_exchange_rates (base_currency, quote_currency, cny_per_usd, source_updated_at, refreshed_at)
         VALUES ('USD', 'CNY', $1, $2, $3) ON CONFLICT (base_currency, quote_currency)
         DO UPDATE SET cny_per_usd = excluded.cny_per_usd, source_updated_at = excluded.source_updated_at, refreshed_at = excluded.refreshed_at",
        vec![value.into(), (now()-Duration::hours(3)).to_rfc3339().into(), now().to_rfc3339().into()],
    )).await.unwrap();
}

#[tokio::test]
async fn legacy_state_load_is_read_only_and_a_snapshot_does_not_pin_a_stream() {
    let db = database().await;
    let state = AccountingState::load(db.clone()).await.unwrap();
    let admitted = state.operation().await.snapshot().clone();
    assert_eq!(admitted.currency(), Currency::USD);
    rate(&db, "6.729032").await;
    let record = state
        .activate_cny("cutover-1", now(), |_, _, _| Box::pin(async { Ok(()) }))
        .await
        .unwrap();
    assert_eq!(record.cny_per_usd, "6.729032");
    assert_eq!(admitted.currency(), Currency::USD);
    let current = state.operation().await;
    assert_eq!(current.snapshot().currency(), Currency::CNY);
    assert_eq!(
        current
            .snapshot()
            .normalize(NanoMoney::new(1_000_000_000, Currency::USD, 0).unwrap())
            .unwrap()
            .amount,
        6_729_032_000
    );
}

#[tokio::test]
async fn committed_activation_retry_preserves_rate_and_does_not_repeat_transition() {
    let db = database().await;
    rate(&db, "6.729032").await;
    let state = AccountingState::load(db.clone()).await.unwrap();
    let first = state
        .activate_cny("cutover-1", now(), |_, _, _| Box::pin(async { Ok(()) }))
        .await
        .unwrap();
    rate(&db, "7").await;
    let recovered = AccountingState::load(db.clone()).await.unwrap();
    let again = recovered
        .activate_cny("cutover-1", now() + Duration::days(1), |_, _, _| {
            Box::pin(async { panic!("already committed migration must not run again") })
        })
        .await
        .unwrap();
    assert_eq!(first, again);
    assert!(matches!(
        recovered
            .activate_cny("different", now(), |_, _, _| Box::pin(async { Ok(()) }))
            .await,
        Err(ActivationError::AlreadyActivated)
    ));
}

#[tokio::test]
async fn transition_failure_rolls_back_schema_and_activation_record() {
    let db = database().await;
    rate(&db, "6.729032").await;
    let state = AccountingState::load(db.clone()).await.unwrap();
    let result = state
        .activate_cny("cutover-1", now(), |_, tx, _| {
            Box::pin(async move {
                tx.execute_unprepared("CREATE TABLE failed_activation_probe (id TEXT)")
                    .await?;
                Err(ActivationError::Transition("injected failure".into()))
            })
        })
        .await;
    assert!(result.is_err());
    assert_eq!(state.operation().await.snapshot().currency(), Currency::USD);
    let row = db
        .read()
        .query_one(db.stmt(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='failed_activation_probe'",
            vec![],
        ))
        .await
        .unwrap();
    assert!(row.is_none());
    assert_eq!(
        AccountingState::load(db)
            .await
            .unwrap()
            .operation()
            .await
            .snapshot()
            .currency(),
        Currency::USD
    );
}

#[tokio::test]
async fn activation_waits_for_an_existing_short_operation() {
    let db = database().await;
    rate(&db, "6.729032").await;
    let state = AccountingState::load(db).await.unwrap();
    let operation = state.operation().await;
    let task = tokio::spawn({
        let state = state.clone();
        async move {
            state
                .activate_cny("cutover-1", now(), |_, _, _| Box::pin(async { Ok(()) }))
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    drop(operation);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn missing_or_stale_rate_cannot_activate_accounting() {
    let db = database().await;
    let state = AccountingState::load(db.clone()).await.unwrap();
    assert!(matches!(
        state
            .activate_cny("cutover-1", now(), |_, _, _| Box::pin(async { Ok(()) }))
            .await,
        Err(ActivationError::ExchangeRateUnavailable)
    ));
    rate(&db, "6.729032").await;
    assert!(matches!(
        state
            .activate_cny("cutover-1", now() + Duration::minutes(61), |_, _, _| {
                Box::pin(async { Ok(()) })
            })
            .await,
        Err(ActivationError::ExchangeRateUnavailable)
    ));
    assert_eq!(state.operation().await.snapshot().currency(), Currency::USD);
}

#[tokio::test]
async fn nested_operations_reuse_the_gate_with_an_activation_writer_queued() {
    let db = database().await;
    rate(&db, "6.729032").await;
    let state = AccountingState::load(db.clone()).await.unwrap();
    state
        .run(async {
            let activation =
                state.activate_cny("cutover-1", now(), |_, _, _| Box::pin(async { Ok(()) }));
            // Activation from the owning operation is rejected rather than self-deadlocking.
            assert!(matches!(
                activation.await,
                Err(ActivationError::InsideOperation)
            ));
            let (started, waiting) = tokio::sync::oneshot::channel();
            let task = tokio::spawn({
                let state = state.clone();
                async move {
                    let activation = state
                        .activate_cny("cutover-1", now(), |_, _, _| Box::pin(async { Ok(()) }));
                    tokio::pin!(activation);
                    assert!(futures_util::poll!(activation.as_mut()).is_pending());
                    started.send(()).unwrap();
                    activation.await
                }
            });
            waiting.await.unwrap();
            let nested = db.accounting();
            assert_eq!(
                nested
                    .run(async { nested.operation_snapshot().unwrap().currency() })
                    .await,
                Currency::USD
            );
            assert!(!task.is_finished());
            task
        })
        .await
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db.accounting().operation().await.snapshot().currency(),
        Currency::CNY
    );
}

#[tokio::test]
async fn corrupt_persisted_state_never_falls_back_to_usd() {
    let db = database().await;
    db.write()
        .await
        .execute(db.stmt(
            "INSERT INTO state_records (tenant_id, kind, id, value, expires_at)
         VALUES ('__monoize_accounting', 'currency_epoch', 'active', '{}', NULL)",
            vec![],
        ))
        .await
        .unwrap();
    assert!(matches!(
        AccountingState::load(db).await,
        Err(ActivationError::CorruptState(_))
    ));
}

#[tokio::test]
async fn caller_cancellation_cannot_leave_a_committed_epoch_unpublished() {
    let db = database().await;
    rate(&db, "6.729032").await;
    let state = AccountingState::load(db).await.unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let caller = tokio::spawn({
        let state = state.clone();
        async move {
            state
                .activate_cny("cutover-1", now(), |_, _, _| {
                    Box::pin(async move {
                        entered.send(()).unwrap();
                        release_rx.await.unwrap();
                        Ok(())
                    })
                })
                .await
        }
    });
    entered_rx.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    assert_eq!(state.operation().await.snapshot().currency(), Currency::CNY);
}

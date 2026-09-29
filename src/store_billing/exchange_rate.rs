use super::money::parse_rate;
use crate::db::DbPool;
use async_trait::async_trait;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

/// SB-FX-1: the single CNY-per-USD rate used by every conversion. It is a fixed
/// constant, not a market quote, so a stored USD amount always projects to the
/// same CNY amount. Changing it rescales every CNY projection of existing balances.
pub const FIXED_CNY_PER_USD: &str = "6.72";
pub const EXCHANGE_RATE_REFRESH_INTERVAL: Duration = Duration::minutes(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRateSnapshot {
    pub base: String,
    pub quote: String,
    pub cny_per_usd: String,
    pub source_updated_at: DateTime<Utc>,
    pub refreshed_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum ExchangeRateError {
    #[error("invalid exchange-rate response: {0}")]
    InvalidPayload(String),
    #[error("exchange-rate request failed: {0}")]
    Request(String),
    #[error("exchange-rate storage failed: {0}")]
    Storage(String),
    #[error("no valid exchange rate is available")]
    Unavailable,
}

pub fn parse_er_api_response(
    body: &str,
    refreshed_at: DateTime<Utc>,
) -> Result<ExchangeRateSnapshot, ExchangeRateError> {
    let payload: Value = serde_json::from_str(body)
        .map_err(|error| ExchangeRateError::InvalidPayload(error.to_string()))?;
    require_string(&payload, "result", "success")?;
    require_string(&payload, "base_code", "USD")?;

    let source_timestamp = payload
        .get("time_last_update_unix")
        .and_then(Value::as_i64)
        .filter(|timestamp| *timestamp > 0)
        .ok_or_else(|| invalid_payload("time_last_update_unix must be a positive integer"))?;
    let source_updated_at = DateTime::from_timestamp(source_timestamp, 0)
        .ok_or_else(|| invalid_payload("time_last_update_unix is outside the supported range"))?;
    let rate_number = payload
        .get("rates")
        .and_then(|rates| rates.get("CNY"))
        .and_then(Value::as_number)
        .ok_or_else(|| invalid_payload("rates.CNY must be a JSON number"))?;
    let cny_per_usd = rate_number.to_string();
    parse_rate(&cny_per_usd)
        .map_err(|_| invalid_payload("rates.CNY must be a positive finite decimal"))?;

    Ok(ExchangeRateSnapshot {
        base: "USD".to_string(),
        quote: "CNY".to_string(),
        cny_per_usd,
        source_updated_at,
        refreshed_at,
    })
}

fn require_string(payload: &Value, field: &str, expected: &str) -> Result<(), ExchangeRateError> {
    if payload.get(field).and_then(Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(invalid_payload(&format!("{field} must equal {expected}")))
    }
}

fn invalid_payload(message: &str) -> ExchangeRateError {
    ExchangeRateError::InvalidPayload(message.to_string())
}

#[derive(Debug, Clone)]
pub struct ExchangeRateCache {
    snapshot: Option<ExchangeRateSnapshot>,
    last_attempt_at: Option<DateTime<Utc>>,
}

impl ExchangeRateCache {
    pub fn new(snapshot: Option<ExchangeRateSnapshot>) -> Self {
        Self {
            snapshot,
            last_attempt_at: None,
        }
    }

    pub fn snapshot(&self) -> Option<&ExchangeRateSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn should_refresh(&self, now: DateTime<Utc>) -> bool {
        self.last_attempt_at
            .is_none_or(|last_attempt| now >= last_attempt + EXCHANGE_RATE_REFRESH_INTERVAL)
    }

    pub fn record_attempt(&mut self, attempted_at: DateTime<Utc>) {
        self.last_attempt_at = Some(attempted_at);
    }

    pub fn record_success(&mut self, snapshot: ExchangeRateSnapshot) {
        self.snapshot = Some(snapshot);
    }

    pub fn record_failure(&mut self) {}
}

#[derive(Debug, Clone)]
pub struct ExchangeRateStore {
    db: DbPool,
}

impl ExchangeRateStore {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }

    pub async fn load(&self) -> Result<Option<ExchangeRateSnapshot>, ExchangeRateError> {
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(
                "SELECT base_currency, quote_currency, cny_per_usd, source_updated_at, refreshed_at
                 FROM store_exchange_rates
                 WHERE base_currency = 'USD' AND quote_currency = 'CNY'",
                vec![],
            ))
            .await
            .map_err(|error| ExchangeRateError::Storage(error.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };

        let base = row_string(&row, "base_currency")?;
        let quote = row_string(&row, "quote_currency")?;
        let cny_per_usd = row_string(&row, "cny_per_usd")?;
        parse_rate(&cny_per_usd).map_err(|_| {
            ExchangeRateError::Storage("stored exchange rate is invalid".to_string())
        })?;
        let source_updated_at = parse_stored_timestamp(&row_string(&row, "source_updated_at")?)?;
        let refreshed_at = parse_stored_timestamp(&row_string(&row, "refreshed_at")?)?;

        Ok(Some(ExchangeRateSnapshot {
            base,
            quote,
            cny_per_usd,
            source_updated_at,
            refreshed_at,
        }))
    }

    pub async fn persist(&self, snapshot: &ExchangeRateSnapshot) -> Result<(), ExchangeRateError> {
        if snapshot.base != "USD" || snapshot.quote != "CNY" {
            return Err(ExchangeRateError::Storage(
                "only the USD/CNY exchange-rate pair is supported".to_string(),
            ));
        }
        parse_rate(&snapshot.cny_per_usd)
            .map_err(|_| ExchangeRateError::Storage("exchange rate is invalid".to_string()))?;

        let write = self.db.write().await;
        write
            .execute(self.db.stmt(
                "INSERT INTO store_exchange_rates
                    (base_currency, quote_currency, cny_per_usd, source_updated_at, refreshed_at)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (base_currency, quote_currency) DO UPDATE SET
                    cny_per_usd = excluded.cny_per_usd,
                    source_updated_at = excluded.source_updated_at,
                    refreshed_at = excluded.refreshed_at",
                vec![
                    snapshot.base.clone().into(),
                    snapshot.quote.clone().into(),
                    snapshot.cny_per_usd.clone().into(),
                    timestamp_string(snapshot.source_updated_at).into(),
                    timestamp_string(snapshot.refreshed_at).into(),
                ],
            ))
            .await
            .map_err(|error| ExchangeRateError::Storage(error.to_string()))?;
        Ok(())
    }
}

fn row_string(row: &sea_orm::QueryResult, column: &str) -> Result<String, ExchangeRateError> {
    row.try_get("", column)
        .map_err(|error| ExchangeRateError::Storage(error.to_string()))
}

fn timestamp_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_stored_timestamp(value: &str) -> Result<DateTime<Utc>, ExchangeRateError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| ExchangeRateError::Storage(error.to_string()))
}

#[async_trait]
pub trait ExchangeRateFetcher: Send + Sync {
    async fn fetch_latest_usd(&self) -> Result<String, String>;
}

/// Produces the SB-FX-1 fixed rate in the validated response shape, so the fixed
/// rate passes the same parsing, persistence, and freshness path as any snapshot.
#[derive(Clone)]
struct FixedExchangeRateFetcher;

#[async_trait]
impl ExchangeRateFetcher for FixedExchangeRateFetcher {
    async fn fetch_latest_usd(&self) -> Result<String, String> {
        Ok(format!(
            r#"{{"result":"success","base_code":"USD","time_last_update_unix":{},"rates":{{"CNY":{FIXED_CNY_PER_USD}}}}}"#,
            Utc::now().timestamp()
        ))
    }
}

#[derive(Clone)]
pub struct ExchangeRateService {
    store: ExchangeRateStore,
    fetcher: Option<Arc<dyn ExchangeRateFetcher>>,
    cache: Arc<Mutex<ExchangeRateCache>>,
    read_only: bool,
}

impl ExchangeRateService {
    pub async fn new(db: DbPool) -> Result<Self, ExchangeRateError> {
        let service =
            Self::with_fetcher(ExchangeRateStore::new(db), FixedExchangeRateFetcher).await?;
        let _ = service.refresh_if_due(Utc::now()).await;
        Ok(service)
    }

    pub async fn new_read_only(db: DbPool) -> Result<Self, ExchangeRateError> {
        let store = ExchangeRateStore::new(db);
        let snapshot = store.load().await?;
        Ok(Self {
            store,
            fetcher: None,
            cache: Arc::new(Mutex::new(ExchangeRateCache::new(snapshot))),
            read_only: true,
        })
    }

    pub async fn with_fetcher<F>(
        store: ExchangeRateStore,
        fetcher: F,
    ) -> Result<Self, ExchangeRateError>
    where
        F: ExchangeRateFetcher + 'static,
    {
        Self::with_fetcher_mode(store, fetcher, false).await
    }

    async fn with_fetcher_mode<F>(
        store: ExchangeRateStore,
        fetcher: F,
        read_only: bool,
    ) -> Result<Self, ExchangeRateError>
    where
        F: ExchangeRateFetcher + 'static,
    {
        let snapshot = store.load().await?;
        Ok(Self {
            store,
            fetcher: Some(Arc::new(fetcher)),
            cache: Arc::new(Mutex::new(ExchangeRateCache::new(snapshot))),
            read_only,
        })
    }

    pub async fn current(&self) -> Result<ExchangeRateSnapshot, ExchangeRateError> {
        self.cache
            .lock()
            .await
            .snapshot()
            .cloned()
            .ok_or(ExchangeRateError::Unavailable)
    }

    pub async fn refresh_if_due(
        &self,
        attempted_at: DateTime<Utc>,
    ) -> Result<ExchangeRateSnapshot, ExchangeRateError> {
        if self.read_only {
            return self.current().await;
        }
        {
            let mut cache = self.cache.lock().await;
            if !cache.should_refresh(attempted_at) {
                return cache
                    .snapshot()
                    .cloned()
                    .ok_or(ExchangeRateError::Unavailable);
            }
            cache.record_attempt(attempted_at);
        }

        let refreshed = self
            .fetcher
            .as_ref()
            .expect("writable exchange-rate service must have a fetcher")
            .fetch_latest_usd()
            .await
            .map_err(ExchangeRateError::Request)
            .and_then(|body| parse_er_api_response(&body, attempted_at));
        let snapshot = match refreshed {
            Ok(snapshot) => {
                if let Err(error) = self.store.persist(&snapshot).await {
                    return self.last_good_or(error).await;
                }
                self.cache.lock().await.record_success(snapshot.clone());
                snapshot
            }
            Err(error) => return self.last_good_or(error).await,
        };

        Ok(snapshot)
    }

    async fn last_good_or(
        &self,
        error: ExchangeRateError,
    ) -> Result<ExchangeRateSnapshot, ExchangeRateError> {
        let mut cache = self.cache.lock().await;
        cache.record_failure();
        cache.snapshot().cloned().ok_or(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExchangeRateCache, ExchangeRateFetcher, ExchangeRateService, ExchangeRateSnapshot,
        ExchangeRateStore, parse_er_api_response,
    };
    use crate::{db::DbPool, migration::Migrator};
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use sea_orm_migration::MigratorTrait;
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    const VALID_RESPONSE: &str = r#"{
        "result":"success",
        "base_code":"USD",
        "time_last_update_unix":1787788800,
        "rates":{"CNY":6.7370}
    }"#;

    #[test]
    fn parses_a_successful_usd_response_without_binary_float_conversion() {
        let refreshed_at = Utc.with_ymd_and_hms(2026, 8, 27, 1, 2, 3).unwrap();
        let snapshot = parse_er_api_response(VALID_RESPONSE, refreshed_at).unwrap();

        assert_eq!(snapshot.base, "USD");
        assert_eq!(snapshot.quote, "CNY");
        assert_eq!(snapshot.cny_per_usd, "6.7370");
        assert_eq!(snapshot.source_updated_at.timestamp(), 1_787_788_800);
        assert_eq!(snapshot.refreshed_at, refreshed_at);
    }

    #[test]
    fn rejects_unsuccessful_wrong_base_missing_nonpositive_or_nonfinite_rates() {
        for invalid in [
            r#"{"result":"error","base_code":"USD","time_last_update_unix":1787788800,"rates":{"CNY":6.7}}"#,
            r#"{"result":"success","base_code":"EUR","time_last_update_unix":1787788800,"rates":{"CNY":6.7}}"#,
            r#"{"result":"success","base_code":"USD","time_last_update_unix":1787788800,"rates":{}}"#,
            r#"{"result":"success","base_code":"USD","time_last_update_unix":1787788800,"rates":{"CNY":0}}"#,
            r#"{"result":"success","base_code":"USD","time_last_update_unix":1787788800,"rates":{"CNY":"NaN"}}"#,
        ] {
            assert!(
                parse_er_api_response(invalid, Utc::now()).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn failed_refresh_keeps_the_last_good_snapshot() {
        let refreshed_at = Utc.with_ymd_and_hms(2026, 8, 27, 1, 2, 3).unwrap();
        let snapshot = parse_er_api_response(VALID_RESPONSE, refreshed_at).unwrap();
        let mut cache = ExchangeRateCache::new(Some(snapshot.clone()));

        cache.record_attempt(refreshed_at);
        cache.record_success(snapshot.clone());
        cache.record_attempt(refreshed_at + chrono::Duration::minutes(15));
        cache.record_failure();

        assert_eq!(cache.snapshot(), Some(&snapshot));
    }

    #[derive(Clone)]
    struct StubFetcher {
        calls: Arc<AtomicUsize>,
        responses: Arc<Mutex<VecDeque<Result<String, String>>>>,
    }

    impl StubFetcher {
        fn new(responses: impl IntoIterator<Item = Result<&'static str, &'static str>>) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                responses: Arc::new(Mutex::new(
                    responses
                        .into_iter()
                        .map(|result| result.map(str::to_owned).map_err(str::to_owned))
                        .collect(),
                )),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ExchangeRateFetcher for StubFetcher {
        async fn fetch_latest_usd(&self) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("no stub response".to_string()))
        }
    }

    async fn migrated_store() -> ExchangeRateStore {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        {
            let write = db.write().await;
            Migrator::up(&*write, None).await.unwrap();
        }
        ExchangeRateStore::new(db)
    }

    fn snapshot(refreshed_at: chrono::DateTime<Utc>) -> ExchangeRateSnapshot {
        parse_er_api_response(VALID_RESPONSE, refreshed_at).unwrap()
    }

    #[tokio::test]
    async fn writable_service_always_reports_the_fixed_rate() {
        let store = migrated_store().await;
        // A previously persisted market rate is replaced by the fixed rate at startup.
        store
            .persist(&snapshot(Utc.with_ymd_and_hms(2026, 8, 27, 1, 2, 3).unwrap()))
            .await
            .unwrap();
        let service = ExchangeRateService::with_fetcher(store.clone(), super::FixedExchangeRateFetcher)
            .await
            .unwrap();
        let now = Utc::now();
        let first = service.refresh_if_due(now).await.unwrap();
        let later = service
            .refresh_if_due(now + chrono::Duration::minutes(15))
            .await
            .unwrap();

        assert_eq!(first.cny_per_usd, super::FIXED_CNY_PER_USD);
        assert_eq!(later.cny_per_usd, super::FIXED_CNY_PER_USD);
        assert_eq!(store.load().await.unwrap().unwrap().cny_per_usd, super::FIXED_CNY_PER_USD);
    }

    #[tokio::test]
    async fn store_persists_and_loads_the_exact_snapshot() {
        let store = migrated_store().await;
        let expected = snapshot(Utc.with_ymd_and_hms(2026, 8, 27, 1, 2, 3).unwrap());

        store.persist(&expected).await.unwrap();

        assert_eq!(store.load().await.unwrap(), Some(expected));
    }

    #[tokio::test]
    async fn service_attempts_at_most_once_per_fifteen_minutes() {
        let store = migrated_store().await;
        let fetcher = StubFetcher::new([Ok(VALID_RESPONSE), Ok(VALID_RESPONSE)]);
        let service = ExchangeRateService::with_fetcher(store, fetcher.clone())
            .await
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 27, 1, 2, 3).unwrap();

        service.refresh_if_due(now).await.unwrap();
        service
            .refresh_if_due(now + chrono::Duration::minutes(14))
            .await
            .unwrap();
        assert_eq!(fetcher.call_count(), 1);

        service
            .refresh_if_due(now + chrono::Duration::minutes(15))
            .await
            .unwrap();
        assert_eq!(fetcher.call_count(), 2);
    }

    #[tokio::test]
    async fn service_returns_and_persists_the_last_good_rate_after_failure() {
        let store = migrated_store().await;
        let now = Utc.with_ymd_and_hms(2026, 8, 27, 1, 2, 3).unwrap();
        let expected = snapshot(now);
        store.persist(&expected).await.unwrap();
        let fetcher = StubFetcher::new([Err("offline")]);
        let service = ExchangeRateService::with_fetcher(store.clone(), fetcher)
            .await
            .unwrap();

        let returned = service.refresh_if_due(now).await.unwrap();

        assert_eq!(returned, expected);
        assert_eq!(store.load().await.unwrap(), Some(expected));
    }

    #[tokio::test]
    async fn read_only_service_never_fetches_or_persists() {
        let store = migrated_store().await;
        let now = Utc.with_ymd_and_hms(2026, 8, 27, 1, 2, 3).unwrap();
        let expected = snapshot(now);
        store.persist(&expected).await.unwrap();
        let fetcher = StubFetcher::new([Ok(VALID_RESPONSE)]);
        let service = ExchangeRateService::with_fetcher_mode(store.clone(), fetcher.clone(), true)
            .await
            .unwrap();

        let returned = service
            .refresh_if_due(now + chrono::Duration::hours(1))
            .await
            .unwrap();

        assert_eq!(returned, expected);
        assert_eq!(fetcher.call_count(), 0);
        assert_eq!(store.load().await.unwrap(), Some(expected));
    }
}

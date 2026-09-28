use super::money::{Currency, ExchangeRateRational, MoneyError, NanoMoney};
use crate::db::DbPool;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

#[derive(Debug, thiserror::Error)]
pub enum ActivationError {
    #[error("accounting storage error: {0}")]
    Storage(#[from] DbErr),
    #[error("stored accounting activation is invalid: {0}")]
    CorruptState(String),
    #[error("accounting activation requires a valid current exchange-rate snapshot")]
    ExchangeRateUnavailable,
    #[error("accounting has already been activated with a different migration ID")]
    AlreadyActivated,
    #[error("migration ID must contain 1 to 128 printable non-space ASCII characters")]
    InvalidMigrationId,
    #[error("online accounting activation currently requires a single SQLite Primary")]
    UnsupportedBackend,
    #[error("accounting activation cannot run inside a monetary operation")]
    InsideOperation,
    #[error("accounting activation task failed: {0}")]
    TaskFailed(String),
    #[error("accounting transition failed: {0}")]
    Transition(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationRecord {
    pub schema_version: u8,
    pub migration_id: String,
    pub epoch: u64,
    pub currency: Currency,
    pub cny_per_usd: String,
    pub rate_numerator: String,
    pub rate_denominator: String,
    pub source_updated_at: DateTime<Utc>,
    pub refreshed_at: DateTime<Utc>,
    pub activated_at: DateTime<Utc>,
}

impl ActivationRecord {
    fn validate(&self) -> Result<ExchangeRateRational, ActivationError> {
        validate_id(&self.migration_id)?;
        let rate = ExchangeRateRational::parse(&self.cny_per_usd)
            .map_err(|error| ActivationError::CorruptState(error.to_string()))?;
        if self.schema_version != 1
            || self.epoch != 1
            || self.currency != Currency::CNY
            || self.rate_numerator != rate.numerator().to_string()
            || self.rate_denominator != rate.denominator().to_string()
            || !rate_and_times_valid(
                &rate,
                self.source_updated_at,
                self.refreshed_at,
                self.activated_at,
            )
        {
            return Err(ActivationError::CorruptState(
                "unsupported version, denomination, or exchange-rate evidence".into(),
            ));
        }
        Ok(rate)
    }
}

#[derive(Debug, Clone)]
pub struct AccountingSnapshot {
    activation: Option<Arc<ActivationRecord>>,
    rate: Option<ExchangeRateRational>,
}

impl AccountingSnapshot {
    fn from_record(record: Option<ActivationRecord>) -> Result<Self, ActivationError> {
        let rate = record
            .as_ref()
            .map(ActivationRecord::validate)
            .transpose()?;
        Ok(Self {
            activation: record.map(Arc::new),
            rate,
        })
    }

    pub fn epoch(&self) -> u64 {
        self.activation.as_ref().map_or(0, |record| record.epoch)
    }

    pub fn currency(&self) -> Currency {
        self.activation
            .as_ref()
            .map_or(Currency::USD, |record| record.currency)
    }

    pub fn activation(&self) -> Option<&ActivationRecord> {
        self.activation.as_deref()
    }

    /// Normalize a frozen accounting amount without consulting the live FX service.
    pub fn normalize(&self, amount: NanoMoney) -> Result<NanoMoney, MoneyError> {
        amount.validate()?;
        if amount.epoch == self.epoch() {
            return Ok(amount);
        }
        let Some(rate) = self.rate.as_ref() else {
            return Err(MoneyError::IncompatibleAmounts);
        };
        amount.convert_to(Currency::CNY, 1, rate)
    }
}

#[derive(Clone)]
pub struct AccountingOperation {
    guard: Arc<OwnedRwLockReadGuard<AccountingSnapshot>>,
}

impl AccountingOperation {
    pub fn snapshot(&self) -> &AccountingSnapshot {
        &self.guard
    }
}

#[derive(Clone)]
pub struct AccountingState {
    db: DbPool,
    core: Arc<AccountingCore>,
}

#[derive(Debug)]
pub(crate) struct AccountingCore {
    current: Arc<RwLock<AccountingSnapshot>>,
}

impl AccountingCore {
    pub(crate) fn legacy() -> Self {
        Self {
            current: Arc::new(RwLock::new(AccountingSnapshot {
                activation: None,
                rate: None,
            })),
        }
    }

    /// Resolve the SQL layout for column-name rendering without awaiting. Returns
    /// `None` while an activation writer holds the lock; callers keep the legacy
    /// layout because activation is barred inside monetary operations.
    pub(crate) fn layout(&self) -> Option<crate::accounting::schema::SchemaLayout> {
        let current = self.current.try_read().ok()?;
        Some(match current.currency() {
            crate::accounting::money::Currency::CNY => {
                crate::accounting::schema::SchemaLayout::CurrencyEpoch
            }
            crate::accounting::money::Currency::USD => {
                crate::accounting::schema::SchemaLayout::LegacyUsd
            }
        })
    }
}

struct OperationContext {
    core: Arc<AccountingCore>,
    operation: AccountingOperation,
}

tokio::task_local! {
    static OPERATION: OperationContext;
}

impl AccountingState {
    /// Load before constructing stores or replaying durable financial operations.
    pub async fn load(db: DbPool) -> Result<Self, ActivationError> {
        let state = Self::for_pool(db);
        {
            let mut current = state.core.current.write().await;
            let record = read_record(&state.db, state.db.read()).await?;
            *current = AccountingSnapshot::from_record(record)?;
        }
        Ok(state)
    }

    pub(crate) fn for_pool(db: DbPool) -> Self {
        let core = db.accounting_core();
        Self { db, core }
    }

    /// Reuse this operation in nested storage helpers; clone only its snapshot for a stream.
    pub async fn operation(&self) -> AccountingOperation {
        if let Ok(Some(operation)) = OPERATION.try_with(|context| {
            Arc::ptr_eq(&context.core, &self.core).then(|| context.operation.clone())
        }) {
            return operation;
        }
        AccountingOperation {
            guard: Arc::new(self.core.current.clone().read_owned().await),
        }
    }

    /// Nested storage calls reuse the outer gate even when an activation writer is queued.
    pub async fn run<T>(&self, future: impl Future<Output = T>) -> T {
        if OPERATION
            .try_with(|context| Arc::ptr_eq(&context.core, &self.core))
            .unwrap_or(false)
        {
            return future.await;
        }
        let operation = self.operation().await;
        OPERATION
            .scope(
                OperationContext {
                    core: self.core.clone(),
                    operation,
                },
                future,
            )
            .await
    }

    pub fn operation_snapshot(&self) -> Option<AccountingSnapshot> {
        OPERATION
            .try_with(|context| {
                Arc::ptr_eq(&context.core, &self.core).then(|| context.operation.snapshot().clone())
            })
            .ok()
            .flatten()
    }

    /// Commit the caller's guarded schema transition and the immutable epoch as one unit.
    /// Deployment authentication and writer-barrier verification belong to the caller.
    pub async fn activate_cny<F>(
        &self,
        migration_id: &str,
        activated_at: DateTime<Utc>,
        transition: F,
    ) -> Result<ActivationRecord, ActivationError>
    where
        F: for<'a> FnOnce(
                &'a DbPool,
                &'a DatabaseTransaction,
                &'a ActivationRecord,
            )
                -> Pin<Box<dyn Future<Output = Result<(), ActivationError>> + Send + 'a>>
            + Send
            + 'static,
    {
        validate_id(migration_id)?;
        if self.operation_snapshot().is_some() {
            return Err(ActivationError::InsideOperation);
        }
        if !self.db.is_sqlite() {
            return Err(ActivationError::UnsupportedBackend);
        }
        let state = self.clone();
        let migration_id = migration_id.to_owned();
        tokio::spawn(async move {
            state
                .activate_cny_inner(migration_id, activated_at, transition)
                .await
        })
        .await
        .map_err(|error| ActivationError::TaskFailed(error.to_string()))?
    }

    async fn activate_cny_inner<F>(
        &self,
        migration_id: String,
        activated_at: DateTime<Utc>,
        transition: F,
    ) -> Result<ActivationRecord, ActivationError>
    where
        F: for<'a> FnOnce(
                &'a DbPool,
                &'a DatabaseTransaction,
                &'a ActivationRecord,
            )
                -> Pin<Box<dyn Future<Output = Result<(), ActivationError>> + Send + 'a>>
            + Send
            + 'static,
    {
        // Dropping an HTTP caller must not cancel between database commit and
        // publishing the new layout. The owned activation task completes both.
        let mut current = self.core.current.write().await;
        let db = self.db.clone();
        let record = self
            .db
            .with_immediate_write(move |tx| {
                Box::pin(async move {
                    if let Some(record) = read_record(&db, tx).await? {
                        if record.migration_id != migration_id {
                            return Err(ActivationError::AlreadyActivated);
                        }
                        return Ok(record);
                    }
                    let record =
                        activation_from_current_rate(&db, tx, migration_id, activated_at).await?;
                    transition(&db, tx, &record).await?;
                    let value = serde_json::to_string(&record)
                        .map_err(|error| ActivationError::CorruptState(error.to_string()))?;
                    tx.execute(db.stmt(
                        "INSERT INTO state_records (tenant_id, kind, id, value, expires_at)
                 VALUES ('__monoize_accounting', 'currency_epoch', 'active', $1, NULL)",
                        vec![value.into()],
                    ))
                    .await?;
                    Ok(record)
                })
            })
            .await?;
        // Publish only a committed record. A lost response or process restart recovers
        // this exact record instead of repeating the conversion with a newer rate.
        *current = AccountingSnapshot::from_record(Some(record.clone()))?;
        Ok(record)
    }
}

fn validate_id(id: &str) -> Result<(), ActivationError> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(ActivationError::InvalidMigrationId);
    }
    Ok(())
}

async fn read_record<C: ConnectionTrait>(
    db: &DbPool,
    connection: &C,
) -> Result<Option<ActivationRecord>, ActivationError> {
    let row = connection
        .query_one(db.stmt(
            "SELECT value, expires_at FROM state_records
         WHERE tenant_id = '__monoize_accounting' AND kind = 'currency_epoch' AND id = 'active'",
            vec![],
        ))
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.try_get::<Option<i64>>("", "expires_at")?.is_some() {
        return Err(ActivationError::CorruptState(
            "activation must not expire".into(),
        ));
    }
    let value: String = row.try_get("", "value")?;
    let record: ActivationRecord = serde_json::from_str(&value)
        .map_err(|error| ActivationError::CorruptState(error.to_string()))?;
    record.validate()?;
    Ok(Some(record))
}

async fn activation_from_current_rate<C: ConnectionTrait>(
    db: &DbPool,
    connection: &C,
    migration_id: String,
    activated_at: DateTime<Utc>,
) -> Result<ActivationRecord, ActivationError> {
    let row = connection
        .query_one(db.stmt(
            "SELECT cny_per_usd, source_updated_at, refreshed_at FROM store_exchange_rates
         WHERE base_currency = 'USD' AND quote_currency = 'CNY'",
            vec![],
        ))
        .await?
        .ok_or(ActivationError::ExchangeRateUnavailable)?;
    let cny_per_usd: String = row.try_get("", "cny_per_usd")?;
    let rate = ExchangeRateRational::parse(&cny_per_usd)
        .map_err(|_| ActivationError::ExchangeRateUnavailable)?;
    let parse_time = |field| -> Result<DateTime<Utc>, ActivationError> {
        let raw: String = row.try_get("", field)?;
        DateTime::parse_from_rfc3339(&raw)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|_| ActivationError::ExchangeRateUnavailable)
    };
    let source_updated_at = parse_time("source_updated_at")?;
    let refreshed_at = parse_time("refreshed_at")?;
    if !rate_and_times_valid(&rate, source_updated_at, refreshed_at, activated_at) {
        return Err(ActivationError::ExchangeRateUnavailable);
    }
    Ok(ActivationRecord {
        schema_version: 1,
        migration_id,
        epoch: 1,
        currency: Currency::CNY,
        cny_per_usd,
        rate_numerator: rate.numerator().to_string(),
        rate_denominator: rate.denominator().to_string(),
        source_updated_at,
        refreshed_at,
        activated_at,
    })
}

fn rate_and_times_valid(
    rate: &ExchangeRateRational,
    source: DateTime<Utc>,
    refreshed: DateTime<Utc>,
    at: DateTime<Utc>,
) -> bool {
    rate.numerator() >= rate.denominator()
        && rate
            .denominator()
            .checked_mul(20)
            .is_some_and(|max| rate.numerator() <= max)
        && source <= refreshed + Duration::minutes(5)
        && source >= refreshed - Duration::hours(48)
        && source <= at + Duration::minutes(5)
        && source >= at - Duration::hours(48)
        && refreshed <= at + Duration::minutes(5)
        && refreshed >= at - Duration::minutes(60)
}

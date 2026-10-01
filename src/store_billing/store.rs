use super::{
    crypto::{EncryptedSecret, PaymentKeyRing},
    exchange_rate::ExchangeRateSnapshot,
    governance::{evaluate_channel, evaluate_channels},
    models::*,
    money::{
        Currency, MoneyError, convert_minor, parse_minor, parse_signed_minor,
        quoted_received_to_nano_usd,
    },
    quota::{EntitlementGenerationInput, QuotaError, replace_entitlement_tx},
    quota_gate::QuotaGateStore,
    redemption::{
        RedemptionAuditContext, RevealRedemptionInput, RevealedRedemptionCode, code_digest,
        decrypt_code, generate_code_material, normalize_code, source_ip_digest,
        validate_audit_context, validate_reveal_input,
    },
};
use crate::db::DbPool;
use chrono::{DateTime, Duration, SecondsFormat, Timelike, Utc};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use sea_orm::{ConnectionTrait, QueryResult};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const STORE_SETTING_KEYS: [&str; 4] = [
    "store.custom_recharge_cny_min_minor",
    "store.custom_recharge_cny_max_minor",
    "store.custom_recharge_usd_min_minor",
    "store.custom_recharge_usd_max_minor",
];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreBillingError {
    #[error("invalid Store input")]
    InvalidInput,
    #[error("invalid monetary amount")]
    InvalidAmount,
    #[error("monetary amount overflow")]
    AmountOverflow,
    #[error("invalid exchange rate")]
    InvalidExchangeRate,
    #[error("product is not available")]
    ProductNotAvailable,
    #[error("payment channel is invalid")]
    InvalidPaymentChannel,
    #[error("payment channel icon is invalid")]
    InvalidIcon,
    #[error("redemption code is invalid")]
    InvalidRedemptionCode,
    #[error("redemption code is expired")]
    RedemptionCodeExpired,
    #[error("redemption code is used")]
    RedemptionCodeUsed,
    #[error("redemption code is revoked")]
    RedemptionCodeRevoked,
    #[error("redemption access requires configured encryption keys")]
    EncryptionUnavailable,
    #[error("redemption attempt rate limit exceeded")]
    RedemptionRateLimited,
    #[error("redemption attempt cooldown is active")]
    RedemptionCooldown,
    #[error("payment hold blocks Store mutations")]
    PaymentHold,
    #[error("Store storage failed: {0}")]
    Storage(String),
    #[error("Store record was not found")]
    NotFound,
    #[error("Store record is in use")]
    Conflict,
    #[error("Store writes require the Primary repository")]
    WriteRejected,
}

impl From<MoneyError> for StoreBillingError {
    fn from(error: MoneyError) -> Self {
        match error {
            MoneyError::InvalidExchangeRate => Self::InvalidExchangeRate,
            MoneyError::InvalidAmount => Self::InvalidAmount,
            MoneyError::AmountOverflow => Self::AmountOverflow,
        }
    }
}

impl From<QuotaError> for StoreBillingError {
    fn from(error: QuotaError) -> Self {
        match error.code() {
            "plan_requires_postgres" => Self::ProductNotAvailable,
            "entitlement_generation_conflict" | "entitlement_source_conflict" => Self::Conflict,
            "invalid_entitlement" | "invalid_quota_window" => Self::InvalidInput,
            _ => Self::Storage(error.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoreBillingStore {
    db: DbPool,
    read_only: bool,
}

impl StoreBillingStore {
    pub fn new(db: DbPool) -> Self {
        Self {
            db,
            read_only: false,
        }
    }

    pub fn new_read_only(db: DbPool) -> Self {
        Self {
            db,
            read_only: true,
        }
    }

    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn require_write(&self) -> Result<(), StoreBillingError> {
        if self.read_only {
            Err(StoreBillingError::WriteRejected)
        } else {
            Ok(())
        }
    }

    pub async fn get_settings(&self) -> Result<StoreSettings, StoreBillingError> {
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                "SELECT key, value FROM system_settings
                 WHERE key IN ($1, $2, $3, $4)",
                STORE_SETTING_KEYS.iter().map(|key| (*key).into()).collect(),
            ))
            .await
            .map_err(storage)?;
        let mut settings = StoreSettings::default();
        for row in rows {
            let key = row_string(&row, "key")?;
            let value = row_string(&row, "value")?;
            match key.as_str() {
                "store.custom_recharge_cny_min_minor" => {
                    settings.custom_recharge_cny_min_minor = value
                }
                "store.custom_recharge_cny_max_minor" => {
                    settings.custom_recharge_cny_max_minor = value
                }
                "store.custom_recharge_usd_min_minor" => {
                    settings.custom_recharge_usd_min_minor = value
                }
                "store.custom_recharge_usd_max_minor" => {
                    settings.custom_recharge_usd_max_minor = value
                }
                _ => {}
            }
        }
        validate_settings(&settings)?;
        Ok(settings)
    }

    pub async fn update_settings(
        &self,
        settings: StoreSettings,
    ) -> Result<StoreSettings, StoreBillingError> {
        self.require_write()?;
        validate_settings(&settings)?;
        let now = timestamp(Utc::now());
        let tx = self.db.begin_write().await.map_err(storage)?;
        for (key, value) in [
            (
                STORE_SETTING_KEYS[0],
                settings.custom_recharge_cny_min_minor.as_str(),
            ),
            (
                STORE_SETTING_KEYS[1],
                settings.custom_recharge_cny_max_minor.as_str(),
            ),
            (
                STORE_SETTING_KEYS[2],
                settings.custom_recharge_usd_min_minor.as_str(),
            ),
            (
                STORE_SETTING_KEYS[3],
                settings.custom_recharge_usd_max_minor.as_str(),
            ),
        ] {
            tx.execute(self.db.stmt(
                "INSERT INTO system_settings (key, value, updated_at) VALUES ($1, $2, $3)
                 ON CONFLICT (key) DO UPDATE SET
                    value = excluded.value, updated_at = excluded.updated_at",
                vec![key.into(), value.into(), now.clone().into()],
            ))
            .await
            .map_err(storage)?;
        }
        tx.commit().await.map_err(storage)?;
        Ok(settings)
    }

    pub async fn create_product(
        &self,
        input: CreateProductInput,
    ) -> Result<StoreProduct, StoreBillingError> {
        self.require_write()?;
        let mut input = input;
        input.group_ids = canonical_group_ids(&input.group_ids)?;
        validate_product(&input)?;
        if input.kind == ProductKind::Plan
            && input.enabled
            && !QuotaGateStore::new(self.db.clone())
                .plan_features_enabled()
                .await
                .map_err(storage)?
        {
            return Err(StoreBillingError::ProductNotAvailable);
        }
        let id = Uuid::new_v4().to_string();
        let now = timestamp(Utc::now());
        let group_ids = to_json(&input.group_ids)?;
        let tx = self.db.begin_write().await.map_err(storage)?;
        self.validate_group_ids(&*tx, &input.group_ids).await?;
        tx.execute(self.db.stmt(
            "INSERT INTO store_products
                (id, kind, name, description, price_currency, price_minor, duration_seconds,
                 group_ids, sort_order, enabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
            vec![
                id.clone().into(),
                input.kind.as_str().into(),
                input.name.trim().to_string().into(),
                input.description.trim().to_string().into(),
                currency_string(input.price_currency).into(),
                input.price_minor.clone().into(),
                input.duration_seconds.into(),
                group_ids.into(),
                input.sort_order.into(),
                i32::from(input.enabled).into(),
                now.into(),
            ],
        ))
        .await
        .map_err(storage)?;
        self.insert_product_details(&*tx, &id, &input).await?;
        tx.commit().await.map_err(storage)?;
        self.product_by_id(&id, false)
            .await?
            .ok_or(StoreBillingError::ProductNotAvailable)
    }

    pub async fn update_product(
        &self,
        id: &str,
        input: CreateProductInput,
    ) -> Result<StoreProduct, StoreBillingError> {
        self.require_write()?;
        let mut input = input;
        input.group_ids = canonical_group_ids(&input.group_ids)?;
        validate_product(&input)?;
        if input.kind == ProductKind::Plan
            && input.enabled
            && !QuotaGateStore::new(self.db.clone())
                .plan_features_enabled()
                .await
                .map_err(storage)?
        {
            return Err(StoreBillingError::ProductNotAvailable);
        }
        let group_ids = to_json(&input.group_ids)?;
        let tx = self.db.begin_write().await.map_err(storage)?;
        self.validate_group_ids(&*tx, &input.group_ids).await?;
        let result = tx
            .execute(self.db.stmt(
                "UPDATE store_products SET
                    kind = $2, name = $3, description = $4, price_currency = $5,
                    price_minor = $6, duration_seconds = $7, group_ids = $8,
                    sort_order = $9, enabled = $10, updated_at = $11
                 WHERE id = $1",
                vec![
                    id.into(),
                    input.kind.as_str().into(),
                    input.name.trim().to_string().into(),
                    input.description.trim().to_string().into(),
                    currency_string(input.price_currency).into(),
                    input.price_minor.clone().into(),
                    input.duration_seconds.into(),
                    group_ids.into(),
                    input.sort_order.into(),
                    i32::from(input.enabled).into(),
                    timestamp(Utc::now()).into(),
                ],
            ))
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            tx.rollback().await.map_err(storage)?;
            return Err(StoreBillingError::ProductNotAvailable);
        }
        tx.execute(self.db.stmt(
            "DELETE FROM store_balance_products WHERE product_id = $1",
            vec![id.into()],
        ))
        .await
        .map_err(storage)?;
        tx.execute(self.db.stmt(
            "DELETE FROM store_plan_quotas WHERE product_id = $1",
            vec![id.into()],
        ))
        .await
        .map_err(storage)?;
        self.insert_product_details(&*tx, id, &input).await?;
        tx.commit().await.map_err(storage)?;
        self.product_by_id(id, false)
            .await?
            .ok_or(StoreBillingError::ProductNotAvailable)
    }

    async fn insert_product_details<C: ConnectionTrait>(
        &self,
        conn: &C,
        product_id: &str,
        input: &CreateProductInput,
    ) -> Result<(), StoreBillingError> {
        match input.kind {
            ProductKind::Balance => {
                let balance = input.balance.as_ref().expect("validated balance product");
                conn.execute(self.db.stmt(
                    "INSERT INTO store_balance_products
                        (product_id, recharge_minor, bonus_minor) VALUES ($1, $2, $3)",
                    vec![
                        product_id.into(),
                        balance.recharge_minor.clone().into(),
                        balance.bonus_minor.clone().into(),
                    ],
                ))
                .await
                .map_err(storage)?;
            }
            ProductKind::Plan => {
                for quota in &input.quotas {
                    conn.execute(self.db.stmt(
                        "INSERT INTO store_plan_quotas
                            (id, product_id, window_kind, window_seconds, quota_fen_cny, sort_order)
                         VALUES ($1, $2, $3, $4, $5, $6)",
                        vec![
                            Uuid::new_v4().to_string().into(),
                            product_id.into(),
                            quota.window_kind.as_str().into(),
                            quota.window_seconds.into(),
                            quota.quota_fen_cny.clone().into(),
                            quota.sort_order.into(),
                        ],
                    ))
                    .await
                    .map_err(storage)?;
                }
            }
        }
        Ok(())
    }

    async fn validate_group_ids<C: ConnectionTrait>(
        &self,
        conn: &C,
        group_ids: &[String],
    ) -> Result<(), StoreBillingError> {
        for group_id in group_ids {
            if conn
                .query_one(self.db.stmt(
                    "SELECT 1 AS present FROM monoize_groups WHERE id = $1",
                    vec![group_id.clone().into()],
                ))
                .await
                .map_err(storage)?
                .is_none()
            {
                return Err(StoreBillingError::InvalidInput);
            }
        }
        Ok(())
    }

    async fn product_by_id(
        &self,
        id: &str,
        enabled_only: bool,
    ) -> Result<Option<StoreProduct>, StoreBillingError> {
        let suffix = if enabled_only { " AND enabled = 1" } else { "" };
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(
                &format!(
                    "SELECT id, kind, name, description, price_currency, price_minor,
                            duration_seconds, group_ids, sort_order, enabled, created_at, updated_at
                     FROM store_products WHERE id = $1{suffix}"
                ),
                vec![id.into()],
            ))
            .await
            .map_err(storage)?;
        match row {
            Some(row) => self.product_from_row(self.db.read(), row).await.map(Some),
            None => Ok(None),
        }
    }

    async fn product_from_row<C: ConnectionTrait>(
        &self,
        conn: &C,
        row: QueryResult,
    ) -> Result<StoreProduct, StoreBillingError> {
        let id = row_string(&row, "id")?;
        let kind = ProductKind::from_str(&row_string(&row, "kind")?)
            .ok_or_else(|| storage("stored product kind is invalid"))?;
        let balance = if kind == ProductKind::Balance {
            let row = conn
                .query_one(self.db.stmt(
                    "SELECT recharge_minor, bonus_minor FROM store_balance_products
                     WHERE product_id = $1",
                    vec![id.clone().into()],
                ))
                .await
                .map_err(storage)?
                .ok_or_else(|| storage("balance product details are missing"))?;
            let input = BalanceProductInput {
                recharge_minor: row_string(&row, "recharge_minor")?,
                bonus_minor: row_string(&row, "bonus_minor")?,
            };
            Some(BalanceProduct {
                actual_received_minor: actual_received(&input)?,
                recharge_minor: input.recharge_minor,
                bonus_minor: input.bonus_minor,
            })
        } else {
            None
        };
        let quotas = if kind == ProductKind::Plan {
            conn.query_all(self.db.stmt(
                "SELECT id, window_kind, window_seconds, quota_fen_cny, sort_order
                 FROM store_plan_quotas WHERE product_id = $1
                 ORDER BY sort_order ASC, id ASC",
                vec![id.clone().into()],
            ))
            .await
            .map_err(storage)?
            .into_iter()
            .map(plan_quota_from_row)
            .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };

        Ok(StoreProduct {
            id,
            kind,
            name: row_string(&row, "name")?,
            description: row_string(&row, "description")?,
            price_currency: parse_currency(&row_string(&row, "price_currency")?)?,
            price_minor: row_string(&row, "price_minor")?,
            duration_seconds: row.try_get("", "duration_seconds").map_err(storage)?,
            group_ids: parse_json(&row_string(&row, "group_ids")?)?,
            sort_order: row.try_get("", "sort_order").map_err(storage)?,
            enabled: row_i32(&row, "enabled")? != 0,
            created_at: parse_timestamp(&row_string(&row, "created_at")?)?,
            updated_at: parse_timestamp(&row_string(&row, "updated_at")?)?,
            balance,
            quotas,
        })
    }

    pub async fn create_payment_channel(
        &self,
        input: CreatePaymentChannelInput,
    ) -> Result<PaymentChannel, StoreBillingError> {
        self.require_write()?;
        validate_payment_channel(&input.name, input.icon_kind, input.icon_value.as_deref())?;
        let id = Uuid::new_v4().to_string();
        let now = timestamp(Utc::now());
        let write = self.db.write().await;
        write
            .execute(self.db.stmt(
                "INSERT INTO store_payment_channels
                    (id, adapter_kind, name, icon_kind, icon_value, sort_order, enabled,
                     revision, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $8)",
                vec![
                    id.clone().into(),
                    input.adapter_kind.as_str().into(),
                    input.name.trim().to_string().into(),
                    input.icon_kind.as_str().into(),
                    input.icon_value.into(),
                    input.sort_order.into(),
                    i32::from(input.enabled).into(),
                    now.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        drop(write);
        self.payment_channel_by_id(&id, false)
            .await?
            .ok_or(StoreBillingError::InvalidPaymentChannel)
    }

    pub async fn save_payment_icon(
        &self,
        content: Vec<u8>,
    ) -> Result<StorePaymentIcon, StoreBillingError> {
        self.require_write()?;
        let content_type = validate_payment_icon(&content)?.to_string();
        let icon = StorePaymentIcon {
            id: Uuid::new_v4().to_string(),
            content_type,
            content,
            created_at: Utc::now()
                .with_nanosecond(0)
                .ok_or_else(|| storage("failed to normalize payment icon timestamp"))?,
        };
        self.db
            .write()
            .await
            .execute(self.db.stmt(
                "INSERT INTO store_payment_icons (id, content_type, content, created_at)
                 VALUES ($1, $2, $3, $4)",
                vec![
                    icon.id.clone().into(),
                    icon.content_type.clone().into(),
                    icon.content.clone().into(),
                    timestamp(icon.created_at).into(),
                ],
            ))
            .await
            .map_err(storage)?;
        Ok(icon)
    }

    pub async fn get_payment_icon(
        &self,
        id: &str,
    ) -> Result<Option<StorePaymentIcon>, StoreBillingError> {
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(
                "SELECT id, content_type, content, created_at
                 FROM store_payment_icons WHERE id = $1",
                vec![id.into()],
            ))
            .await
            .map_err(storage)?;
        row.map(|row| {
            Ok(StorePaymentIcon {
                id: row_string(&row, "id")?,
                content_type: row_string(&row, "content_type")?,
                content: row.try_get("", "content").map_err(storage)?,
                created_at: parse_timestamp(&row_string(&row, "created_at")?)?,
            })
        })
        .transpose()
    }

    pub async fn update_payment_channel(
        &self,
        id: &str,
        input: UpdatePaymentChannelInput,
    ) -> Result<PaymentChannel, StoreBillingError> {
        self.require_write()?;
        let current = self
            .payment_channel_by_id(id, false)
            .await?
            .ok_or(StoreBillingError::InvalidPaymentChannel)?;
        if input.expected_revision <= 0
            || input
                .adapter_kind
                .is_some_and(|adapter_kind| adapter_kind != current.adapter_kind)
        {
            return Err(StoreBillingError::InvalidPaymentChannel);
        }
        let name = input.name.unwrap_or(current.name);
        let icon_kind = input.icon_kind.unwrap_or(current.icon_kind);
        let icon_value = input.icon_value.or(current.icon_value);
        validate_payment_channel(&name, icon_kind, icon_value.as_deref())?;

        let write = self.db.write().await;
        let result = write
            .execute(self.db.stmt(
                "UPDATE store_payment_channels SET
                    adapter_kind = $2, name = $3, icon_kind = $4, icon_value = $5,
                    sort_order = $6, enabled = $7, revision = revision + 1,
                    updated_at = $8
                 WHERE id = $1 AND revision = $9",
                vec![
                    id.into(),
                    current.adapter_kind.as_str().into(),
                    name.trim().to_string().into(),
                    icon_kind.as_str().into(),
                    icon_value.into(),
                    input.sort_order.unwrap_or(current.sort_order).into(),
                    i32::from(input.enabled.unwrap_or(current.enabled)).into(),
                    timestamp(Utc::now()).into(),
                    input.expected_revision.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(StoreBillingError::Conflict);
        }
        drop(write);
        self.payment_channel_by_id(id, false)
            .await?
            .ok_or(StoreBillingError::InvalidPaymentChannel)
    }

    async fn payment_channel_by_id(
        &self,
        id: &str,
        enabled_only: bool,
    ) -> Result<Option<PaymentChannel>, StoreBillingError> {
        let suffix = if enabled_only { " AND enabled = 1" } else { "" };
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(
                &format!(
                    "SELECT id, adapter_kind, name, icon_kind, icon_value,
                            sort_order, enabled, CAST(revision AS BIGINT) AS revision, created_at, updated_at
                     FROM store_payment_channels WHERE id = $1{suffix}"
                ),
                vec![id.into()],
            ))
            .await
            .map_err(storage)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut channel = payment_channel_from_row(row)?;
        let availability = evaluate_channel(&self.db, self.db.read(), id, Utc::now())
            .await
            .map_err(storage)?;
        channel.effective_available = availability.effective_available;
        channel.unavailable_reasons = availability.unavailable_reasons;
        channel.supported_currencies = availability.supported_currencies;
        channel.amount_limits = availability.amount_limits;
        channel.checkout_action_kinds = availability.checkout_action_kinds;
        if channel.adapter_kind == PaymentAdapterKind::Epay {
            channel.epay_methods = load_epay_methods(&self.db, enabled_only)
                .await?
                .remove(id)
                .unwrap_or_default();
        }
        Ok(Some(channel))
    }

    /// Replaces the presentation and enabled state of one EPay method. Enabling a method does
    /// not by itself make the Channel available; the readiness and credential checks still
    /// apply through the normal availability evaluation.
    pub async fn update_epay_method(
        &self,
        channel_id: &str,
        method: EpayMethodKind,
        input: UpdateEpayMethodInput,
    ) -> Result<PaymentChannel, StoreBillingError> {
        self.require_write()?;
        validate_payment_channel(&input.label, input.icon_kind, input.icon_value.as_deref())?;
        let channel = self
            .payment_channel_by_id(channel_id, false)
            .await?
            .ok_or(StoreBillingError::NotFound)?;
        if channel.adapter_kind != PaymentAdapterKind::Epay {
            return Err(StoreBillingError::InvalidPaymentChannel);
        }
        let now = timestamp(Utc::now());
        let changed = self
            .db
            .write()
            .await
            .execute(self.db.stmt(
                "UPDATE store_epay_methods
                 SET label = $3, icon_kind = $4, icon_value = $5, sort_order = $6,
                     enabled = $7, updated_at = $8
                 WHERE channel_id = $1 AND method = $2",
                vec![
                    channel_id.into(),
                    method.as_str().into(),
                    input.label.trim().into(),
                    input.icon_kind.as_str().into(),
                    input.icon_value.clone().into(),
                    input.sort_order.into(),
                    i32::from(input.enabled).into(),
                    now.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        if changed.rows_affected() == 0 {
            return Err(StoreBillingError::NotFound);
        }
        self.payment_channel_by_id(channel_id, false)
            .await?
            .ok_or(StoreBillingError::NotFound)
    }

    pub async fn catalog(&self) -> Result<StoreCatalog, StoreBillingError> {
        let plan_features_enabled = QuotaGateStore::new(self.db.clone())
            .plan_features_enabled()
            .await
            .map_err(storage)?;
        let plan_filter = if plan_features_enabled {
            ""
        } else {
            " AND kind <> 'plan'"
        };
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "SELECT id, kind, name, description, price_currency, price_minor,
                            duration_seconds, group_ids, sort_order, enabled, created_at, updated_at
                     FROM store_products WHERE enabled = 1{plan_filter}
                     ORDER BY sort_order ASC, created_at ASC, id ASC"
                ),
                vec![],
            ))
            .await
            .map_err(storage)?;
        let mut products = Vec::with_capacity(rows.len());
        for row in rows {
            products.push(self.product_from_row(self.db.read(), row).await?);
        }

        let channel_rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                "SELECT id, adapter_kind, name, icon_kind, icon_value,
                        sort_order, enabled, CAST(revision AS BIGINT) AS revision, created_at, updated_at
                 FROM store_payment_channels WHERE enabled = 1
                 ORDER BY sort_order ASC, created_at ASC, id ASC",
                vec![],
            ))
            .await
            .map_err(storage)?;
        let availability = evaluate_channels(&self.db, self.db.read(), Utc::now())
            .await
            .map_err(storage)?;
        let mut epay_methods = load_epay_methods(&self.db, true).await?;
        let mut payment_channels = Vec::new();
        for row in channel_rows {
            let mut channel = payment_channel_from_row(row)?;
            let availability = availability
                .get(&channel.id)
                .ok_or_else(|| storage("Channel availability is missing"))?;
            if availability.effective_available {
                channel.effective_available = true;
                channel.unavailable_reasons = availability.unavailable_reasons.clone();
                channel.supported_currencies = availability.supported_currencies.clone();
                channel.amount_limits = availability.amount_limits.clone();
                channel.checkout_action_kinds = availability.checkout_action_kinds.clone();
                if channel.adapter_kind == PaymentAdapterKind::Epay {
                    channel.epay_methods = epay_methods.remove(&channel.id).unwrap_or_default();
                    // An EPay Channel with no enabled method is not selectable.
                    if channel.epay_methods.is_empty() {
                        continue;
                    }
                }
                payment_channels.push(channel);
            }
        }
        Ok(StoreCatalog {
            products,
            payment_channels,
            settings: self.get_settings().await?,
        })
    }

    pub async fn list_products_admin(&self) -> Result<Vec<StoreProduct>, StoreBillingError> {
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                "SELECT id, kind, name, description, price_currency, price_minor,
                        duration_seconds, group_ids, sort_order, enabled, created_at, updated_at
                 FROM store_products
                 ORDER BY sort_order ASC, created_at ASC, id ASC",
                vec![],
            ))
            .await
            .map_err(storage)?;
        let mut products = Vec::with_capacity(rows.len());
        for row in rows {
            products.push(self.product_from_row(self.db.read(), row).await?);
        }
        Ok(products)
    }

    pub async fn list_payment_channels_admin(
        &self,
    ) -> Result<Vec<PaymentChannel>, StoreBillingError> {
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                "SELECT id, adapter_kind, name, icon_kind, icon_value,
                        sort_order, enabled, CAST(revision AS BIGINT) AS revision, created_at, updated_at
                 FROM store_payment_channels
                 ORDER BY sort_order ASC, created_at ASC, id ASC",
                vec![],
            ))
            .await
            .map_err(storage)?;
        let availability = evaluate_channels(&self.db, self.db.read(), Utc::now())
            .await
            .map_err(storage)?;
        let mut epay_methods = load_epay_methods(&self.db, false).await?;
        let mut channels = Vec::with_capacity(rows.len());
        for row in rows {
            let mut channel = payment_channel_from_row(row)?;
            let availability = availability
                .get(&channel.id)
                .ok_or_else(|| storage("Channel availability is missing"))?;
            channel.effective_available = availability.effective_available;
            channel.unavailable_reasons = availability.unavailable_reasons.clone();
            channel.supported_currencies = availability.supported_currencies.clone();
            channel.amount_limits = availability.amount_limits.clone();
            channel.checkout_action_kinds = availability.checkout_action_kinds.clone();
            if channel.adapter_kind == PaymentAdapterKind::Epay {
                channel.epay_methods = epay_methods.remove(&channel.id).unwrap_or_default();
            }
            channels.push(channel);
        }
        Ok(channels)
    }

    pub async fn delete_product(&self, id: &str) -> Result<(), StoreBillingError> {
        self.require_write()?;
        let write = self.db.write().await;
        let result = write
            .execute(
                self.db
                    .stmt("DELETE FROM store_products WHERE id = $1", vec![id.into()]),
            )
            .await
            .map_err(delete_error)?;
        if result.rows_affected() == 0 {
            return Err(StoreBillingError::NotFound);
        }
        Ok(())
    }

    pub async fn delete_payment_channel(&self, id: &str) -> Result<(), StoreBillingError> {
        self.require_write()?;
        let write = self.db.write().await;
        let result = write
            .execute(self.db.stmt(
                "DELETE FROM store_payment_channels WHERE id = $1",
                vec![id.into()],
            ))
            .await
            .map_err(delete_error)?;
        if result.rows_affected() == 0 {
            return Err(StoreBillingError::NotFound);
        }
        Ok(())
    }

    pub async fn current_entitlement(
        &self,
        user_id: &str,
    ) -> Result<Option<PlanEntitlement>, StoreBillingError> {
        self.db
            .read()
            .query_one(self.db.stmt(
                "SELECT g.id, g.user_id, g.generation, g.product_id, g.product_name,
                        g.starts_at, g.ends_at, g.rate_numerator, g.rate_denominator,
                        g.group_ids, g.quota_json, g.source_kind, g.source_id
                 FROM store_plan_entitlement_current p
                 JOIN store_plan_entitlement_generations g ON g.id = p.entitlement_id
                 JOIN store_plan_entitlement_lifecycle l ON l.entitlement_id = g.id
                 WHERE p.user_id = $1 AND g.ends_at > $2
                   AND l.suspended_at IS NULL AND l.revoked_at IS NULL",
                vec![user_id.into(), timestamp(Utc::now()).into()],
            ))
            .await
            .map_err(storage)?
            .map(plan_entitlement_from_row)
            .transpose()
    }

    async fn credit_balance<C: ConnectionTrait>(
        &self,
        conn: &C,
        user_id: &str,
        delta: i128,
        kind: &str,
        idempotency_key: &str,
        metadata: serde_json::Value,
        created_at: DateTime<Utc>,
    ) -> Result<(), StoreBillingError> {
        let lock = if self.db.is_postgres() {
            " FOR UPDATE"
        } else {
            ""
        };
        let row = conn
            .query_one(self.db.stmt(
                &format!("SELECT balance_nano_usd FROM users WHERE id = $1{lock}"),
                vec![user_id.into()],
            ))
            .await
            .map_err(storage)?
            .ok_or_else(|| storage("reward user does not exist"))?;
        let previous = parse_signed_minor(&row_string(&row, "balance_nano_usd")?)?;
        let balance = previous
            .checked_add(delta)
            .ok_or(StoreBillingError::AmountOverflow)?;
        conn.execute(self.db.stmt(
            "UPDATE users SET balance_nano_usd = $2, updated_at = $3 WHERE id = $1",
            vec![
                user_id.into(),
                balance.to_string().into(),
                timestamp(created_at).into(),
            ],
        ))
        .await
        .map_err(storage)?;
        conn.execute(self.db.stmt(
            "INSERT INTO billing_ledger
                (id, user_id, kind, delta_nano_usd, balance_after_nano_usd,
                 meta_json, created_at, idempotency_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            vec![
                Uuid::new_v4().to_string().into(),
                user_id.into(),
                kind.into(),
                delta.to_string().into(),
                balance.to_string().into(),
                to_json(&metadata)?.into(),
                timestamp(created_at).into(),
                idempotency_key.into(),
            ],
        ))
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn activate_plan<C: ConnectionTrait>(
        &self,
        conn: &C,
        user_id: &str,
        product: &ProductSnapshot,
        cny_per_usd: &str,
        source_kind: &str,
        source_id: &str,
        starts_at: DateTime<Utc>,
    ) -> Result<(), StoreBillingError> {
        let duration = product
            .duration_seconds
            .ok_or_else(|| storage("plan duration is missing"))?;
        let ends_at = starts_at
            .checked_add_signed(Duration::seconds(duration))
            .ok_or(StoreBillingError::InvalidInput)?;
        let rate = super::money::ExchangeRateRational::parse(cny_per_usd)?;
        let expected_generation = conn
            .query_one(self.db.stmt(
                "SELECT generation FROM store_plan_entitlement_current WHERE user_id = $1",
                vec![user_id.into()],
            ))
            .await
            .map_err(storage)?
            .map(|row| row.try_get("", "generation").map_err(storage))
            .transpose()?;
        replace_entitlement_tx(
            &self.db,
            conn,
            EntitlementGenerationInput {
                expected_generation,
                user_id: user_id.to_string(),
                product_id: product.id.clone(),
                product_name: product.name.clone(),
                starts_at,
                ends_at,
                rate_numerator: rate.numerator().to_string(),
                rate_denominator: rate.denominator().to_string(),
                group_ids: product.group_ids.clone(),
                quotas: product.quotas.clone(),
                source_kind: source_kind.to_string(),
                source_id: source_id.to_string(),
            },
        )
        .await?;
        Ok(())
    }

    pub async fn generate_redemption_codes(
        &self,
        key_ring: &PaymentKeyRing,
        created_by_user_id: &str,
        input: GenerateRedemptionCodesInput,
    ) -> Result<Vec<GeneratedRedemptionCode>, StoreBillingError> {
        self.require_write()?;
        if !(1..=20).contains(&input.count) || !(1..=365).contains(&input.validity_days) {
            return Err(StoreBillingError::InvalidInput);
        }
        let reward = match input.reward {
            RedemptionRewardInput::Balance {
                currency,
                amount_minor,
            } => {
                if parse_minor(&amount_minor)? == 0 {
                    return Err(StoreBillingError::InvalidAmount);
                }
                PersistedReward::Balance {
                    currency,
                    amount_minor,
                }
            }
            RedemptionRewardInput::Plan { product_id } => {
                if !QuotaGateStore::new(self.db.clone())
                    .plan_features_enabled()
                    .await
                    .map_err(storage)?
                {
                    return Err(StoreBillingError::ProductNotAvailable);
                }
                let product = self
                    .product_by_id(&product_id, true)
                    .await?
                    .filter(|product| product.kind == ProductKind::Plan)
                    .ok_or(StoreBillingError::ProductNotAvailable)?;
                PersistedReward::Plan {
                    product: product_snapshot(&product),
                }
            }
        };
        let reward_kind = match &reward {
            PersistedReward::Balance { .. } => ProductKind::Balance,
            PersistedReward::Plan { .. } => ProductKind::Plan,
        };
        let reward_json = to_json(&reward)?;
        let created_at = Utc::now();
        let expires_at = created_at
            .checked_add_signed(Duration::days(input.validity_days))
            .ok_or(StoreBillingError::InvalidInput)?;
        let tx = self.db.begin_write().await.map_err(storage)?;
        let mut generated = Vec::with_capacity(input.count as usize);
        for _ in 0..input.count {
            let id = Uuid::new_v4().to_string();
            let material = generate_code_material(key_ring, &id)
                .map_err(|_| StoreBillingError::EncryptionUnavailable)?;
            let digest = code_digest(&material.normalized);
            tx.execute(self.db.stmt(
                "INSERT INTO store_redemption_codes
                    (id, code_format_version, code_digest, code_hint,
                     encrypted_format_version, encrypted_key_id, encrypted_nonce_base64,
                     encrypted_ciphertext_base64, ciphertext_destroyed_at,
                     reward_kind, reward_json, status, expires_at,
                     redeemed_by_user_id, redeemed_at, revoked_at,
                     created_by_user_id, created_at)
                 VALUES ($1, 2, $2, $3, $4, $5, $6, $7, NULL,
                         $8, $9, 'unused', $10, NULL, NULL, NULL, $11, $12)",
                vec![
                    id.clone().into(),
                    digest.into(),
                    material.hint.clone().into(),
                    i32::from(material.encrypted.version).into(),
                    material.encrypted.key_id.into(),
                    material.encrypted.nonce_base64.into(),
                    material.encrypted.ciphertext_base64.into(),
                    reward_kind.as_str().into(),
                    reward_json.clone().into(),
                    timestamp(expires_at).into(),
                    created_by_user_id.into(),
                    timestamp(created_at).into(),
                ],
            ))
            .await
            .map_err(storage)?;
            generated.push(GeneratedRedemptionCode {
                code: material.code,
                record: RedemptionCodeRecord {
                    id,
                    code_hint: material.hint,
                    reward_kind,
                    reward: serde_json::to_value(&reward).map_err(storage)?,
                    status: RedemptionCodeStatus::Unused,
                    expires_at,
                    redeemed_by_user_id: None,
                    redeemed_at: None,
                    created_by_user_id: created_by_user_id.to_string(),
                    created_at,
                    can_reveal: true,
                    reveal_unavailable_reason: None,
                },
            });
        }
        tx.commit().await.map_err(storage)?;
        Ok(generated)
    }

    pub async fn reveal_redemption_codes(
        &self,
        key_ring: &PaymentKeyRing,
        input: RevealRedemptionInput,
        context: &RedemptionAuditContext,
    ) -> Result<Vec<RevealedRedemptionCode>, StoreBillingError> {
        self.require_write()?;
        if !validate_reveal_input(&input) || !validate_audit_context(context) {
            return Err(StoreBillingError::InvalidInput);
        }
        let tx = self.db.begin_write().await.map_err(storage)?;
        let mut revealed = Vec::with_capacity(input.code_ids.len());
        for id in &input.code_ids {
            let row = tx
                .query_one(self.db.stmt(
                    "SELECT code_format_version, status, expires_at,
                            encrypted_format_version, encrypted_key_id,
                            encrypted_nonce_base64, encrypted_ciphertext_base64
                     FROM store_redemption_codes WHERE id = $1",
                    vec![id.into()],
                ))
                .await
                .map_err(storage)?
                .ok_or(StoreBillingError::InvalidRedemptionCode)?;
            if row_i32(&row, "code_format_version")? != 2
                || row_string(&row, "status")? != "unused"
                || parse_timestamp(&row_string(&row, "expires_at")?)? <= Utc::now()
            {
                return Err(StoreBillingError::InvalidRedemptionCode);
            }
            let encrypted = encrypted_redemption_from_row(&row)?;
            let code = decrypt_code(key_ring, id, &encrypted)
                .map_err(|_| StoreBillingError::EncryptionUnavailable)?;
            revealed.push(RevealedRedemptionCode {
                id: id.clone(),
                code,
            });
        }
        let now = Utc::now();
        tx.execute(self.db.stmt(
            "INSERT INTO store_access_audits
                (id, actor_id, actor_role, action, scope_json, reason, result, created_at)
             VALUES ($1, $2, 'admin', $3, $4, 'redemption_access', 'success', $5)",
            vec![
                Uuid::new_v4().to_string().into(),
                context.admin_user_id.clone().into(),
                input.action.audit_action().into(),
                serde_json::json!({
                    "code_ids": input.code_ids,
                    "count": revealed.len(),
                    "source_ip": context.source_ip,
                    "user_agent": context.user_agent,
                })
                .to_string()
                .into(),
                timestamp(now).into(),
            ],
        ))
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(revealed)
    }

    pub async fn revoke_redemption_code(
        &self,
        code_id: &str,
        admin_user_id: &str,
    ) -> Result<RedemptionCodeRecord, StoreBillingError> {
        self.require_write()?;
        if code_id.is_empty() || admin_user_id.trim().is_empty() {
            return Err(StoreBillingError::InvalidInput);
        }
        let tx = self.db.begin_write().await.map_err(storage)?;
        let now = Utc::now();
        let changed = tx
            .execute(self.db.stmt(
                "UPDATE store_redemption_codes
                 SET status = 'revoked', encrypted_format_version = NULL,
                     encrypted_key_id = NULL, encrypted_nonce_base64 = NULL,
                     encrypted_ciphertext_base64 = NULL,
                     ciphertext_destroyed_at = CASE
                         WHEN code_format_version = 2 THEN $2 ELSE NULL END,
                     revoked_at = $2
                 WHERE id = $1 AND status = 'unused'",
                vec![code_id.into(), timestamp(now).into()],
            ))
            .await
            .map_err(storage)?;
        if changed.rows_affected() != 1 {
            return Err(StoreBillingError::Conflict);
        }
        tx.execute(self.db.stmt(
            "INSERT INTO store_access_audits
                (id, actor_id, actor_role, action, scope_json, reason, result, created_at)
             VALUES ($1, $2, 'admin', 'redemption_revoke', $3,
                     'redemption_revocation', 'success', $4)",
            vec![
                Uuid::new_v4().to_string().into(),
                admin_user_id.into(),
                serde_json::json!({"code_ids": [code_id], "count": 1})
                    .to_string()
                    .into(),
                timestamp(now).into(),
            ],
        ))
        .await
        .map_err(storage)?;
        let record = tx
            .query_one(self.db.stmt(
                &format!("{} WHERE id = $1", redemption_select()),
                vec![code_id.into()],
            ))
            .await
            .map_err(storage)?
            .ok_or(StoreBillingError::NotFound)
            .and_then(redemption_record_from_row)?;
        tx.commit().await.map_err(storage)?;
        Ok(record)
    }

    pub async fn cleanup_expired_redemption_ciphertexts(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, StoreBillingError> {
        self.require_write()?;
        let cutoff = now
            .checked_sub_signed(Duration::hours(24))
            .ok_or(StoreBillingError::InvalidInput)?;
        self.db
            .write()
            .await
            .execute(self.db.stmt(
                "UPDATE store_redemption_codes
                 SET encrypted_format_version = NULL, encrypted_key_id = NULL,
                     encrypted_nonce_base64 = NULL, encrypted_ciphertext_base64 = NULL,
                     ciphertext_destroyed_at = $2
                 WHERE code_format_version = 2 AND status = 'unused'
                   AND expires_at <= $1 AND encrypted_ciphertext_base64 IS NOT NULL",
                vec![timestamp(cutoff).into(), timestamp(now).into()],
            ))
            .await
            .map(|result| result.rows_affected())
            .map_err(storage)
    }

    pub async fn redeem(
        &self,
        user_id: &str,
        code: &str,
        rate: Option<&ExchangeRateSnapshot>,
        source_ip: &str,
    ) -> Result<RedemptionCodeRecord, StoreBillingError> {
        self.require_write()?;
        let source_ip_digest =
            source_ip_digest(source_ip).ok_or(StoreBillingError::InvalidInput)?;
        let tx = self.db.begin_write().await.map_err(storage)?;
        let lock = if self.db.is_postgres() {
            " FOR UPDATE"
        } else {
            ""
        };
        let hold = tx
            .query_one(self.db.stmt(
                &format!(
                    "SELECT active FROM store_balance_holds
                     WHERE user_id = $1 AND active = 1{lock}"
                ),
                vec![user_id.into()],
            ))
            .await
            .map_err(storage)?;
        if hold.is_some() {
            return Err(StoreBillingError::PaymentHold);
        }
        let redeemed_at = Utc::now();
        if let Err(error) = check_redemption_limit(
            &self.db,
            &*tx,
            user_id,
            &source_ip_digest,
            redeemed_at,
            lock,
        )
        .await
        {
            tx.commit().await.map_err(storage)?;
            return Err(error);
        }
        let Some(normalized) = normalize_code(code) else {
            record_redemption_attempt(
                &self.db,
                &*tx,
                user_id,
                &source_ip_digest,
                false,
                redeemed_at,
            )
            .await?;
            tx.commit().await.map_err(storage)?;
            return Err(StoreBillingError::InvalidRedemptionCode);
        };
        let row = tx
            .query_one(self.db.stmt(
                &format!("{} WHERE code_digest = $1{lock}", redemption_select()),
                vec![code_digest(&normalized).into()],
            ))
            .await
            .map_err(storage)?;
        let Some(row) = row else {
            record_redemption_attempt(
                &self.db,
                &*tx,
                user_id,
                &source_ip_digest,
                false,
                redeemed_at,
            )
            .await?;
            tx.commit().await.map_err(storage)?;
            return Err(StoreBillingError::InvalidRedemptionCode);
        };
        let mut record = redemption_record_from_row(row)?;
        let rejected = match record.status {
            RedemptionCodeStatus::Used => Some(StoreBillingError::RedemptionCodeUsed),
            RedemptionCodeStatus::Revoked => Some(StoreBillingError::RedemptionCodeRevoked),
            RedemptionCodeStatus::Unused if record.expires_at <= redeemed_at => {
                Some(StoreBillingError::RedemptionCodeExpired)
            }
            RedemptionCodeStatus::Unused => None,
        };
        if let Some(error) = rejected {
            record_redemption_attempt(
                &self.db,
                &*tx,
                user_id,
                &source_ip_digest,
                false,
                redeemed_at,
            )
            .await?;
            tx.commit().await.map_err(storage)?;
            return Err(error);
        }
        let reward: PersistedReward =
            serde_json::from_value(record.reward.clone()).map_err(storage)?;
        match reward {
            PersistedReward::Balance {
                currency,
                amount_minor,
            } => {
                let rate_value = match currency {
                    Currency::USD => "1",
                    Currency::CNY => {
                        let rate = rate.ok_or(StoreBillingError::InvalidExchangeRate)?;
                        validate_rate_snapshot(rate)?;
                        &rate.cny_per_usd
                    }
                };
                let delta =
                    quoted_received_to_nano_usd(parse_minor(&amount_minor)?, currency, rate_value)?;
                self.credit_balance(
                    &*tx,
                    user_id,
                    delta,
                    "redemption_credit",
                    &format!("store-redemption:{}", record.id),
                    serde_json::json!({"redemption_code_id": record.id}),
                    redeemed_at,
                )
                .await?;
            }
            PersistedReward::Plan { product } => {
                let rate = rate.ok_or(StoreBillingError::InvalidExchangeRate)?;
                validate_rate_snapshot(rate)?;
                self.activate_plan(
                    &*tx,
                    user_id,
                    &product,
                    &rate.cny_per_usd,
                    "redemption",
                    &record.id,
                    redeemed_at,
                )
                .await?;
            }
        }
        tx.execute(self.db.stmt(
            "UPDATE store_redemption_codes
             SET status = 'used', redeemed_by_user_id = $2, redeemed_at = $3,
                 encrypted_format_version = NULL, encrypted_key_id = NULL,
                 encrypted_nonce_base64 = NULL, encrypted_ciphertext_base64 = NULL,
                 ciphertext_destroyed_at = CASE
                     WHEN code_format_version = 2 THEN $3 ELSE NULL END
             WHERE id = $1 AND status = 'unused'",
            vec![
                record.id.clone().into(),
                user_id.into(),
                timestamp(redeemed_at).into(),
            ],
        ))
        .await
        .map_err(storage)?;
        record_redemption_attempt(
            &self.db,
            &*tx,
            user_id,
            &source_ip_digest,
            true,
            redeemed_at,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        record.status = RedemptionCodeStatus::Used;
        record.redeemed_by_user_id = Some(user_id.to_string());
        record.redeemed_at = Some(redeemed_at);
        record.can_reveal = false;
        record.reveal_unavailable_reason =
            Some(RedemptionRevealUnavailableReason::CiphertextDestroyed);
        Ok(record)
    }

    pub async fn list_redemption_codes_admin(
        &self,
        limit: u64,
    ) -> Result<Vec<RedemptionCodeRecord>, StoreBillingError> {
        self.db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "{} ORDER BY created_at DESC, id DESC LIMIT $1",
                    redemption_select()
                ),
                vec![(limit.min(100) as i64).into()],
            ))
            .await
            .map_err(storage)?
            .into_iter()
            .map(redemption_record_from_row)
            .collect()
    }
}

fn plan_quota_from_row(row: QueryResult) -> Result<PlanQuota, StoreBillingError> {
    Ok(PlanQuota {
        id: row_string(&row, "id")?,
        window_kind: WindowKind::from_str(&row_string(&row, "window_kind")?)
            .ok_or_else(|| storage("stored quota window kind is invalid"))?,
        window_seconds: row_i64(&row, "window_seconds")?,
        quota_fen_cny: row_string(&row, "quota_fen_cny")?,
        sort_order: row.try_get("", "sort_order").map_err(storage)?,
    })
}

fn validate_payment_channel(
    name: &str,
    icon_kind: IconKind,
    icon_value: Option<&str>,
) -> Result<(), StoreBillingError> {
    if !(1..=80).contains(&name.trim().chars().count()) {
        return Err(StoreBillingError::InvalidInput);
    }
    if icon_kind == IconKind::Url && !icon_value.is_some_and(|value| value.starts_with("https://"))
    {
        return Err(StoreBillingError::InvalidInput);
    }
    if icon_kind == IconKind::Upload
        && !icon_value.is_some_and(|value| value.starts_with("/api/dashboard/store/icons/"))
    {
        return Err(StoreBillingError::InvalidInput);
    }
    Ok(())
}

fn validate_payment_icon(content: &[u8]) -> Result<&'static str, StoreBillingError> {
    if content.is_empty() || content.len() > PAYMENT_ICON_MAX_BYTES {
        return Err(StoreBillingError::InvalidIcon);
    }
    if content.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if content.starts_with(b"\xff\xd8\xff") {
        return Ok("image/jpeg");
    }
    if content.len() >= 12 && content.starts_with(b"RIFF") && &content[8..12] == b"WEBP" {
        return Ok("image/webp");
    }

    validate_svg(content)?;
    Ok("image/svg+xml")
}

fn validate_svg(content: &[u8]) -> Result<(), StoreBillingError> {
    std::str::from_utf8(content).map_err(|_| StoreBillingError::InvalidIcon)?;
    let mut reader = Reader::from_reader(content);
    reader.config_mut().check_end_names = true;
    let mut root_seen = false;
    let mut root_closed = false;
    let mut depth = 0_usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                if root_closed
                    || (!root_seen && !is_local_name(element.local_name().as_ref(), "svg"))
                {
                    return Err(StoreBillingError::InvalidIcon);
                }
                validate_svg_element(&element)?;
                root_seen = true;
                depth = depth.checked_add(1).ok_or(StoreBillingError::InvalidIcon)?;
            }
            Ok(Event::Empty(element)) => {
                if root_closed
                    || (!root_seen && !is_local_name(element.local_name().as_ref(), "svg"))
                {
                    return Err(StoreBillingError::InvalidIcon);
                }
                validate_svg_element(&element)?;
                if !root_seen {
                    root_seen = true;
                    root_closed = true;
                }
            }
            Ok(Event::End(_)) => {
                depth = depth.checked_sub(1).ok_or(StoreBillingError::InvalidIcon)?;
                if depth == 0 {
                    root_closed = true;
                }
            }
            Ok(Event::Text(text)) => {
                if depth == 0
                    && text
                        .as_ref()
                        .chars()
                        .any(|character| !character.is_ascii_whitespace())
                {
                    return Err(StoreBillingError::InvalidIcon);
                }
            }
            Ok(Event::CData(_)) if depth == 0 => return Err(StoreBillingError::InvalidIcon),
            Ok(Event::GeneralRef(_)) if depth == 0 => return Err(StoreBillingError::InvalidIcon),
            Ok(Event::Decl(_)) if root_seen => return Err(StoreBillingError::InvalidIcon),
            Ok(Event::PI(_) | Event::DocType(_)) => return Err(StoreBillingError::InvalidIcon),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(StoreBillingError::InvalidIcon),
        }
    }

    if root_seen && root_closed && depth == 0 {
        Ok(())
    } else {
        Err(StoreBillingError::InvalidIcon)
    }
}

fn validate_svg_element(element: &BytesStart<'_>) -> Result<(), StoreBillingError> {
    const FORBIDDEN_ELEMENTS: [&str; 8] = [
        "script",
        "style",
        "foreignObject",
        "iframe",
        "object",
        "embed",
        "use",
        "image",
    ];
    let local_name = element.local_name();
    if FORBIDDEN_ELEMENTS
        .iter()
        .any(|forbidden| local_name.as_ref().eq_ignore_ascii_case(forbidden))
    {
        return Err(StoreBillingError::InvalidIcon);
    }
    for attribute in element.attributes().with_checks(true) {
        let attribute = attribute.map_err(|_| StoreBillingError::InvalidIcon)?;
        let local_name = attribute.key.local_name();
        let local_name = local_name.as_ref();
        if local_name
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("on"))
            || is_local_name(local_name, "href")
            || is_local_name(local_name, "style")
        {
            return Err(StoreBillingError::InvalidIcon);
        }
    }
    Ok(())
}

fn is_local_name(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

fn payment_channel_from_row(row: QueryResult) -> Result<PaymentChannel, StoreBillingError> {
    Ok(PaymentChannel {
        id: row_string(&row, "id")?,
        adapter_kind: PaymentAdapterKind::from_str(&row_string(&row, "adapter_kind")?)
            .ok_or_else(|| storage("stored payment adapter kind is invalid"))?,
        name: row_string(&row, "name")?,
        icon_kind: IconKind::from_str(&row_string(&row, "icon_kind")?)
            .ok_or_else(|| storage("stored icon kind is invalid"))?,
        icon_value: row_optional_string(&row, "icon_value")?,
        sort_order: row.try_get("", "sort_order").map_err(storage)?,
        enabled: row_i32(&row, "enabled")? != 0,
        revision: row.try_get("", "revision").map_err(storage)?,
        effective_available: false,
        unavailable_reasons: Vec::new(),
        supported_currencies: Vec::new(),
        amount_limits: Default::default(),
        checkout_action_kinds: Vec::new(),
        epay_methods: Vec::new(),
        created_at: parse_timestamp(&row_string(&row, "created_at")?)?,
        updated_at: parse_timestamp(&row_string(&row, "updated_at")?)?,
    })
}

fn epay_method_from_row(row: &QueryResult) -> Result<EpayMethodConfig, StoreBillingError> {
    Ok(EpayMethodConfig {
        method: EpayMethodKind::from_str(&row_string(row, "method")?)
            .ok_or_else(|| storage("stored EPay method is invalid"))?,
        label: row_string(row, "label")?,
        icon_kind: IconKind::from_str(&row_string(row, "icon_kind")?)
            .ok_or_else(|| storage("stored EPay method icon kind is invalid"))?,
        icon_value: row_optional_string(row, "icon_value")?,
        sort_order: row.try_get("", "sort_order").map_err(storage)?,
        enabled: row_i32(row, "enabled")? != 0,
    })
}

/// Loads the EPay method configuration for every EPay Channel in one query. `enabled_only`
/// restricts the result to the methods a user may select.
async fn load_epay_methods(
    db: &DbPool,
    enabled_only: bool,
) -> Result<std::collections::HashMap<String, Vec<EpayMethodConfig>>, StoreBillingError> {
    let filter = if enabled_only {
        " WHERE enabled = 1"
    } else {
        ""
    };
    let rows = db
        .read()
        .query_all(db.stmt(
            &format!(
                "SELECT channel_id, method, label, icon_kind, icon_value, sort_order, enabled
                 FROM store_epay_methods{filter}
                 ORDER BY sort_order ASC, method ASC"
            ),
            vec![],
        ))
        .await
        .map_err(storage)?;
    let mut methods: std::collections::HashMap<String, Vec<EpayMethodConfig>> =
        std::collections::HashMap::new();
    for row in &rows {
        methods
            .entry(row_string(row, "channel_id")?)
            .or_default()
            .push(epay_method_from_row(row)?);
    }
    Ok(methods)
}

fn validate_rate_snapshot(rate: &ExchangeRateSnapshot) -> Result<(), StoreBillingError> {
    if rate.base != "USD" || rate.quote != "CNY" {
        return Err(StoreBillingError::InvalidExchangeRate);
    }
    convert_minor(1, Currency::USD, Currency::USD, &rate.cny_per_usd)?;
    Ok(())
}

fn plan_entitlement_from_row(row: QueryResult) -> Result<PlanEntitlement, StoreBillingError> {
    Ok(PlanEntitlement {
        id: row_string(&row, "id")?,
        user_id: row_string(&row, "user_id")?,
        generation: row.try_get("", "generation").map_err(storage)?,
        product_id: row_string(&row, "product_id")?,
        product_name: row_string(&row, "product_name")?,
        starts_at: parse_timestamp(&row_string(&row, "starts_at")?)?,
        ends_at: parse_timestamp(&row_string(&row, "ends_at")?)?,
        rate_numerator: row_string(&row, "rate_numerator")?,
        rate_denominator: row_string(&row, "rate_denominator")?,
        group_ids: parse_json(&row_string(&row, "group_ids")?)?,
        quotas: parse_json(&row_string(&row, "quota_json")?)?,
        source_kind: row_string(&row, "source_kind")?,
        source_id: row_string(&row, "source_id")?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum PersistedReward {
    Balance {
        currency: Currency,
        amount_minor: String,
    },
    Plan {
        product: ProductSnapshot,
    },
}

fn product_snapshot(product: &StoreProduct) -> ProductSnapshot {
    ProductSnapshot {
        id: product.id.clone(),
        kind: product.kind,
        name: product.name.clone(),
        description: product.description.clone(),
        price_currency: product.price_currency,
        price_minor: product.price_minor.clone(),
        duration_seconds: product.duration_seconds,
        group_ids: product.group_ids.clone(),
        balance: product.balance.clone(),
        quotas: product.quotas.clone(),
    }
}

fn redemption_select() -> &'static str {
    "SELECT id, code_format_version, code_hint, reward_kind, reward_json, status, expires_at,
            encrypted_format_version, encrypted_key_id, encrypted_nonce_base64,
            encrypted_ciphertext_base64, redeemed_by_user_id, redeemed_at,
            created_by_user_id, created_at
     FROM store_redemption_codes"
}

fn redemption_record_from_row(row: QueryResult) -> Result<RedemptionCodeRecord, StoreBillingError> {
    let code_format_version = row_i32(&row, "code_format_version")?;
    let status = RedemptionCodeStatus::from_str(&row_string(&row, "status")?)
        .ok_or_else(|| storage("stored redemption status is invalid"))?;
    let expires_at = parse_timestamp(&row_string(&row, "expires_at")?)?;
    let encrypted_value_present = row
        .try_get::<Option<i32>>("", "encrypted_format_version")
        .map_err(storage)?
        .is_some()
        && row_optional_string(&row, "encrypted_key_id")?.is_some()
        && row_optional_string(&row, "encrypted_nonce_base64")?.is_some()
        && row_optional_string(&row, "encrypted_ciphertext_base64")?.is_some();
    let can_reveal = code_format_version == 2
        && status == RedemptionCodeStatus::Unused
        && expires_at > Utc::now()
        && encrypted_value_present;
    let reveal_unavailable_reason = if code_format_version == 1 {
        Some(RedemptionRevealUnavailableReason::LegacyDigestOnly)
    } else if code_format_version == 2 && !encrypted_value_present {
        Some(RedemptionRevealUnavailableReason::CiphertextDestroyed)
    } else {
        None
    };
    Ok(RedemptionCodeRecord {
        id: row_string(&row, "id")?,
        code_hint: row_string(&row, "code_hint")?,
        reward_kind: ProductKind::from_str(&row_string(&row, "reward_kind")?)
            .ok_or_else(|| storage("stored redemption reward kind is invalid"))?,
        reward: parse_json(&row_string(&row, "reward_json")?)?,
        status,
        expires_at,
        redeemed_by_user_id: row_optional_string(&row, "redeemed_by_user_id")?,
        redeemed_at: row_optional_string(&row, "redeemed_at")?
            .map(|value| parse_timestamp(&value))
            .transpose()?,
        created_by_user_id: row_string(&row, "created_by_user_id")?,
        created_at: parse_timestamp(&row_string(&row, "created_at")?)?,
        can_reveal,
        reveal_unavailable_reason,
    })
}

fn encrypted_redemption_from_row(row: &QueryResult) -> Result<EncryptedSecret, StoreBillingError> {
    let version = row
        .try_get::<Option<i32>>("", "encrypted_format_version")
        .map_err(storage)?
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(StoreBillingError::EncryptionUnavailable)?;
    Ok(EncryptedSecret {
        version,
        key_id: row_optional_string(row, "encrypted_key_id")?
            .ok_or(StoreBillingError::EncryptionUnavailable)?,
        nonce_base64: row_optional_string(row, "encrypted_nonce_base64")?
            .ok_or(StoreBillingError::EncryptionUnavailable)?,
        ciphertext_base64: row_optional_string(row, "encrypted_ciphertext_base64")?
            .ok_or(StoreBillingError::EncryptionUnavailable)?,
    })
}

async fn check_redemption_limit<C: ConnectionTrait>(
    db: &DbPool,
    conn: &C,
    user_id: &str,
    source_digest: &str,
    now: DateTime<Utc>,
    lock: &str,
) -> Result<(), StoreBillingError> {
    let now_text = timestamp(now);
    conn.execute(db.stmt(
        "INSERT INTO store_redemption_limits
            (user_id, source_ip_digest, cooldown_until, updated_at)
         VALUES ($1, $2, NULL, $3)
         ON CONFLICT (user_id, source_ip_digest) DO NOTHING",
        vec![
            user_id.into(),
            source_digest.into(),
            now_text.clone().into(),
        ],
    ))
    .await
    .map_err(storage)?;
    let limit = conn
        .query_one(db.stmt(
            &format!(
                "SELECT cooldown_until FROM store_redemption_limits
                 WHERE user_id = $1 AND source_ip_digest = $2{lock}"
            ),
            vec![user_id.into(), source_digest.into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| storage("redemption limit row is missing"))?;
    if row_optional_string(&limit, "cooldown_until")?
        .map(|value| parse_timestamp(&value))
        .transpose()?
        .is_some_and(|value| value > now)
    {
        return Err(StoreBillingError::RedemptionCooldown);
    }
    let cutoff = now
        .checked_sub_signed(Duration::minutes(1))
        .ok_or(StoreBillingError::InvalidInput)?;
    let attempts = conn
        .query_one(db.stmt(
            "SELECT COUNT(*) AS value FROM store_redemption_attempts
             WHERE user_id = $1 AND source_ip_digest = $2 AND attempted_at > $3",
            vec![
                user_id.into(),
                source_digest.into(),
                timestamp(cutoff).into(),
            ],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| storage("redemption attempt count is missing"))?
        .try_get::<i64>("", "value")
        .map_err(storage)?;
    if attempts >= 10 {
        return Err(StoreBillingError::RedemptionRateLimited);
    }
    Ok(())
}

async fn record_redemption_attempt<C: ConnectionTrait>(
    db: &DbPool,
    conn: &C,
    user_id: &str,
    source_digest: &str,
    success: bool,
    now: DateTime<Utc>,
) -> Result<(), StoreBillingError> {
    let now_text = timestamp(now);
    conn.execute(db.stmt(
        "INSERT INTO store_redemption_attempts
            (id, user_id, source_ip_digest, success, attempted_at)
         VALUES ($1, $2, $3, $4, $5)",
        vec![
            Uuid::new_v4().to_string().into(),
            user_id.into(),
            source_digest.into(),
            i32::from(success).into(),
            now_text.clone().into(),
        ],
    ))
    .await
    .map_err(storage)?;
    if !success {
        let cutoff = now
            .checked_sub_signed(Duration::minutes(15))
            .ok_or(StoreBillingError::InvalidInput)?;
        let failures = conn
            .query_one(db.stmt(
                "SELECT COUNT(*) AS value FROM store_redemption_attempts
                 WHERE user_id = $1 AND source_ip_digest = $2
                   AND success = 0 AND attempted_at > $3",
                vec![
                    user_id.into(),
                    source_digest.into(),
                    timestamp(cutoff).into(),
                ],
            ))
            .await
            .map_err(storage)?
            .ok_or_else(|| storage("redemption failure count is missing"))?
            .try_get::<i64>("", "value")
            .map_err(storage)?;
        if failures >= 5 {
            let cooldown_until = now
                .checked_add_signed(Duration::minutes(30))
                .ok_or(StoreBillingError::InvalidInput)?;
            conn.execute(db.stmt(
                "UPDATE store_redemption_limits
                 SET cooldown_until = $3, updated_at = $4
                 WHERE user_id = $1 AND source_ip_digest = $2",
                vec![
                    user_id.into(),
                    source_digest.into(),
                    timestamp(cooldown_until).into(),
                    now_text.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        }
    }
    Ok(())
}

fn storage(error: impl ToString) -> StoreBillingError {
    StoreBillingError::Storage(error.to_string())
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, StoreBillingError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(storage)
}

fn row_string(row: &QueryResult, column: &str) -> Result<String, StoreBillingError> {
    row.try_get("", column).map_err(storage)
}

fn row_optional_string(
    row: &QueryResult,
    column: &str,
) -> Result<Option<String>, StoreBillingError> {
    row.try_get("", column).map_err(storage)
}

fn row_i64(row: &QueryResult, column: &str) -> Result<i64, StoreBillingError> {
    row.try_get("", column).map_err(storage)
}

fn row_i32(row: &QueryResult, column: &str) -> Result<i32, StoreBillingError> {
    row.try_get("", column).map_err(storage)
}

fn currency_string(currency: Currency) -> &'static str {
    match currency {
        Currency::CNY => "CNY",
        Currency::USD => "USD",
    }
}

fn parse_currency(value: &str) -> Result<Currency, StoreBillingError> {
    match value {
        "CNY" => Ok(Currency::CNY),
        "USD" => Ok(Currency::USD),
        _ => Err(storage("stored currency is invalid")),
    }
}

fn parse_json<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, StoreBillingError> {
    serde_json::from_str(value).map_err(storage)
}

fn to_json<T: Serialize>(value: &T) -> Result<String, StoreBillingError> {
    serde_json::to_string(value).map_err(storage)
}

fn delete_error(error: impl ToString) -> StoreBillingError {
    let message = error.to_string();
    if message.to_ascii_lowercase().contains("foreign key") {
        StoreBillingError::Conflict
    } else {
        StoreBillingError::Storage(message)
    }
}

fn canonical_group_ids(group_ids: &[String]) -> Result<Vec<String>, StoreBillingError> {
    let mut seen = std::collections::HashSet::new();
    let mut canonical = Vec::new();
    for group_id in group_ids {
        let group_id = group_id.trim();
        if group_id.is_empty() || !seen.insert(group_id.to_string()) {
            continue;
        }
        canonical.push(group_id.to_string());
    }
    if canonical.len() > 32 {
        return Err(StoreBillingError::InvalidInput);
    }
    Ok(canonical)
}

fn validate_settings(settings: &StoreSettings) -> Result<(), StoreBillingError> {
    for (minimum, maximum) in [
        (
            &settings.custom_recharge_cny_min_minor,
            &settings.custom_recharge_cny_max_minor,
        ),
        (
            &settings.custom_recharge_usd_min_minor,
            &settings.custom_recharge_usd_max_minor,
        ),
    ] {
        let minimum = parse_minor(minimum)?;
        let maximum = parse_minor(maximum)?;
        if minimum == 0 || maximum == 0 || minimum > maximum {
            return Err(StoreBillingError::InvalidAmount);
        }
    }
    Ok(())
}

fn validate_product(input: &CreateProductInput) -> Result<(), StoreBillingError> {
    let name_len = input.name.trim().chars().count();
    if !(1..=100).contains(&name_len) || input.description.trim().chars().count() > 500 {
        return Err(StoreBillingError::InvalidInput);
    }
    if parse_minor(&input.price_minor)? == 0 {
        return Err(StoreBillingError::InvalidAmount);
    }

    match input.kind {
        ProductKind::Balance => {
            let balance = input
                .balance
                .as_ref()
                .ok_or(StoreBillingError::InvalidInput)?;
            let recharge = parse_minor(&balance.recharge_minor)?;
            parse_minor(&balance.bonus_minor)?;
            if recharge == 0
                || balance.recharge_minor != input.price_minor
                || input.duration_seconds.is_some()
                || !input.group_ids.is_empty()
                || !input.quotas.is_empty()
            {
                return Err(StoreBillingError::InvalidInput);
            }
        }
        ProductKind::Plan => {
            if input.balance.is_some()
                || !matches!(input.duration_seconds, Some(3600..=31_536_000))
                || input.quotas.is_empty()
            {
                return Err(StoreBillingError::InvalidInput);
            }
            let mut windows = std::collections::HashSet::new();
            for quota in &input.quotas {
                if parse_minor(&quota.quota_fen_cny)? == 0
                    || !valid_window(quota.window_kind, quota.window_seconds)
                    || !windows.insert(quota.window_seconds)
                {
                    return Err(StoreBillingError::InvalidInput);
                }
            }
        }
    }
    Ok(())
}

fn valid_window(kind: WindowKind, seconds: i64) -> bool {
    match kind {
        WindowKind::FiveHours => seconds == 18_000,
        WindowKind::TwelveHours => seconds == 43_200,
        WindowKind::Day => seconds == 86_400,
        WindowKind::Week => seconds == 604_800,
        WindowKind::Month => seconds == 2_592_000,
        WindowKind::Custom => (3_600..=31_536_000).contains(&seconds) && seconds % 3_600 == 0,
    }
}

fn actual_received(balance: &BalanceProductInput) -> Result<String, StoreBillingError> {
    parse_minor(&balance.recharge_minor)?
        .checked_add(parse_minor(&balance.bonus_minor)?)
        .map(|value| value.to_string())
        .ok_or(StoreBillingError::AmountOverflow)
}

#[cfg(test)]
mod tests {
    use super::{StoreBillingError, StoreBillingStore, row_i32, validate_payment_icon};
    use crate::db::DbPool;
    use crate::migration::Migrator;
    use sea_orm::QueryResult;
    use sea_orm_migration::MigratorTrait;

    #[test]
    fn store_integer_boolean_decoder_uses_postgres_int4_width() {
        let decoder = row_i32 as fn(&QueryResult, &str) -> Result<i32, StoreBillingError>;
        let _ = decoder;
    }

    #[test]
    fn payment_icon_validation_uses_exact_file_signatures() {
        assert_eq!(
            validate_payment_icon(b"\x89PNG\r\n\x1a\nbody").unwrap(),
            "image/png"
        );
        assert_eq!(
            validate_payment_icon(b"\xff\xd8\xff\xe0jpeg").unwrap(),
            "image/jpeg"
        );
        assert_eq!(
            validate_payment_icon(b"RIFF\x04\0\0\0WEBPdata").unwrap(),
            "image/webp"
        );
        assert_eq!(
            validate_payment_icon(b"<?xml version=\"1.0\"?><svg viewBox=\"0 0 1 1\"></svg>")
                .unwrap(),
            "image/svg+xml"
        );

        for invalid in [
            b"not an image".as_slice(),
            b"<svg><script>alert(1)</script></svg>".as_slice(),
            b"<svg xmlns:s='urn:test'><s:script>alert(1)</s:script></svg>".as_slice(),
            b"<svg onload=\"alert(1)\"></svg>".as_slice(),
            b"<svg><image href=\"https://example.test/a.png\"/></svg>".as_slice(),
            b"<svg><path xlink:href='//example.test/a.svg#x'/></svg>".as_slice(),
            b"<svg><path></svg>".as_slice(),
            b"<?unsafe value?><svg></svg>".as_slice(),
            b"\xff<svg></svg>".as_slice(),
        ] {
            assert_eq!(
                validate_payment_icon(invalid),
                Err(StoreBillingError::InvalidIcon)
            );
        }
        assert_eq!(
            validate_payment_icon(&vec![0_u8; 2 * 1024 * 1024 + 1]),
            Err(StoreBillingError::InvalidIcon)
        );
    }

    #[tokio::test]
    async fn payment_icon_storage_round_trips_exact_bytes() {
        let db = DbPool::connect("sqlite::memory:")
            .await
            .expect("connect db");
        {
            let write = db.write().await;
            Migrator::up(&*write, None).await.expect("migrate db");
        }
        let store = StoreBillingStore::new(db);
        let content = b"\x89PNG\r\n\x1a\nexact\0bytes".to_vec();

        let saved = store
            .save_payment_icon(content.clone())
            .await
            .expect("save icon");
        assert_eq!(saved.content_type, "image/png");
        assert_eq!(saved.content, content);

        let loaded = store
            .get_payment_icon(&saved.id)
            .await
            .expect("load icon")
            .expect("icon exists");
        assert_eq!(loaded, saved);
        assert!(store.get_payment_icon("missing").await.unwrap().is_none());
    }
}

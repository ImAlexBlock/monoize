use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait, TryGetable};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if !matches!(backend, DbBackend::Sqlite | DbBackend::Postgres) {
            return Ok(());
        }

        reject_unimported_legacy_secrets(manager, backend).await?;
        let tx = manager.get_connection().begin().await?;
        for sql in up_statements(backend) {
            tx.execute(Statement::from_string(backend, sql)).await?;
        }
        tx.commit().await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if !matches!(backend, DbBackend::Sqlite | DbBackend::Postgres) {
            return Ok(());
        }

        let tx = manager.get_connection().begin().await?;
        for table in PAYMENT_TABLES.iter().rev() {
            tx.execute(Statement::from_string(
                backend,
                format!("DROP TABLE IF EXISTS {table}"),
            ))
            .await?;
        }
        if backend == DbBackend::Postgres {
            for function in [
                "store_guard_payment_transition",
                "store_guard_fulfillment_transition",
                "store_guard_quote_immutable",
                "store_guard_recovery_limit",
            ] {
                tx.execute(Statement::from_string(
                    backend,
                    format!("DROP FUNCTION IF EXISTS {function}()"),
                ))
                .await?;
            }
        }
        tx.commit().await
    }
}

const PAYMENT_TABLES: &[&str] = &[
    "store_channel_credentials",
    "store_payment_compliance",
    "store_merchant_capabilities",
    "store_payment_attempts",
    "store_provider_events",
    "store_order_event_applications",
    "store_refunds",
    "store_order_reward_recoveries",
    "store_order_recovery_claims",
    "store_settlement_reports",
    "store_settlement_lines",
    "store_redemption_limits",
    "store_redemption_attempts",
    "store_balance_holds",
    "store_reconciliation_leases",
    "store_reconciliation_cases",
    "store_privacy_records",
    "store_access_audits",
    "store_retention_runs",
    "store_legal_holds",
    "store_primary_leases",
    "store_quota_gates",
    "store_quota_buckets",
    "store_quota_reservations",
    "store_quota_reservation_buckets",
    "store_quota_violations",
    "store_quota_admission_blocks",
    "store_admission_keys",
    "store_admission_tokens",
    "store_admission_terminal_receipts",
];

async fn reject_unimported_legacy_secrets(
    manager: &SchemaManager<'_>,
    backend: DbBackend,
) -> Result<(), DbErr> {
    let row = manager
        .get_connection()
        .query_one(Statement::from_string(
            backend,
            "SELECT COUNT(*) AS value FROM store_payment_channels WHERE config_secret IS NOT NULL AND trim(config_secret) <> ''".to_string(),
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("legacy Channel secret preflight returned no row".to_string()))?;
    let count = i64::try_get(&row, "", "value")?;
    if count != 0 {
        return Err(DbErr::Custom(
            "store_legacy_secret_import_required".to_string(),
        ));
    }
    Ok(())
}

fn up_statements(backend: DbBackend) -> Vec<String> {
    let mut statements = match backend {
        DbBackend::Sqlite => sqlite_rebuild_statements(),
        DbBackend::Postgres => postgres_rebuild_statements(),
        _ => Vec::new(),
    };
    statements.extend(common_table_statements());
    statements.extend(common_index_statements());
    statements.extend(match backend {
        DbBackend::Sqlite => sqlite_trigger_statements(),
        DbBackend::Postgres => postgres_trigger_statements(),
        _ => Vec::new(),
    });
    statements
}

fn sqlite_rebuild_statements() -> Vec<String> {
    vec![
        "ALTER TABLE store_products ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0)".to_string(),
        sqlite_create_redemption_codes_v2(),
        "INSERT INTO store_redemption_codes_v2
            (id, code_format_version, code_digest, code_hint,
             encrypted_format_version, encrypted_key_id, encrypted_nonce_base64,
             encrypted_ciphertext_base64, ciphertext_destroyed_at,
             reward_kind, reward_json, status,
             expires_at, redeemed_by_user_id, redeemed_at, revoked_at,
             created_by_user_id, created_at)
         SELECT id, 1, code_digest, code_hint, NULL, NULL, NULL, NULL, NULL,
                reward_kind, reward_json, status, expires_at,
                redeemed_by_user_id, redeemed_at, NULL,
                created_by_user_id, created_at
         FROM store_redemption_codes"
            .to_string(),
        "DROP TABLE store_redemption_codes".to_string(),
        "ALTER TABLE store_redemption_codes_v2 RENAME TO store_redemption_codes".to_string(),
        "ALTER TABLE store_payment_channels RENAME TO store_payment_channels_legacy".to_string(),
        "CREATE TABLE store_payment_channels (
            id TEXT NOT NULL PRIMARY KEY,
            adapter_kind TEXT NOT NULL,
            name TEXT NOT NULL,
            icon_kind TEXT NOT NULL,
            icon_value TEXT,
            sort_order INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 0,
            revision INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            CONSTRAINT ck_store_payment_channels_adapter CHECK (adapter_kind IN ('alipay', 'wechat', 'stripe', 'http')),
            CONSTRAINT ck_store_payment_channels_enabled CHECK (enabled IN (0, 1)),
            CONSTRAINT ck_store_payment_channels_revision CHECK (revision > 0)
        )"
        .to_string(),
        "INSERT INTO store_payment_channels
            (id, adapter_kind, name, icon_kind, icon_value, sort_order, enabled, revision, created_at, updated_at)
         SELECT id, CASE kind WHEN 'custom' THEN 'http' ELSE kind END, name, icon_kind,
                icon_value, sort_order, 0, 1, created_at, updated_at
         FROM store_payment_channels_legacy"
            .to_string(),
        "INSERT INTO store_payment_channels
            (id, adapter_kind, name, icon_kind, icon_value, sort_order, enabled, revision, created_at, updated_at)
         VALUES ('store-channel-stripe', 'stripe', 'Stripe', 'builtin', 'stripe', 30, 0, 1,
                 '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z')
         ON CONFLICT (id) DO NOTHING"
            .to_string(),
        sqlite_create_orders_v2(),
        "INSERT INTO store_orders_v2
            (id, order_number, user_id, product_id, product_kind, payment_state,
             fulfillment_state, dispute_state, payment_hold, payment_channel_id,
             payment_currency, payment_minor, cny_per_usd, rate_numerator,
             rate_denominator, rate_source_updated_at, quote_json, contract_version,
             state_revision, creation_idempotency_key, creation_request_digest,
             expires_at, created_at, updated_at, paid_at,
             fulfillment_started_at, fulfilled_at, fulfillment_failed_at, closed_at,
             refund_pending_at, refunded_at)
         SELECT id, order_number, user_id, product_id, product_kind,
                CASE status WHEN 'completed' THEN 'paid' ELSE 'closed' END,
                CASE status WHEN 'completed' THEN 'fulfilled' ELSE 'pending' END,
                'none', 0, payment_channel_id, payment_currency, payment_minor,
                cny_per_usd, replace(cny_per_usd, '.', ''),
                CASE WHEN instr(cny_per_usd, '.') = 0 THEN '1'
                     ELSE '1' || substr('000000000000000000', 1, length(cny_per_usd) - instr(cny_per_usd, '.')) END,
                rate_source_updated_at, quote_json, 1, 0, NULL, NULL,
                datetime(created_at, '+30 minutes'), created_at, updated_at,
                completed_at, completed_at, completed_at, NULL,
                CASE WHEN status = 'completed' THEN NULL ELSE COALESCE(cancelled_at, updated_at) END,
                NULL, NULL
         FROM store_orders"
            .to_string(),
        "DROP TABLE store_orders".to_string(),
        "ALTER TABLE store_orders_v2 RENAME TO store_orders".to_string(),
        "DROP TABLE store_payment_channels_legacy".to_string(),
    ]
}

fn postgres_rebuild_statements() -> Vec<String> {
    vec![
        "ALTER TABLE store_products ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0)".to_string(),
        "ALTER TABLE store_redemption_codes RENAME TO store_redemption_codes_legacy".to_string(),
        postgres_create_redemption_codes_v2(),
        "INSERT INTO store_redemption_codes_v2
            (id, code_format_version, code_digest, code_hint,
             encrypted_format_version, encrypted_key_id, encrypted_nonce_base64,
             encrypted_ciphertext_base64, ciphertext_destroyed_at,
             reward_kind, reward_json, status,
             expires_at, redeemed_by_user_id, redeemed_at, revoked_at,
             created_by_user_id, created_at)
         SELECT id, 1, code_digest, code_hint, NULL, NULL, NULL, NULL, NULL,
                reward_kind, reward_json, status, expires_at,
                redeemed_by_user_id, redeemed_at, NULL,
                created_by_user_id, created_at
         FROM store_redemption_codes_legacy"
            .to_string(),
        "DROP TABLE store_redemption_codes_legacy CASCADE".to_string(),
        "ALTER TABLE store_redemption_codes_v2 RENAME TO store_redemption_codes".to_string(),
        "ALTER TABLE store_payment_channels RENAME TO store_payment_channels_legacy".to_string(),
        "CREATE TABLE store_payment_channels (
            id TEXT NOT NULL PRIMARY KEY,
            adapter_kind TEXT NOT NULL CHECK (adapter_kind IN ('alipay', 'wechat', 'stripe', 'http')),
            name TEXT NOT NULL,
            icon_kind TEXT NOT NULL,
            icon_value TEXT,
            sort_order INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
            revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )"
        .to_string(),
        "INSERT INTO store_payment_channels
            (id, adapter_kind, name, icon_kind, icon_value, sort_order, enabled, revision, created_at, updated_at)
         SELECT id, CASE kind WHEN 'custom' THEN 'http' ELSE kind END, name, icon_kind,
                icon_value, sort_order, 0, 1, created_at, updated_at
         FROM store_payment_channels_legacy"
            .to_string(),
        "INSERT INTO store_payment_channels
            (id, adapter_kind, name, icon_kind, icon_value, sort_order, enabled, revision, created_at, updated_at)
         VALUES ('store-channel-stripe', 'stripe', 'Stripe', 'builtin', 'stripe', 30, 0, 1,
                 '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z')
         ON CONFLICT (id) DO NOTHING"
            .to_string(),
        postgres_create_orders_v2(),
        "INSERT INTO store_orders_v2
            (id, order_number, user_id, product_id, product_kind, payment_state,
             fulfillment_state, dispute_state, payment_hold, payment_channel_id,
             payment_currency, payment_minor, cny_per_usd, rate_numerator,
             rate_denominator, rate_source_updated_at, quote_json, contract_version,
             state_revision, creation_idempotency_key, creation_request_digest,
             expires_at, created_at, updated_at, paid_at,
             fulfillment_started_at, fulfilled_at, fulfillment_failed_at, closed_at,
             refund_pending_at, refunded_at)
         SELECT id, order_number, user_id, product_id, product_kind,
                CASE status WHEN 'completed' THEN 'paid' ELSE 'closed' END,
                CASE status WHEN 'completed' THEN 'fulfilled' ELSE 'pending' END,
                'none', 0, payment_channel_id, payment_currency, payment_minor,
                cny_per_usd, replace(cny_per_usd, '.', ''),
                CASE WHEN strpos(cny_per_usd, '.') = 0 THEN '1'
                     ELSE '1' || repeat('0', length(cny_per_usd) - strpos(cny_per_usd, '.')) END,
                rate_source_updated_at, quote_json, 1, 0, NULL, NULL,
                ((created_at::timestamptz + interval '30 minutes')::text), created_at, updated_at,
                completed_at, completed_at, completed_at, NULL,
                CASE WHEN status = 'completed' THEN NULL ELSE COALESCE(cancelled_at, updated_at) END,
                NULL, NULL
         FROM store_orders"
            .to_string(),
        "DROP TABLE store_orders CASCADE".to_string(),
        "ALTER TABLE store_orders_v2 RENAME TO store_orders".to_string(),
        // Dropping the legacy channels table must happen after store_orders was
        // rebuilt: the old store_orders foreign key still references the
        // renamed legacy table, and PostgreSQL enforces the dependency.
        // CASCADE drops only the stale referencing constraints carried over by
        // the rename (fresh schema replay path; SQLite builds never hit this).
        "DROP TABLE store_payment_channels_legacy CASCADE".to_string(),
    ]
}

fn sqlite_create_orders_v2() -> String {
    create_orders_v2("CREATE TABLE store_orders_v2")
}

fn sqlite_create_redemption_codes_v2() -> String {
    create_redemption_codes_v2(
        "CREATE TABLE store_redemption_codes_v2",
        "length(code_digest) = 64 AND code_digest NOT GLOB '*[^0-9a-f]*'",
    )
}

fn postgres_create_redemption_codes_v2() -> String {
    create_redemption_codes_v2(
        "CREATE TABLE store_redemption_codes_v2",
        "code_digest ~ '^[0-9a-f]{64}$'",
    )
}

fn create_redemption_codes_v2(prefix: &str, digest_check: &str) -> String {
    format!(
        "{prefix} (
            id TEXT NOT NULL PRIMARY KEY,
            code_format_version INTEGER NOT NULL CHECK (code_format_version IN (1, 2)),
            code_digest TEXT NOT NULL,
            code_hint TEXT NOT NULL CHECK (length(code_hint) = 4),
            encrypted_format_version INTEGER,
            encrypted_key_id TEXT,
            encrypted_nonce_base64 TEXT,
            encrypted_ciphertext_base64 TEXT,
            ciphertext_destroyed_at TEXT,
            reward_kind TEXT NOT NULL CHECK (reward_kind IN ('balance', 'plan')),
            reward_json TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'unused'
                CHECK (status IN ('unused', 'used', 'revoked')),
            expires_at TEXT NOT NULL,
            redeemed_by_user_id TEXT,
            redeemed_at TEXT,
            revoked_at TEXT,
            created_by_user_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            CHECK ({digest_check}),
            CHECK (
                (code_format_version = 1
                 AND encrypted_format_version IS NULL
                 AND encrypted_key_id IS NULL
                 AND encrypted_nonce_base64 IS NULL
                 AND encrypted_ciphertext_base64 IS NULL
                 AND ciphertext_destroyed_at IS NULL)
                OR
                (code_format_version = 2 AND (
                    (status = 'unused'
                     AND encrypted_format_version IS NOT NULL
                     AND encrypted_key_id IS NOT NULL
                     AND encrypted_nonce_base64 IS NOT NULL
                     AND encrypted_ciphertext_base64 IS NOT NULL
                     AND ciphertext_destroyed_at IS NULL)
                    OR
                    (encrypted_format_version IS NULL
                     AND encrypted_key_id IS NULL
                     AND encrypted_nonce_base64 IS NULL
                     AND encrypted_ciphertext_base64 IS NULL
                     AND ciphertext_destroyed_at IS NOT NULL
                     AND (status IN ('used', 'revoked') OR status = 'unused'))
                ))
            ),
            CHECK (
                (status = 'unused' AND redeemed_by_user_id IS NULL
                 AND redeemed_at IS NULL AND revoked_at IS NULL)
                OR
                (status = 'used' AND redeemed_by_user_id IS NOT NULL
                 AND redeemed_at IS NOT NULL AND revoked_at IS NULL)
                OR
                (status = 'revoked' AND redeemed_by_user_id IS NULL
                 AND redeemed_at IS NULL AND revoked_at IS NOT NULL)
            )
        )"
    )
}

fn postgres_create_orders_v2() -> String {
    create_orders_v2("CREATE TABLE store_orders_v2")
}

fn create_orders_v2(prefix: &str) -> String {
    format!(
        "{prefix} (
            id TEXT NOT NULL PRIMARY KEY,
            order_number TEXT NOT NULL,
            user_id TEXT NOT NULL,
            product_id TEXT NOT NULL,
            product_kind TEXT NOT NULL CHECK (product_kind IN ('balance', 'plan')),
            payment_state TEXT NOT NULL CHECK (payment_state IN ('unpaid', 'paid', 'refund_pending', 'refunded', 'closed')),
            fulfillment_state TEXT NOT NULL CHECK (fulfillment_state IN ('pending', 'fulfilled', 'failed')),
            dispute_state TEXT NOT NULL DEFAULT 'none' CHECK (dispute_state IN ('none', 'open', 'won', 'lost')),
            payment_hold INTEGER NOT NULL DEFAULT 0 CHECK (payment_hold IN (0, 1)),
            payment_channel_id TEXT NOT NULL,
            payment_currency TEXT NOT NULL CHECK (payment_currency IN ('CNY', 'USD')),
            payment_minor TEXT NOT NULL,
            cny_per_usd TEXT NOT NULL,
            rate_numerator TEXT NOT NULL,
            rate_denominator TEXT NOT NULL,
            rate_source_updated_at TEXT NOT NULL,
            quote_json TEXT NOT NULL,
            contract_version INTEGER NOT NULL DEFAULT 2 CHECK (contract_version IN (1, 2)),
            state_revision INTEGER NOT NULL DEFAULT 0 CHECK (state_revision >= 0),
            creation_idempotency_key TEXT,
            creation_request_digest TEXT,
            expires_at TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            paid_at TEXT,
            fulfillment_started_at TEXT,
            fulfilled_at TEXT,
            fulfillment_failed_at TEXT,
            closed_at TEXT,
            refund_pending_at TEXT,
            refunded_at TEXT,
            FOREIGN KEY (product_id) REFERENCES store_products(id),
            FOREIGN KEY (payment_channel_id) REFERENCES store_payment_channels(id)
        )"
    )
}

fn common_table_statements() -> Vec<String> {
    vec![
        "CREATE TABLE store_channel_credentials (
            id TEXT NOT NULL PRIMARY KEY, channel_id TEXT NOT NULL, adapter_kind TEXT NOT NULL,
            format_version INTEGER NOT NULL, key_id TEXT NOT NULL, nonce_base64 TEXT NOT NULL,
            ciphertext_base64 TEXT NOT NULL, account_identity_digest TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('active', 'retired')),
            created_at TEXT NOT NULL, retired_at TEXT
        )",
        "CREATE TABLE store_payment_compliance (
            id TEXT NOT NULL PRIMARY KEY, channel_id TEXT NOT NULL, terms_version TEXT NOT NULL,
            admin_user_id TEXT NOT NULL, source_ip TEXT NOT NULL, confirmed_at TEXT NOT NULL,
            invalidated_at TEXT
        )",
        "CREATE TABLE store_merchant_capabilities (
            id TEXT NOT NULL PRIMARY KEY, channel_id TEXT NOT NULL, capability TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('supported', 'unsupported', 'manual')),
            environment TEXT NOT NULL, merchant_account_digest TEXT NOT NULL,
            provider_product TEXT NOT NULL, evidence_digest TEXT NOT NULL,
            controlled_transaction_id TEXT, verifier_admin_id TEXT NOT NULL,
            verified_at TEXT NOT NULL, expires_at TEXT NOT NULL
        )",
        "CREATE TABLE store_payment_attempts (
            id TEXT NOT NULL PRIMARY KEY, order_id TEXT NOT NULL, channel_id TEXT NOT NULL,
            adapter_kind TEXT NOT NULL, credential_version_id TEXT NOT NULL,
            merchant_account_identity TEXT NOT NULL, expected_payment_method TEXT,
            payment_contract_version INTEGER NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('created', 'presented', 'expired', 'failed', 'paid')),
            failure_kind TEXT CHECK (failure_kind IS NULL OR failure_kind IN ('configuration_unavailable', 'provider_rejected')),
            provider_transaction_id TEXT, provider_object_id TEXT, idempotency_key TEXT NOT NULL,
            action_kind TEXT CHECK (action_kind IS NULL OR action_kind IN ('redirect', 'qr', 'form')),
            action_json TEXT, provider_expires_at TEXT, presented_at TEXT, paid_at TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
            CHECK ((state = 'failed' AND failure_kind IS NOT NULL)
                OR (state <> 'failed' AND failure_kind IS NULL))
        )",
        "CREATE TABLE store_provider_events (
            id TEXT NOT NULL PRIMARY KEY, credential_version_id TEXT NOT NULL,
            provider_event_id TEXT NOT NULL, event_kind TEXT NOT NULL,
            provider_object_version TEXT, body_digest TEXT NOT NULL, parsed_json TEXT NOT NULL,
            verification_result TEXT NOT NULL, raw_format_version INTEGER,
            raw_key_id TEXT, raw_nonce_base64 TEXT, raw_ciphertext_base64 TEXT,
            source_ip TEXT, user_agent TEXT,
            projection_state TEXT NOT NULL CHECK (projection_state IN ('pending', 'applied', 'superseded', 'manual_review')),
            state_revision INTEGER NOT NULL DEFAULT 0, received_at TEXT NOT NULL, applied_at TEXT
        )",
        "CREATE TABLE store_order_event_applications (
            provider_event_row_id TEXT NOT NULL PRIMARY KEY, order_id TEXT NOT NULL,
            result TEXT NOT NULL, applied_at TEXT NOT NULL
        )",
        "CREATE TABLE store_refunds (
            id TEXT NOT NULL PRIMARY KEY, order_id TEXT NOT NULL, attempt_id TEXT NOT NULL,
            provider_refund_id TEXT, idempotency_key TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('created', 'pending', 'succeeded', 'failed')),
            amount_minor TEXT NOT NULL, currency TEXT NOT NULL CHECK (currency IN ('CNY', 'USD')),
            requested_by_admin_id TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
            resolved_at TEXT
        )",
        "CREATE TABLE store_order_reward_recoveries (
            id TEXT NOT NULL PRIMARY KEY, order_id TEXT NOT NULL,
            original_nano_usd TEXT NOT NULL, reserved_nano_usd TEXT NOT NULL DEFAULT '0',
            recovered_nano_usd TEXT NOT NULL DEFAULT '0', debit_ledger_key TEXT,
            release_ledger_key TEXT,
            state TEXT NOT NULL CHECK (state IN ('open', 'reserved', 'recovered', 'released')),
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        )",
        "CREATE TABLE store_order_recovery_claims (
            id TEXT NOT NULL PRIMARY KEY, recovery_id TEXT NOT NULL,
            credential_version_id TEXT NOT NULL, provider_claim_id TEXT NOT NULL,
            provider_event_row_id TEXT, kind TEXT NOT NULL CHECK (kind IN ('refund', 'dispute', 'chargeback')),
            amount_nano_usd TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('open', 'resolved', 'consumed')),
            created_at TEXT NOT NULL, resolved_at TEXT
        )",
        "CREATE TABLE store_settlement_reports (
            id TEXT NOT NULL PRIMARY KEY, channel_id TEXT NOT NULL,
            credential_version_id TEXT NOT NULL, provider_report_id TEXT NOT NULL,
            report_date TEXT NOT NULL, body_digest TEXT NOT NULL, imported_at TEXT NOT NULL
        )",
        "CREATE TABLE store_settlement_lines (
            id TEXT NOT NULL PRIMARY KEY, report_id TEXT NOT NULL,
            credential_version_id TEXT NOT NULL, provider_line_id TEXT NOT NULL,
            class TEXT NOT NULL CHECK (class IN ('gross', 'refund', 'dispute', 'fee',
                                                'tax', 'currency_conversion', 'net')),
            amount_minor TEXT NOT NULL,
            currency TEXT NOT NULL CHECK (currency IN ('CNY', 'USD')),
            provider_transaction_id TEXT, matched_order_id TEXT, created_at TEXT NOT NULL
        )",
        "CREATE TABLE store_redemption_limits (
            user_id TEXT NOT NULL, source_ip_digest TEXT NOT NULL,
            cooldown_until TEXT, updated_at TEXT NOT NULL,
            PRIMARY KEY (user_id, source_ip_digest)
        )",
        "CREATE TABLE store_redemption_attempts (
            id TEXT NOT NULL PRIMARY KEY, user_id TEXT NOT NULL,
            source_ip_digest TEXT NOT NULL,
            success INTEGER NOT NULL CHECK (success IN (0, 1)), attempted_at TEXT NOT NULL
        )",
        "CREATE TABLE store_balance_holds (
            user_id TEXT NOT NULL PRIMARY KEY, active INTEGER NOT NULL CHECK (active IN (0, 1)),
            reason TEXT NOT NULL, opened_at TEXT NOT NULL, cleared_at TEXT
        )",
        "CREATE TABLE store_reconciliation_leases (
            name TEXT NOT NULL PRIMARY KEY, owner_id TEXT NOT NULL, epoch INTEGER NOT NULL,
            expires_at TEXT NOT NULL, updated_at TEXT NOT NULL
        )",
        "CREATE TABLE store_reconciliation_cases (
            id TEXT NOT NULL PRIMARY KEY, order_id TEXT, channel_id TEXT,
            severity TEXT NOT NULL, kind TEXT NOT NULL, state TEXT NOT NULL,
            owner_admin_id TEXT, provider_deadline TEXT, internal_deadline TEXT,
            evidence_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
            closed_at TEXT
        )",
        "CREATE TABLE store_privacy_records (
            id TEXT NOT NULL PRIMARY KEY, policy_version TEXT NOT NULL, jurisdiction TEXT NOT NULL,
            allowed_regions_json TEXT NOT NULL, retention_json TEXT NOT NULL, legal_basis TEXT NOT NULL,
            reviewer_id TEXT NOT NULL, evidence_digest TEXT NOT NULL, approved_at TEXT NOT NULL,
            next_review_at TEXT NOT NULL, accepted INTEGER NOT NULL CHECK (accepted IN (0, 1))
        )",
        "CREATE TABLE store_access_audits (
            id TEXT NOT NULL PRIMARY KEY, actor_id TEXT NOT NULL, actor_role TEXT NOT NULL,
            action TEXT NOT NULL, scope_json TEXT NOT NULL, reason TEXT NOT NULL,
            result TEXT NOT NULL, created_at TEXT NOT NULL
        )",
        "CREATE TABLE store_retention_runs (
            id TEXT NOT NULL PRIMARY KEY, policy_version TEXT NOT NULL, counts_json TEXT NOT NULL,
            oldest_remaining_at TEXT, state TEXT NOT NULL, error_category TEXT,
            started_at TEXT NOT NULL, completed_at TEXT
        )",
        "CREATE TABLE store_legal_holds (
            id TEXT NOT NULL PRIMARY KEY, data_class TEXT NOT NULL, identifiers_json TEXT NOT NULL,
            reason TEXT NOT NULL, requesting_authority TEXT NOT NULL, approver_id TEXT NOT NULL,
            starts_at TEXT NOT NULL, expires_at TEXT NOT NULL, created_at TEXT NOT NULL
        )",
        "CREATE TABLE store_primary_leases (
            name TEXT NOT NULL PRIMARY KEY, owner_id TEXT NOT NULL, epoch INTEGER NOT NULL,
            expires_at TEXT NOT NULL, updated_at TEXT NOT NULL
        )",
        "CREATE TABLE store_quota_gates (
            backend TEXT NOT NULL, slot TEXT NOT NULL CHECK (slot IN ('current', 'next')),
            state TEXT NOT NULL CHECK (state IN ('pending', 'passed', 'failed')),
            compatibility_fingerprint TEXT NOT NULL, manifest_json TEXT NOT NULL,
            tested_at TEXT, failure_reason TEXT, updated_at TEXT NOT NULL,
            PRIMARY KEY (backend, slot)
        )",
        "CREATE TABLE store_quota_buckets (
            id TEXT NOT NULL PRIMARY KEY, entitlement_id TEXT NOT NULL, generation INTEGER NOT NULL,
            quota_rule_id TEXT NOT NULL, window_kind TEXT NOT NULL,
            window_start TEXT NOT NULL, window_end TEXT NOT NULL,
            settled_fen_cny TEXT NOT NULL DEFAULT '0', reserved_fen_cny TEXT NOT NULL DEFAULT '0',
            quota_fen_cny TEXT NOT NULL, updated_at TEXT NOT NULL,
            FOREIGN KEY (entitlement_id, generation)
                REFERENCES store_plan_entitlement_generations (id, generation) ON DELETE RESTRICT
        )",
        "CREATE TABLE store_quota_reservations (
            id TEXT NOT NULL PRIMARY KEY, request_id TEXT NOT NULL, entitlement_id TEXT NOT NULL,
            generation INTEGER NOT NULL, user_id TEXT NOT NULL,
            maximum_nano_usd TEXT NOT NULL, reserved_fen_cny TEXT NOT NULL,
            rate_numerator TEXT NOT NULL, rate_denominator TEXT NOT NULL,
            pricing_revision TEXT NOT NULL, actual_nano_usd TEXT, actual_fen_cny TEXT,
            state TEXT NOT NULL CHECK (state IN ('reserved', 'settled', 'released', 'violated')),
            admitted_at TEXT NOT NULL, terminal_at TEXT, updated_at TEXT NOT NULL,
            FOREIGN KEY (entitlement_id, user_id, generation)
                REFERENCES store_plan_entitlement_generations (id, user_id, generation) ON DELETE RESTRICT
        )",
        "CREATE TABLE store_quota_reservation_buckets (
            reservation_id TEXT NOT NULL, bucket_id TEXT NOT NULL,
            reserved_fen_cny TEXT NOT NULL,
            PRIMARY KEY (reservation_id, bucket_id),
            FOREIGN KEY (reservation_id) REFERENCES store_quota_reservations (id) ON DELETE RESTRICT,
            FOREIGN KEY (bucket_id) REFERENCES store_quota_buckets (id) ON DELETE RESTRICT
        )",
        "CREATE TABLE store_quota_violations (
            id TEXT NOT NULL PRIMARY KEY, reservation_id TEXT NOT NULL UNIQUE,
            request_id TEXT NOT NULL, user_id TEXT NOT NULL, entitlement_id TEXT NOT NULL,
            generation INTEGER NOT NULL, reserved_fen_cny TEXT NOT NULL,
            actual_fen_cny TEXT NOT NULL, severity TEXT NOT NULL CHECK (severity = 'critical'),
            detected_at TEXT NOT NULL,
            FOREIGN KEY (reservation_id) REFERENCES store_quota_reservations (id) ON DELETE RESTRICT,
            FOREIGN KEY (entitlement_id, generation)
                REFERENCES store_plan_entitlement_generations (id, generation) ON DELETE RESTRICT
        )",
        "CREATE TABLE store_quota_admission_blocks (
            user_id TEXT NOT NULL PRIMARY KEY, violation_id TEXT NOT NULL UNIQUE,
            entitlement_id TEXT NOT NULL, generation INTEGER NOT NULL,
            reason TEXT NOT NULL CHECK (reason = 'above_reserve'),
            blocked_at TEXT NOT NULL, cleared_at TEXT,
            FOREIGN KEY (violation_id) REFERENCES store_quota_violations (id) ON DELETE RESTRICT,
            FOREIGN KEY (entitlement_id, generation)
                REFERENCES store_plan_entitlement_generations (id, generation) ON DELETE RESTRICT
        )",
        "CREATE TABLE store_admission_keys (
            key_id TEXT NOT NULL PRIMARY KEY, public_key_base64 TEXT NOT NULL,
            encrypted_private_key_json TEXT, state TEXT NOT NULL CHECK (state IN ('published', 'active', 'retired')),
            published_at TEXT NOT NULL, activated_at TEXT, retired_at TEXT,
            last_issued_expires_at TEXT, verify_until TEXT, config_epoch INTEGER NOT NULL DEFAULT 0,
            CHECK (config_epoch >= 0),
            CHECK (
                (state = 'published' AND encrypted_private_key_json IS NULL
                    AND activated_at IS NULL AND retired_at IS NULL
                    AND last_issued_expires_at IS NULL AND verify_until IS NULL) OR
                (state = 'active' AND encrypted_private_key_json IS NOT NULL
                    AND activated_at IS NOT NULL AND retired_at IS NULL AND verify_until IS NULL) OR
                (state = 'retired' AND encrypted_private_key_json IS NOT NULL
                    AND activated_at IS NOT NULL AND retired_at IS NOT NULL
                    AND verify_until IS NOT NULL)
            )
        )",
        "CREATE TABLE store_admission_tokens (
            token_id TEXT NOT NULL PRIMARY KEY, audience TEXT NOT NULL, request_id TEXT NOT NULL,
            user_id TEXT NOT NULL, effective_groups_json TEXT NOT NULL,
            reservation_id TEXT NOT NULL, entitlement_id TEXT NOT NULL, generation INTEGER NOT NULL,
            maximum_nano_usd TEXT NOT NULL, reserved_fen_cny TEXT NOT NULL,
            pricing_revision TEXT NOT NULL, key_id TEXT NOT NULL,
            compact_jws TEXT NOT NULL, compact_jws_digest TEXT NOT NULL,
            issued_at TEXT NOT NULL, expires_at TEXT NOT NULL,
            expires_at_unix INTEGER NOT NULL, confirmed_at TEXT,
            FOREIGN KEY (reservation_id) REFERENCES store_quota_reservations (id) ON DELETE RESTRICT,
            FOREIGN KEY (key_id) REFERENCES store_admission_keys (key_id) ON DELETE RESTRICT
        )",
        "CREATE TABLE store_admission_terminal_receipts (
            token_id TEXT NOT NULL PRIMARY KEY, reservation_id TEXT NOT NULL,
            request_id TEXT NOT NULL, audience TEXT NOT NULL,
            terminal_kind TEXT NOT NULL CHECK (terminal_kind IN ('settlement', 'release')),
            actual_nano_usd TEXT, canonical_digest TEXT NOT NULL, applied_at TEXT NOT NULL,
            CHECK ((terminal_kind = 'settlement' AND actual_nano_usd IS NOT NULL) OR
                   (terminal_kind = 'release' AND actual_nano_usd IS NULL)),
            FOREIGN KEY (token_id) REFERENCES store_admission_tokens (token_id) ON DELETE RESTRICT,
            FOREIGN KEY (reservation_id) REFERENCES store_quota_reservations (id) ON DELETE RESTRICT
        )",
        "INSERT INTO store_quota_gates
            (backend, slot, state, compatibility_fingerprint, manifest_json, tested_at,
             failure_reason, updated_at)
         VALUES ('sqlite', 'current', 'pending', '', '{}', NULL, NULL,
                 '2026-08-27T00:00:00Z')
         ON CONFLICT (backend, slot) DO NOTHING",
        "INSERT INTO store_quota_gates
            (backend, slot, state, compatibility_fingerprint, manifest_json, tested_at,
             failure_reason, updated_at)
         VALUES ('sqlite', 'next', 'pending', '', '{}', NULL, NULL,
                 '2026-08-27T00:00:00Z')
         ON CONFLICT (backend, slot) DO NOTHING",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn common_index_statements() -> Vec<String> {
    [
        "CREATE UNIQUE INDEX uq_store_orders_order_number_v2 ON store_orders (order_number)",
        "CREATE INDEX idx_store_orders_user_created_v2 ON store_orders (user_id, created_at DESC, id DESC)",
        "CREATE INDEX idx_store_orders_reconcile ON store_orders (payment_state, fulfillment_state, updated_at, id)",
        "CREATE INDEX idx_store_attempt_order_candidates ON store_payment_attempts (order_id, channel_id, adapter_kind, created_at DESC, id DESC)",
        "CREATE UNIQUE INDEX uq_store_orders_creation_idempotency ON store_orders (user_id, creation_idempotency_key)",
        "CREATE UNIQUE INDEX uq_store_credentials_channel_active ON store_channel_credentials (channel_id, id)",
        "CREATE UNIQUE INDEX uq_store_capability_channel_kind ON store_merchant_capabilities (channel_id, capability)",
        "CREATE UNIQUE INDEX uq_store_attempt_idempotency ON store_payment_attempts (idempotency_key)",
        "CREATE UNIQUE INDEX uq_store_attempt_provider_transaction ON store_payment_attempts (credential_version_id, provider_transaction_id)",
        "CREATE UNIQUE INDEX uq_store_attempt_active_order ON store_payment_attempts (order_id) WHERE state IN ('created', 'presented')",
        "CREATE UNIQUE INDEX uq_store_provider_event_identity ON store_provider_events (credential_version_id, provider_event_id)",
        "CREATE UNIQUE INDEX uq_store_refund_idempotency ON store_refunds (idempotency_key)",
        "CREATE UNIQUE INDEX uq_store_recovery_order ON store_order_reward_recoveries (order_id)",
        "CREATE UNIQUE INDEX uq_store_recovery_claim_provider ON store_order_recovery_claims (credential_version_id, provider_claim_id, kind)",
        "CREATE UNIQUE INDEX uq_store_recovery_claim_event ON store_order_recovery_claims (provider_event_row_id, kind)",
        "CREATE UNIQUE INDEX uq_store_settlement_report_provider ON store_settlement_reports (credential_version_id, provider_report_id)",
        "CREATE UNIQUE INDEX uq_store_settlement_line_provider ON store_settlement_lines (credential_version_id, provider_line_id)",
        "CREATE UNIQUE INDEX uq_store_redemption_codes_digest_v2 ON store_redemption_codes (code_digest)",
        "CREATE INDEX idx_store_redemption_codes_status_expires_v2 ON store_redemption_codes (status, expires_at, id)",
        "CREATE INDEX idx_store_redemption_attempts_limit ON store_redemption_attempts (user_id, source_ip_digest, attempted_at)",
        "CREATE UNIQUE INDEX uq_store_quota_request ON store_quota_reservations (request_id)",
        "CREATE UNIQUE INDEX uq_store_quota_bucket_window ON store_quota_buckets
            (entitlement_id, generation, quota_rule_id, window_start, window_end)",
        "CREATE INDEX idx_store_quota_bucket_active ON store_quota_buckets
            (entitlement_id, generation, quota_rule_id, window_end, id)",
        "CREATE UNIQUE INDEX uq_store_admission_active_key ON store_admission_keys (state)
            WHERE state = 'active'",
        "CREATE UNIQUE INDEX uq_store_admission_token_request ON store_admission_tokens (audience, request_id)",
        "CREATE UNIQUE INDEX uq_store_admission_token_reservation ON store_admission_tokens (reservation_id)",
        "CREATE UNIQUE INDEX uq_store_admission_token_digest ON store_admission_tokens (compact_jws_digest)",
        "CREATE INDEX idx_store_admission_unconfirmed_expiry ON store_admission_tokens
            (confirmed_at, expires_at_unix, token_id)",
        "CREATE INDEX idx_store_provider_events_projection ON store_provider_events (projection_state, received_at, id)",
        "CREATE INDEX idx_store_refunds_state ON store_refunds (state, updated_at, id)",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn sqlite_trigger_statements() -> Vec<String> {
    vec![
        "CREATE TRIGGER trg_store_orders_payment_transition
         BEFORE UPDATE OF payment_state ON store_orders
         WHEN NOT (
             OLD.payment_state = NEW.payment_state OR
             (OLD.payment_state = 'unpaid' AND NEW.payment_state IN ('paid', 'closed')) OR
             (OLD.payment_state = 'closed' AND NEW.payment_state = 'paid' AND OLD.contract_version = 2) OR
             (OLD.payment_state = 'paid' AND NEW.payment_state = 'refund_pending') OR
             (OLD.payment_state = 'refund_pending' AND NEW.payment_state IN ('refunded', 'paid'))
         ) BEGIN SELECT RAISE(ABORT, 'invalid store payment transition'); END"
            .to_string(),
        "CREATE TRIGGER trg_store_orders_fulfillment_transition
         BEFORE UPDATE OF fulfillment_state ON store_orders
         WHEN NOT (
             OLD.fulfillment_state = NEW.fulfillment_state OR
             (OLD.fulfillment_state = 'pending' AND NEW.fulfillment_state IN ('fulfilled', 'failed')) OR
             (OLD.fulfillment_state = 'failed' AND NEW.fulfillment_state = 'fulfilled')
         ) BEGIN SELECT RAISE(ABORT, 'invalid store fulfillment transition'); END"
            .to_string(),
        "CREATE TRIGGER trg_store_orders_quote_immutable
         BEFORE UPDATE ON store_orders
         WHEN OLD.product_id IS NOT NEW.product_id
           OR OLD.product_kind IS NOT NEW.product_kind
           OR OLD.payment_channel_id IS NOT NEW.payment_channel_id
           OR OLD.payment_currency IS NOT NEW.payment_currency
           OR OLD.payment_minor IS NOT NEW.payment_minor
           OR OLD.cny_per_usd IS NOT NEW.cny_per_usd
           OR OLD.rate_numerator IS NOT NEW.rate_numerator
           OR OLD.rate_denominator IS NOT NEW.rate_denominator
           OR OLD.rate_source_updated_at IS NOT NEW.rate_source_updated_at
           OR OLD.quote_json IS NOT NEW.quote_json
           OR OLD.contract_version IS NOT NEW.contract_version
         BEGIN SELECT RAISE(ABORT, 'immutable store order quote'); END"
            .to_string(),
        sqlite_recovery_trigger("INSERT", "trg_store_recovery_insert_limit"),
        sqlite_recovery_trigger("UPDATE", "trg_store_recovery_update_limit"),
    ]
}

fn sqlite_recovery_trigger(operation: &str, name: &str) -> String {
    // Full refunds use either a reserve or a recovered value, never both. This stronger
    // invariant makes the arbitrary-precision text comparison exact in SQLite.
    format!(
        "CREATE TRIGGER {name}
         BEFORE {operation} ON store_order_reward_recoveries
         WHEN NEW.original_nano_usd = '' OR NEW.original_nano_usd GLOB '*[^0-9]*'
           OR NEW.reserved_nano_usd = '' OR NEW.reserved_nano_usd GLOB '*[^0-9]*'
           OR NEW.recovered_nano_usd = '' OR NEW.recovered_nano_usd GLOB '*[^0-9]*'
           OR (NEW.reserved_nano_usd <> '0' AND NEW.recovered_nano_usd <> '0')
           OR length(NEW.reserved_nano_usd) > length(NEW.original_nano_usd)
           OR (length(NEW.reserved_nano_usd) = length(NEW.original_nano_usd)
               AND NEW.reserved_nano_usd > NEW.original_nano_usd)
           OR length(NEW.recovered_nano_usd) > length(NEW.original_nano_usd)
           OR (length(NEW.recovered_nano_usd) = length(NEW.original_nano_usd)
               AND NEW.recovered_nano_usd > NEW.original_nano_usd)
         BEGIN SELECT RAISE(ABORT, 'store recovery exceeds original reward'); END"
    )
}

fn postgres_trigger_statements() -> Vec<String> {
    vec![
        "CREATE FUNCTION store_guard_payment_transition() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NOT (
             OLD.payment_state = NEW.payment_state OR
             (OLD.payment_state = 'unpaid' AND NEW.payment_state IN ('paid', 'closed')) OR
             (OLD.payment_state = 'closed' AND NEW.payment_state = 'paid' AND OLD.contract_version = 2) OR
             (OLD.payment_state = 'paid' AND NEW.payment_state = 'refund_pending') OR
             (OLD.payment_state = 'refund_pending' AND NEW.payment_state IN ('refunded', 'paid'))
           ) THEN RAISE EXCEPTION 'invalid store payment transition'; END IF;
           RETURN NEW;
         END $$"
            .to_string(),
        "CREATE TRIGGER trg_store_orders_payment_transition BEFORE UPDATE OF payment_state ON store_orders FOR EACH ROW EXECUTE FUNCTION store_guard_payment_transition()".to_string(),
        "CREATE FUNCTION store_guard_fulfillment_transition() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NOT (
             OLD.fulfillment_state = NEW.fulfillment_state OR
             (OLD.fulfillment_state = 'pending' AND NEW.fulfillment_state IN ('fulfilled', 'failed')) OR
             (OLD.fulfillment_state = 'failed' AND NEW.fulfillment_state = 'fulfilled')
           ) THEN RAISE EXCEPTION 'invalid store fulfillment transition'; END IF;
           RETURN NEW;
         END $$"
            .to_string(),
        "CREATE TRIGGER trg_store_orders_fulfillment_transition BEFORE UPDATE OF fulfillment_state ON store_orders FOR EACH ROW EXECUTE FUNCTION store_guard_fulfillment_transition()".to_string(),
        "CREATE FUNCTION store_guard_quote_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF ROW(OLD.product_id, OLD.product_kind, OLD.payment_channel_id, OLD.payment_currency,
                  OLD.payment_minor, OLD.cny_per_usd, OLD.rate_numerator, OLD.rate_denominator,
                  OLD.rate_source_updated_at, OLD.quote_json, OLD.contract_version)
              IS DISTINCT FROM
              ROW(NEW.product_id, NEW.product_kind, NEW.payment_channel_id, NEW.payment_currency,
                  NEW.payment_minor, NEW.cny_per_usd, NEW.rate_numerator, NEW.rate_denominator,
                  NEW.rate_source_updated_at, NEW.quote_json, NEW.contract_version)
           THEN RAISE EXCEPTION 'immutable store order quote'; END IF;
           RETURN NEW;
         END $$"
            .to_string(),
        "CREATE TRIGGER trg_store_orders_quote_immutable BEFORE UPDATE ON store_orders FOR EACH ROW EXECUTE FUNCTION store_guard_quote_immutable()".to_string(),
        "CREATE FUNCTION store_guard_recovery_limit() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.original_nano_usd !~ '^(0|[1-9][0-9]*)$'
              OR NEW.reserved_nano_usd !~ '^(0|[1-9][0-9]*)$'
              OR NEW.recovered_nano_usd !~ '^(0|[1-9][0-9]*)$'
              OR NEW.reserved_nano_usd::numeric + NEW.recovered_nano_usd::numeric > NEW.original_nano_usd::numeric
           THEN RAISE EXCEPTION 'store recovery exceeds original reward'; END IF;
           RETURN NEW;
         END $$"
            .to_string(),
        "CREATE TRIGGER trg_store_recovery_insert_limit BEFORE INSERT ON store_order_reward_recoveries FOR EACH ROW EXECUTE FUNCTION store_guard_recovery_limit()".to_string(),
        "CREATE TRIGGER trg_store_recovery_update_limit BEFORE UPDATE ON store_order_reward_recoveries FOR EACH ROW EXECUTE FUNCTION store_guard_recovery_limit()".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::up_statements;
    use sea_orm::DbBackend;

    #[test]
    fn postgres_statements_define_all_core_guards() {
        let sql = up_statements(DbBackend::Postgres).join("\n");
        for required in [
            "store_guard_payment_transition",
            "store_guard_fulfillment_transition",
            "store_guard_quote_immutable",
            "store_guard_recovery_limit",
            "store-channel-stripe",
            "store_payment_attempts",
            "store_admission_tokens",
            "store_admission_terminal_receipts",
            "uq_store_admission_token_request",
        ] {
            assert!(
                sql.contains(required),
                "missing PostgreSQL SQL for {required}"
            );
        }
    }
}

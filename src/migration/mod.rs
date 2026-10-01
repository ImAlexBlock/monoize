use sea_orm_migration::prelude::*;
use std::collections::BTreeSet;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250101_000001_create_tables::Migration),
            Box::new(m20260229_000002_pg_request_logs_native_shadow::Migration),
            Box::new(m20260307_000003_drop_pg_request_logs_shadow::Migration),
            Box::new(m20260314_000004_request_log_retention_indexes::Migration),
            Box::new(m20260322_000005_retry_breaker_refactor::Migration),
            Box::new(m20260322_000006_provider_retry_interval_and_breaker_toggle::Migration),
            Box::new(m20260323_000007_channel_group_system::Migration),
            Box::new(m20260326_000008_provider_extra_fields_whitelist::Migration),
            Box::new(m20260326_000009_move_groups_to_providers::Migration),
            Box::new(m20260327_000010_api_key_model_redirects::Migration),
            Box::new(m20260328_000011_api_key_sub_account_billing::Migration),
            Box::new(m20260402_000012_provider_strip_cross_protocol_nested_extra::Migration),
            Box::new(m20260403_000013_drop_orphan_channel_override_columns::Migration),
            Box::new(m20260404_000014_api_key_reasoning_envelope_switch::Migration),
            Box::new(m20260501_000015_api_key_request_capture_switch::Migration),
            Box::new(m20260509_000016_api_key_request_capture_mode::Migration),
            Box::new(m20260618_000017_channel_model_affinity_schema::Migration),
            Box::new(m20260619_000018_request_log_visible_tps::Migration),
            Box::new(m20260619_000019_billing_rate_records::Migration),
            Box::new(m20260619_000020_default_pricing_profile::Migration),
            Box::new(m20260620_000021_pricing_profile_pattern_defaults::Migration),
            Box::new(m20260718_000022_move_models_to_channels::Migration),
            Box::new(m20260718_000023_channel_model_multiplier_float8::Migration),
            Box::new(m20260729_000024_channel_affinity_overrides::Migration),
            Box::new(m20260809_000025_storage_ledger_integrity::Migration),
            Box::new(m20260809_000026_exact_multiplier_text::Migration),
            Box::new(m20260809_000027_request_log_legacy_time_index::Migration),
            Box::new(m20260809_000028_channel_model_name_index::Migration),
            Box::new(m20260809_000029_sessions_expires_at_index::Migration),
            Box::new(m20260809_000030_normalize_billing_json_nulls::Migration),
            Box::new(m20260809_000031_request_logs_without_user_fk::Migration),
            Box::new(m20260823_000032_billing_plan_subscriptions::Migration),
            Box::new(m20260823_000033_billing_ledger_delta_dedupe::Migration),
            Box::new(m20260823_000034_channel_egress_proxy::Migration),
            Box::new(m20260823_000035_channel_extra_headers::Migration),
            Box::new(m20260823_000036_channel_session_affinity_auto::Migration),
            Box::new(m20260823_000037_billing_plan_cron_schedule::Migration),
            Box::new(m20260824_000038_request_log_session_affinity::Migration),
            Box::new(m20260824_000039_api_keys_plaintext_only::Migration),
            Box::new(m20260824_000040_drop_request_log_visible_tps::Migration),
            Box::new(m20260824_000041_request_capture_records::Migration),
            Box::new(m20260825_000042_groups_registry::Migration),
            Box::new(m20260825_000043_channel_allow_missing_usage::Migration),
            Box::new(m20260826_000048_provider_pricing_flatten::Migration),
            Box::new(m20260827_000049_store_billing::Migration),
            Box::new(m20260827_000050_store_payment_icons::Migration),
            Box::new(m20260827_000051_store_payment_core::Migration),
            Box::new(m20260827_000052_store_reauth_and_credential_guard::Migration),
            Box::new(m20260827_000053_store_fulfillment_retries::Migration),
            Box::new(m20260828_000054_store_channel_readiness::Migration),
            Box::new(m20260828_000055_store_refund_reauth::Migration),
            Box::new(m20260828_000056_store_reprocess_reauth::Migration),
            Box::new(m20260828_000057_store_refund_query_retries::Migration),
            Box::new(m20260828_000058_store_retention_runtime::Migration),
            Box::new(m20260829_000059_store_released_schema_repair::Migration),
            Box::new(m20260829_000060_api_key_channel_bindings::Migration),
            Box::new(m20260830_000061_usage_ranking_privacy::Migration),
            Box::new(m20260904_000062_group_visibility::Migration),
            Box::new(m20260908_000063_enterprise_account_class::Migration),
            Box::new(m20260908_000064_epay_adapter::Migration),
            Box::new(m20260909_000065_readiness_optional_evidence::Migration),
            Box::new(m20260909_000066_billing_rate_currency::Migration),
            Box::new(m20260909_000067_model_metadata_price_currency::Migration),
            Box::new(m20260909_000068_sales_commission::Migration),
            Box::new(m20260909_000069_sales_withdrawal_cancelled::Migration),
            Box::new(m20260909_000070_attempt_buyer_query_at::Migration),
            Box::new(m20260909_000071_private_account_class::Migration),
            Box::new(m20260910_000072_billing_ledger_user_created_index::Migration),
            Box::new(m20260912_000073_agent_account_class::Migration),
            Box::new(m20260913_000074_user_sub_accounts::Migration),
            Box::new(m20260913_000075_org_spaces::Migration),
            Box::new(m20260914_000076_org_admin_controls::Migration),
            Box::new(m20260914_000077_request_log_usage_breakdown_slim::Migration),
            Box::new(m20260915_000078_firewall_events::Migration),
            Box::new(m20260915_000079_firewall_event_action::Migration),
            Box::new(m20260915_000080_firewall_event_reason::Migration),
            Box::new(m20260915_000081_billing_rate_peak_price::Migration),
            Box::new(m20260915_000082_api_key_model_bindings::Migration),
            Box::new(m20260915_000083_org_key_ownership::Migration),
            Box::new(m20260915_000101_billing_ledger_account_scope::Migration),
            Box::new(m20260915_000102_session_token_hash::Migration),
            Box::new(m20260916_000103_admin_revenue_daily::Migration),
            Box::new(m20260916_000104_normalize_quota_day::Migration),
            Box::new(m20260916_000105_provider_cache_routing_guards::Migration),
            Box::new(m20260916_000106_admin_revenue_user_rows::Migration),
            Box::new(m20260917_000107_admin_revenue_user_model_rows::Migration),
            Box::new(m20260918_000108_announcements::Migration),
            Box::new(m20260919_000109_org_usage_limits::Migration),
            Box::new(m20260921_000110_api_key_daily_limit::Migration),
            Box::new(m20260921_000120_drop_api_key_daily_limit::Migration),
            Box::new(m20260922_000121_studio_workflow::Migration),
            Box::new(m20260922_000122_studio_bridge_ops::Migration),
            Box::new(m20260922_000123_drop_studio_workflow::Migration),
            Box::new(m20260922_000124_org_member_alias::Migration),
            Box::new(m20260924_000125_request_log_upstream_response_model::Migration),
            Box::new(m20261001_000126_firewall_timestamp_bigint::Migration),
        ]
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StartupMigrationDecision {
    RunEmbedded,
    FullyApplied,
    AcceptNewerApplied {
        newest_embedded: String,
        newer_applied: Vec<String>,
    },
}

fn startup_migration_decision(
    embedded: &[String],
    applied: &[String],
) -> Result<StartupMigrationDecision, DbErr> {
    if embedded.is_empty() {
        return Err(DbErr::Custom(
            "embedded migration list must not be empty".to_string(),
        ));
    }
    for versions in embedded.windows(2) {
        if versions[0] >= versions[1] {
            return Err(DbErr::Custom(format!(
                "embedded migration versions must be strictly ordered: '{}' is not before '{}'",
                versions[0], versions[1]
            )));
        }
    }

    let embedded_set = embedded.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let applied_set = applied.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let newer_applied = applied_set
        .difference(&embedded_set)
        .map(|version| (*version).to_string())
        .collect::<Vec<_>>();
    if newer_applied.is_empty() {
        let missing_embedded = embedded_set.difference(&applied_set).count();
        if missing_embedded == 0 {
            return Ok(StartupMigrationDecision::FullyApplied);
        }
        return Ok(StartupMigrationDecision::RunEmbedded);
    }

    let missing_embedded = embedded_set
        .difference(&applied_set)
        .map(|version| (*version).to_string())
        .collect::<Vec<_>>();
    let newest_embedded = embedded
        .last()
        .expect("non-empty embedded migration list")
        .clone();
    let non_later_applied = newer_applied
        .iter()
        .filter(|version| version.as_str() <= newest_embedded.as_str())
        .cloned()
        .collect::<Vec<_>>();

    if !missing_embedded.is_empty() || !non_later_applied.is_empty() {
        return Err(DbErr::Custom(format!(
            "database migration history is incompatible with this binary: missing embedded versions [{}]; non-embedded versions not later than '{}' [{}]",
            missing_embedded.join(", "),
            newest_embedded,
            non_later_applied.join(", ")
        )));
    }

    Ok(StartupMigrationDecision::AcceptNewerApplied {
        newest_embedded,
        newer_applied,
    })
}

/// PRP10 (`primary-replica-deployment.spec.md`): read-only schema currency check for
/// replicas. Returns Err only when embedded migrations are still pending (the database
/// must first be migrated by the primary); never writes.
pub async fn verify_schema_current(db: &sea_orm::DatabaseConnection) -> Result<(), DbErr> {
    let embedded = Migrator::migrations()
        .into_iter()
        .map(|migration| migration.name().to_string())
        .collect::<Vec<_>>();
    let applied = Migrator::get_migration_models(db)
        .await?
        .into_iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();
    match startup_migration_decision(&embedded, &applied)? {
        StartupMigrationDecision::RunEmbedded => {
            Err(DbErr::Custom("replica_schema_pending".to_string()))
        }
        StartupMigrationDecision::FullyApplied => Ok(()),
        StartupMigrationDecision::AcceptNewerApplied {
            newest_embedded,
            newer_applied,
        } => {
            tracing::warn!(
                newest_embedded_version = %newest_embedded,
                newer_applied_versions = ?newer_applied,
                "replica accepting strictly newer applied migrations (rollback binary)"
            );
            Ok(())
        }
    }
}

pub async fn run_startup_migrations(db: &sea_orm::DatabaseConnection) -> Result<(), DbErr> {
    let embedded = Migrator::migrations()
        .into_iter()
        .map(|migration| migration.name().to_string())
        .collect::<Vec<_>>();
    let applied = Migrator::get_migration_models(db)
        .await?
        .into_iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();

    match startup_migration_decision(&embedded, &applied)? {
        StartupMigrationDecision::RunEmbedded | StartupMigrationDecision::FullyApplied => {
            Migrator::up(db, None).await
        }
        StartupMigrationDecision::AcceptNewerApplied {
            newest_embedded,
            newer_applied,
        } => {
            tracing::warn!(
                newest_embedded_version = %newest_embedded,
                newer_applied_versions = ?newer_applied,
                "skipping startup migration because the database contains only strictly newer applied migrations"
            );
            Ok(())
        }
    }
}

mod m20250101_000001_create_tables;
mod m20260229_000002_pg_request_logs_native_shadow;
mod m20260307_000003_drop_pg_request_logs_shadow;
mod m20260314_000004_request_log_retention_indexes;
mod m20260322_000005_retry_breaker_refactor;
mod m20260322_000006_provider_retry_interval_and_breaker_toggle;
mod m20260323_000007_channel_group_system;
mod m20260326_000008_provider_extra_fields_whitelist;
mod m20260326_000009_move_groups_to_providers;
mod m20260327_000010_api_key_model_redirects;
mod m20260328_000011_api_key_sub_account_billing;
mod m20260402_000012_provider_strip_cross_protocol_nested_extra;
mod m20260403_000013_drop_orphan_channel_override_columns;
mod m20260404_000014_api_key_reasoning_envelope_switch;
mod m20260501_000015_api_key_request_capture_switch;
mod m20260509_000016_api_key_request_capture_mode;
mod m20260618_000017_channel_model_affinity_schema;
mod m20260619_000018_request_log_visible_tps;
mod m20260619_000019_billing_rate_records;
mod m20260619_000020_default_pricing_profile;
mod m20260620_000021_pricing_profile_pattern_defaults;
mod m20260718_000022_move_models_to_channels;
mod m20260718_000023_channel_model_multiplier_float8;
mod m20260729_000024_channel_affinity_overrides;
mod m20260809_000025_storage_ledger_integrity;
mod m20260809_000026_exact_multiplier_text;
mod m20260809_000027_request_log_legacy_time_index;
mod m20260809_000028_channel_model_name_index;
mod m20260809_000029_sessions_expires_at_index;
mod m20260809_000030_normalize_billing_json_nulls;
mod m20260809_000031_request_logs_without_user_fk;
mod m20260823_000032_billing_plan_subscriptions;
mod m20260823_000033_billing_ledger_delta_dedupe;
mod m20260823_000034_channel_egress_proxy;
mod m20260823_000035_channel_extra_headers;
mod m20260823_000036_channel_session_affinity_auto;
mod m20260823_000037_billing_plan_cron_schedule;
mod m20260824_000038_request_log_session_affinity;
mod m20260824_000039_api_keys_plaintext_only;
mod m20260824_000040_drop_request_log_visible_tps;
mod m20260824_000041_request_capture_records;
mod m20260825_000042_groups_registry;
mod m20260825_000043_channel_allow_missing_usage;
mod m20260826_000048_provider_pricing_flatten;
mod m20260827_000049_store_billing;
mod m20260827_000050_store_payment_icons;
mod m20260827_000051_store_payment_core;
mod m20260827_000052_store_reauth_and_credential_guard;
mod m20260827_000053_store_fulfillment_retries;
mod m20260828_000054_store_channel_readiness;
mod m20260828_000055_store_refund_reauth;
mod m20260828_000056_store_reprocess_reauth;
mod m20260828_000057_store_refund_query_retries;
mod m20260828_000058_store_retention_runtime;
mod m20260829_000059_store_released_schema_repair;
mod m20260829_000060_api_key_channel_bindings;
mod m20260830_000061_usage_ranking_privacy;
mod m20260904_000062_group_visibility;
mod m20260908_000063_enterprise_account_class;
mod m20260908_000064_epay_adapter;
mod m20260909_000065_readiness_optional_evidence;
mod m20260909_000066_billing_rate_currency;
mod m20260909_000067_model_metadata_price_currency;
mod m20260909_000068_sales_commission;
mod m20260909_000069_sales_withdrawal_cancelled;
mod m20260909_000070_attempt_buyer_query_at;
mod m20260909_000071_private_account_class;
mod m20260910_000072_billing_ledger_user_created_index;
mod m20260912_000073_agent_account_class;
mod m20260913_000074_user_sub_accounts;
mod m20260913_000075_org_spaces;
mod m20260914_000076_org_admin_controls;
mod m20260914_000077_request_log_usage_breakdown_slim;
mod m20260915_000078_firewall_events;
mod m20260915_000079_firewall_event_action;
mod m20260915_000080_firewall_event_reason;
mod m20260915_000081_billing_rate_peak_price;
mod m20260915_000082_api_key_model_bindings;
mod m20260915_000083_org_key_ownership;
mod m20260915_000101_billing_ledger_account_scope;
mod m20260915_000102_session_token_hash;
mod m20260916_000103_admin_revenue_daily;
mod m20260916_000104_normalize_quota_day;
mod m20260916_000105_provider_cache_routing_guards;
mod m20260916_000106_admin_revenue_user_rows;
mod m20260917_000107_admin_revenue_user_model_rows;
mod m20260918_000108_announcements;
mod m20260919_000109_org_usage_limits;
mod m20260921_000110_api_key_daily_limit;
mod m20260921_000120_drop_api_key_daily_limit;
mod m20260922_000121_studio_workflow;
mod m20260922_000122_studio_bridge_ops;
mod m20260922_000123_drop_studio_workflow;
mod m20260922_000124_org_member_alias;
mod m20260924_000125_request_log_upstream_response_model;
mod m20261001_000126_firewall_timestamp_bigint;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    fn versions(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn normal_history_runs_embedded_migrator() {
        let decision = startup_migration_decision(
            &versions(&["m001_initial", "m002_current"]),
            &versions(&["m001_initial"]),
        )
        .expect("normal history is accepted");

        assert_eq!(decision, StartupMigrationDecision::RunEmbedded);
    }

    #[test]
    fn fully_applied_history_is_current() {
        let decision = startup_migration_decision(
            &versions(&["m001_initial", "m002_current"]),
            &versions(&["m001_initial", "m002_current"]),
        )
        .expect("fully applied history is accepted");

        assert_eq!(decision, StartupMigrationDecision::FullyApplied);
    }

    #[test]
    fn complete_history_with_only_later_versions_is_accepted() {
        let decision = startup_migration_decision(
            &versions(&["m001_initial", "m002_current"]),
            &versions(&["m001_initial", "m002_current", "m003_future", "m004_future"]),
        )
        .expect("strictly newer history is accepted");

        assert_eq!(
            decision,
            StartupMigrationDecision::AcceptNewerApplied {
                newest_embedded: "m002_current".to_string(),
                newer_applied: versions(&["m003_future", "m004_future"]),
            }
        );
    }

    #[test]
    fn later_version_with_missing_embedded_version_fails_closed() {
        let error = startup_migration_decision(
            &versions(&["m001_initial", "m002_current"]),
            &versions(&["m001_initial", "m003_future"]),
        )
        .expect_err("known migration gap must fail");

        assert!(error.to_string().contains("m002_current"));
    }

    #[test]
    fn non_embedded_version_inside_known_range_fails_closed() {
        let error = startup_migration_decision(
            &versions(&["m001_initial", "m003_current"]),
            &versions(&["m001_initial", "m002_unknown", "m003_current"]),
        )
        .expect_err("unknown migration inside known range must fail");

        assert!(error.to_string().contains("m002_unknown"));
    }

    #[test]
    fn unordered_embedded_versions_fail_closed() {
        let error = startup_migration_decision(
            &versions(&["m002_current", "m001_initial"]),
            &versions(&[]),
        )
        .expect_err("unordered embedded history must fail");

        assert!(error.to_string().contains("strictly ordered"));
    }

    #[tokio::test]
    async fn startup_wrapper_accepts_history_that_seaorm_rejects_as_too_new() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite");
        Migrator::install(&db)
            .await
            .expect("install migration table");

        let mut applied = Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>();
        applied.push("m99999999_999999_future_migration".to_string());
        for version in applied {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO seaql_migrations (version, applied_at) VALUES (?, ?)",
                [version.into(), 0_i64.into()],
            ))
            .await
            .expect("record applied migration");
        }

        let direct_error = Migrator::up(&db, None)
            .await
            .expect_err("SeaORM rejects migration versions missing from this binary");
        assert!(
            direct_error
                .to_string()
                .contains("m99999999_999999_future_migration")
        );

        run_startup_migrations(&db)
            .await
            .expect("startup wrapper accepts only strictly newer versions");
    }

    #[tokio::test]
    async fn replica_schema_check_accepts_fully_applied_history() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite");
        Migrator::install(&db)
            .await
            .expect("install migration table");
        for version in Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_string())
        {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO seaql_migrations (version, applied_at) VALUES (?, ?)",
                [version.into(), 0_i64.into()],
            ))
            .await
            .expect("record applied migration");
        }

        verify_schema_current(&db)
            .await
            .expect("PRP10 fully-applied replicas continue");
    }

    #[tokio::test]
    async fn replica_schema_check_rejects_pending_embedded_versions() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite");
        Migrator::install(&db)
            .await
            .expect("install migration table");
        let Some(first) = Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_string())
            .next()
        else {
            panic!("embedded migrations must not be empty");
        };
        db.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO seaql_migrations (version, applied_at) VALUES (?, ?)",
            [first.into(), 0_i64.into()],
        ))
        .await
        .expect("record partial history");

        let error = verify_schema_current(&db)
            .await
            .expect_err("pending embedded versions must fail");
        assert!(
            error.to_string().contains("replica_schema_pending"),
            "{error}"
        );
    }
}

use super::*;
use crate::app::{RuntimeConfig, load_state_with_runtime};
use crate::auth::AuthResult;
use crate::billing_rate_store::DbBillingRateRecord;
use crate::model_registry_store::ModelPricing;
use crate::monoize_routing::{
    CreateMonoizeChannelInput, CreateMonoizeProviderInput, MonoizeModelEntry, MonoizeProviderType,
};
use crate::settings::normalize_pricing_model_key;
use crate::urp;
use crate::users::{
    CompiledModelRedirectRule, ModelRedirectRule, RequestCaptureMode, UserRole,
    compile_model_redirects,
};
use axum::http::StatusCode;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

const GROUP_ROUTING_MODEL: &str = "gpt-group-routing";

#[test]
fn admission_client_error_uses_fixed_public_message_and_retains_internal_detail() {
    let error = crate::replica::admission_client::AdmissionClientError::new(
        "plan_admission_verification_unavailable",
        "database at https://secret.invalid/private failed",
    );
    let mapped = map_admission_client_error(error);
    assert_eq!(mapped.code, "plan_admission_verification_unavailable");
    assert_eq!(mapped.message, "plan admission is unavailable");
    assert_eq!(
        mapped.internal_message.as_deref(),
        Some("database at https://secret.invalid/private failed")
    );
}

fn test_rate(
    id: &str,
    usage_class: &str,
    unit_price: i128,
    context_tier: Option<&str>,
    modality: Option<&str>,
    cache_ttl: Option<&str>,
    match_json: Value,
) -> DbBillingRateRecord {
    DbBillingRateRecord {
        id: id.to_string(),
        source: "test".to_string(),
        pricing_profile: "test".to_string(),
        model_pattern: Some("test-model".to_string()),
        provider_type: Some("responses".to_string()),
        rate_kind: "token".to_string(),
        usage_class: usage_class.to_string(),
        unit: "token".to_string(),
        unit_price_nano: unit_price.to_string(),
        unit_price_currency: crate::billing_rate_store::RATE_CURRENCY_USD.to_string(),
        peak_unit_price_nano: None,
        context_tier: context_tier.map(str::to_string),
        service_tier: None,
        modality: modality.map(str::to_string),
        cache_ttl: cache_ttl.map(str::to_string),
        match_json,
        priority: 0,
        enabled: true,
        raw_json: serde_json::json!({}),
        updated_at: chrono::Utc::now(),
    }
}

fn test_meter_rate(
    id: &str,
    usage_class: &str,
    unit: &str,
    unit_price: i128,
    match_json: Value,
) -> DbBillingRateRecord {
    DbBillingRateRecord {
        id: id.to_string(),
        source: "test".to_string(),
        pricing_profile: "test".to_string(),
        model_pattern: Some("test-model".to_string()),
        provider_type: Some("responses".to_string()),
        rate_kind: "meter".to_string(),
        usage_class: usage_class.to_string(),
        unit: unit.to_string(),
        unit_price_nano: unit_price.to_string(),
        unit_price_currency: crate::billing_rate_store::RATE_CURRENCY_USD.to_string(),
        peak_unit_price_nano: None,
        context_tier: None,
        service_tier: None,
        modality: None,
        cache_ttl: None,
        match_json,
        priority: 0,
        enabled: true,
        raw_json: serde_json::json!({}),
        updated_at: chrono::Utc::now(),
    }
}

/// PP-CUR-4: the charge, not the unit price, is converted.
///
/// Converting the unit price first would destroy cheap rates: the operator's cheapest rate is
/// 10 nano-CNY per token, and `10 / 7.1` rounds to 1, a 29 percent error. Multiplying by the
/// quantity first keeps the dividend large enough that the error stays under one nano-USD for
/// the whole line.
#[test]
fn cny_charges_convert_after_multiplication_not_before() {
    use crate::billing_rate_store::{RATE_CURRENCY_CNY, RATE_CURRENCY_USD};
    use crate::handlers::billing::charge_in_usd;
    use rust_decimal::Decimal;
    use std::str::FromStr as _;

    let fx = Decimal::from_str("7.1").expect("rate parses");
    let mut cny = test_rate("cny", "input_uncached", 10, None, None, None, json!({}));
    cny.unit_price_currency = RATE_CURRENCY_CNY.to_string();
    let mut usd = test_rate("usd", "input_uncached", 10, None, None, None, json!({}));
    usd.unit_price_currency = RATE_CURRENCY_USD.to_string();

    // 1,000,000 tokens x 10 nano-CNY = 10,000,000 nano-CNY; / 7.1 = 1,408,450.7 -> 1,408,451.
    let charge = 1_000_000_i128 * 10;
    assert_eq!(
        charge_in_usd(charge, &cny, Some(fx)).expect("converts"),
        1_408_451
    );

    // Quantising the unit price first would give 1 nano-USD/token = 1,000,000 for the same
    // line, which is 29 percent low. The implementation must not produce that.
    assert_ne!(
        charge_in_usd(charge, &cny, Some(fx)).expect("converts"),
        1_000_000
    );

    // A USD-basis rate is returned untouched, with or without a snapshot.
    assert_eq!(
        charge_in_usd(charge, &usd, Some(fx)).expect("passes"),
        charge
    );
    assert_eq!(charge_in_usd(charge, &usd, None).expect("passes"), charge);

    // A CNY rate cannot be billed without a snapshot; it must fail rather than bill at the
    // USD magnitude, which would overcharge by the exchange rate.
    assert!(charge_in_usd(charge, &cny, None).is_err());
    assert!(charge_in_usd(charge, &cny, Some(Decimal::ZERO)).is_err());
}

fn test_resolution(rates: Vec<DbBillingRateRecord>) -> BillingRateResolution {
    BillingRateResolution {
        is_peak: false,
        pricing_profile: "test".to_string(),
        pricing_model: "test-model".to_string(),
        rates,
        // The fixtures are USD-basis, so no exchange rate is needed to bill them.
        cny_per_usd: None,
    }
}

#[test]
fn plan_maximum_charge_requires_finite_input_output_and_meter_caps() {
    let resolution = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            2,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            5,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_meter_rate(
            "search",
            "web_search",
            "call",
            100,
            serde_json::json!({"maximum_units": 3}),
        ),
    ]);
    assert_eq!(
        plan_maximum_charge_nano(
            &resolution,
            1_000,
            Some(200),
            &["web_search".to_string()],
            Multiplier::ONE,
        )
        .unwrap(),
        3_300
    );
    let unbounded = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            2,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            5,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_meter_rate("search", "web_search", "call", 100, serde_json::json!({})),
    ]);
    assert!(
        plan_maximum_charge_nano(
            &unbounded,
            1_000,
            Some(200),
            &["web_search".to_string()],
            Multiplier::ONE,
        )
        .is_none()
    );
}

#[test]
fn plan_maximum_charge_supports_bounded_input_only_embeddings() {
    let resolution = test_resolution(vec![test_rate(
        "embedding-input",
        "input_uncached",
        4,
        None,
        None,
        None,
        serde_json::json!({}),
    )]);
    assert_eq!(
        plan_maximum_charge_nano(&resolution, 2_000, None, &[], Multiplier::ONE),
        Some(8_000)
    );
}

#[tokio::test]
async fn admitted_funding_scope_releases_errors_and_preserves_settlement_and_balance() {
    let store = handler_plan_store().await;
    for code in [
        "transform_failed",
        "client_failed",
        "encode_failed",
        "upstream_usage_required",
        "all_providers_failed",
    ] {
        let request_id = format!("handler-request-{code}");
        reserve_handler_plan(&store, &request_id).await;
        let result: AppResult<()> =
            AdmittedFundingScope::primary_plan(store.clone(), request_id.clone())
                .finish(Err(AppError::new(StatusCode::BAD_GATEWAY, code, code)))
                .await;
        assert_eq!(result.unwrap_err().code, code);
        assert_eq!(
            store
                .reservation_for_request(&request_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            crate::store_billing::quota::QuotaTerminalState::Released
        );
    }

    let settled_request = "handler-request-settled";
    reserve_handler_plan(&store, settled_request).await;
    store
        .settle_request_if_reserved(settled_request, 5_000_000, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        AdmittedFundingScope::primary_plan(store.clone(), settled_request.to_string())
            .finish(Ok(17_u8))
            .await
            .unwrap(),
        17
    );
    assert_eq!(
        store
            .reservation_for_request(settled_request)
            .await
            .unwrap()
            .unwrap()
            .state,
        crate::store_billing::quota::QuotaTerminalState::Settled
    );

    let balance_error = AppError::new(StatusCode::BAD_REQUEST, "balance-error", "balance-error");
    let result: AppResult<()> = AdmittedFundingScope::balance()
        .finish(Err(balance_error))
        .await;
    assert_eq!(result.unwrap_err().code, "balance-error");
}

#[tokio::test]
async fn allowed_missing_usage_releases_plan_and_finishes_successfully() {
    let state = load_state_with_runtime(RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    })
    .await
    .unwrap();
    let store = seed_handler_plan(state.db_pool.clone()).await;
    let request_id = "handler-allowed-missing-usage";
    reserve_handler_plan(&store, request_id).await;
    let mut attempt = affinity_test_attempt(
        "handler-provider",
        "handler-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        0,
    );
    attempt.allow_missing_usage = true;
    let mut response = urp::UrpResponse {
            outcome: Default::default(),
        id: "response-1".to_string(),
        model: "handler-model".to_string(),
        created_at: None,
        output: vec![],
        finish_reason: Some(urp::FinishReason::Stop),
        usage: None,
        extra_body: HashMap::new(),
    };
    let skip_charge = substitute_zero_usage_if_allowed(&mut response.usage, &attempt);
    assert!(skip_charge);

    let charge = maybe_charge_response(
        &state,
        &build_test_auth(None),
        &attempt,
        "handler-model",
        &response,
        skip_charge,
        Some(request_id),
    )
    .await
    .unwrap();
    let finished = AdmittedFundingScope::primary_plan(store.clone(), request_id.to_string())
        .finish(Ok(charge))
        .await
        .unwrap();
    assert_eq!(finished.charge_nano_usd, None);
    assert_eq!(
        store
            .reservation_for_request(request_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        crate::store_billing::quota::QuotaTerminalState::Released
    );
}

#[test]
fn plan_quota_error_mapping_preserves_code_and_status() {
    use crate::store_billing::quota::QuotaError;

    for (error, status, code) in [
        (
            QuotaError::Code("plan_quota_exhausted"),
            StatusCode::PAYMENT_REQUIRED,
            "plan_quota_exhausted",
        ),
        (
            QuotaError::Code("plan_request_unbounded"),
            StatusCode::PAYMENT_REQUIRED,
            "plan_request_unbounded",
        ),
        (
            QuotaError::Code("plan_payment_hold"),
            StatusCode::LOCKED,
            "plan_payment_hold",
        ),
        (
            QuotaError::Code("plan_quota_violation_blocked"),
            StatusCode::LOCKED,
            "plan_quota_violation_blocked",
        ),
        (
            QuotaError::Code("quota_gate_unavailable"),
            StatusCode::SERVICE_UNAVAILABLE,
            "quota_gate_unavailable",
        ),
        (
            QuotaError::Storage("offline".to_string()),
            StatusCode::SERVICE_UNAVAILABLE,
            "quota_storage_error",
        ),
    ] {
        let mapped = map_plan_quota_error(error);
        assert_eq!(mapped.status, status);
        assert_eq!(mapped.code, code);
    }
}

#[tokio::test]
async fn stream_pre_return_error_releases_plan_reservation() {
    let store = handler_plan_store().await;
    let request_id = "handler-stream-pre-return-error";
    reserve_handler_plan(&store, request_id).await;

    let outcome: AppResult<()> = Err(AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "request_log_spool_unavailable",
        "request log spool is unavailable",
    ));
    let error = finish_stream_forwarding_outcome(
        AdmittedFundingScope::primary_plan(store.clone(), request_id.to_string()),
        outcome,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "request_log_spool_unavailable");
    assert_eq!(
        store
            .reservation_for_request(request_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        crate::store_billing::quota::QuotaTerminalState::Released
    );
}

async fn reserve_handler_plan(store: &crate::store_billing::quota::QuotaStore, request_id: &str) {
    store
        .reserve(crate::store_billing::quota::QuotaReservationInput {
            user_id: "handler-plan-user".to_string(),
            request_id: request_id.to_string(),
            maximum_nano_usd: 10_000_000,
            pricing_revision: "handler-pricing".to_string(),
            now: chrono::Utc::now(),
        })
        .await
        .unwrap();
}

async fn handler_plan_store() -> crate::store_billing::quota::QuotaStore {
    use crate::migration::Migrator;
    use sea_orm_migration::MigratorTrait as _;

    let db = crate::db::DbPool::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&*db.write().await, None).await.unwrap();
    seed_handler_plan(db).await
}

async fn seed_handler_plan(db: crate::db::DbPool) -> crate::store_billing::quota::QuotaStore {
    use crate::store_billing::models::{PlanQuota, WindowKind};
    use crate::store_billing::quota::EntitlementGenerationInput;
    use crate::store_billing::quota_gate::{GateSlot, QuotaGateStore, QuotaManifest};
    use sea_orm::ConnectionTrait as _;

    let group = db
        .read()
        .query_one(db.stmt("SELECT id FROM monoize_groups WHERE is_default = 1", vec![]))
        .await
        .unwrap()
        .unwrap();
    let group_id: String = group.try_get("", "id").unwrap();
    db.write()
        .await
        .execute(db.stmt(
            "INSERT INTO users
                (id, username, password_hash, role, created_at, updated_at, enabled,
                 balance_nano_usd, balance_unlimited, group_id)
             VALUES ('handler-plan-user', 'handler-plan-user', 'test', 'user',
                     $1, $1, 1, '0', 0, $2)",
            vec![chrono::Utc::now().to_rfc3339().into(), group_id.into()],
        ))
        .await
        .unwrap();
    db.write()
        .await
        .execute_unprepared(
            "INSERT INTO store_products
                (id, kind, name, description, price_currency, price_minor,
                 duration_seconds, group_ids, sort_order, enabled, created_at,
                 updated_at, revision)
             VALUES ('handler-plan-product', 'plan', 'Handler plan', '', 'CNY', '100',
                     86400, '[]', 0, 0, '2026-08-28T00:00:00Z',
                     '2026-08-28T00:00:00Z', 1)",
        )
        .await
        .unwrap();
    let gate = QuotaGateStore::new(db.clone());
    let environment = gate.live_environment().await.unwrap();
    gate.import_manifest(
        GateSlot::Current,
        QuotaManifest::passed(
            environment,
            "test-app",
            "handler-drill",
            chrono::Utc::now(),
            "handler-admin",
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let store = crate::store_billing::quota::QuotaStore::new(db);
    let now = chrono::Utc::now();
    store
        .replace_entitlement(EntitlementGenerationInput {
            expected_generation: None,
            user_id: "handler-plan-user".to_string(),
            product_id: "handler-plan-product".to_string(),
            product_name: "Handler plan".to_string(),
            starts_at: now - chrono::Duration::minutes(1),
            ends_at: now + chrono::Duration::days(1),
            rate_numerator: "6".to_string(),
            rate_denominator: "1".to_string(),
            group_ids: vec![],
            quotas: vec![PlanQuota {
                id: "handler-day".to_string(),
                window_kind: WindowKind::Day,
                window_seconds: 86_400,
                quota_fen_cny: "1000".to_string(),
                sort_order: 0,
            }],
            source_kind: "order".to_string(),
            source_id: "handler-plan-source".to_string(),
        })
        .await
        .unwrap();
    store
}

fn build_test_auth(effective_groups: Option<Vec<String>>) -> AuthResult {
    build_test_auth_with_role(effective_groups, UserRole::User)
}

fn build_test_auth_with_role(
    effective_groups: Option<Vec<String>>,
    user_role: UserRole,
) -> AuthResult {
    AuthResult {
        tenant_id: "tenant-1".to_string(),
        user_id: None,
        username: None,
        user_role,
        account_class: crate::users::AccountClass::Standard,
        api_key_id: None,
        api_key_name: None,
        internal_source: None,
        max_multiplier: None,
        transforms: Vec::new(),
        model_redirects: Vec::new(),
        effective_groups,
        channel_bindings: Vec::new(),
        model_bindings: Vec::new(),
        model_limits_enabled: false,
        model_limits: Vec::new(),
        ip_whitelist: Vec::new(),
        sub_account_enabled: false,
        sub_account_balance_nano: "0".to_string(),
        reasoning_envelope_enabled: true,
        request_capture_mode: RequestCaptureMode::Off,
        org_key: None,
    }
}

fn build_test_urp_request(model: &str) -> urp::UrpRequest {
    urp::UrpRequest {
            context: Default::default(),
            image_generation: Default::default(),
            instructions_format: Default::default(),
            sampling: Default::default(),
            logprobs: Default::default(),
        model: model.to_string(),
        input: vec![urp::Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
            id: None,
            role: urp::OrdinaryRole::User,
            content: "hello".to_string(),
            phase: None,
            extra_body: HashMap::new(),
        }],
        stream: Some(false),
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        reasoning: None,
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        stop: None,
        verbosity: None,
        response_format: None,
        user: None,
        extra_body: HashMap::new(),
    }
}

#[test]
fn provider_extra_filter_retains_internal_state_until_same_chat_encoding() {
    let mut req = urp::decode::openai_chat::decode_request(&serde_json::json!({
        "model": "gpt-4-0613",
        "messages": [{ "role": "user", "content": "hello" }],
        "functions": [{
            "name": "lookup",
            "parameters": { "type": "object" }
        }],
        "function_call": { "name": "lookup", "x_choice": "kept" },
        "unlisted_provider_field": "drop"
    }))
    .expect("decode deprecated Chat controls");

    filter_extra_body_for_provider(&mut req, ProviderType::ChatCompletion, &None);

    assert!(
        req.extra_body
            .contains_key(urp::CHAT_LEGACY_FUNCTION_CHOICE_EXTRA_KEY)
    );
    assert!(!req.extra_body.contains_key("unlisted_provider_field"));

    let wire = urp::encode::openai_chat::encode_request(&req, "gpt-4-0613");
    assert_eq!(
        wire["function_call"],
        serde_json::json!({ "name": "lookup", "x_choice": "kept" })
    );
    assert_eq!(wire["functions"][0]["name"], serde_json::json!("lookup"));
    assert!(wire.get("tool_choice").is_none());
    assert!(wire.get("tools").is_none());
    assert!(!wire.to_string().contains("_monoize_"));
}

fn build_test_routing_request(model: &str) -> UrpRequest {
    UrpRequest {
            audio_output_format: Default::default(),
            messages_custom_tool_names: Default::default(),
        model: model.to_string(),
        max_multiplier: None,
        server_tool_usage_classes: Vec::new(),
        affinity_explicit: None,
        affinity_prefix_hash: crate::handlers::helpers::short_xxh3_hex(model),
        estimated_input_tokens: 0,
        has_tools: false,
    }
}

fn build_model_redirect_rule(pattern: &str, replace: &str) -> CompiledModelRedirectRule {
    compile_model_redirects(&[ModelRedirectRule {
        pattern: pattern.to_string(),
        replace: replace.to_string(),
    }])
    .expect("model redirect compiles")
    .pop()
    .expect("compiled rule exists")
}

#[test]
fn strip_orphaned_tool_calls_keeps_only_closed_stateless_pairs() {
    let mut req = urp::UrpRequest {
            context: Default::default(),
            image_generation: Default::default(),
            instructions_format: Default::default(),
            sampling: Default::default(),
            logprobs: Default::default(),
        model: "gpt-5.5".to_string(),
        input: vec![
            urp::Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
                id: None,
                role: urp::OrdinaryRole::User,
                content: "start".to_string(),
                phase: None,
                extra_body: HashMap::new(),
            },
            urp::Node::ToolCall {
            namespace: Default::default(),
            signature: Default::default(),
                id: Some("fc_answered".to_string()),
                tool_type: urp::ToolCallType::Function,
                call_id: "call_answered".to_string(),
                name: "tool".to_string(),
                arguments: "{}".to_string(),
                extra_body: HashMap::new(),
            },
            urp::Node::ToolCall {
            namespace: Default::default(),
            signature: Default::default(),
                id: Some("fc_unanswered".to_string()),
                tool_type: urp::ToolCallType::Function,
                call_id: "call_unanswered".to_string(),
                name: "tool".to_string(),
                arguments: "{}".to_string(),
                extra_body: HashMap::new(),
            },
            urp::Node::ToolResult {
            name: Default::default(),
            namespace: Default::default(),
            signature: Default::default(),
                id: None,
                tool_type: urp::ToolCallType::Function,
                call_id: "call_answered".to_string(),
                is_error: false,
                content: vec![urp::ToolResultContent::Text {
                    text: "ok".to_string(),
                    extra_body: HashMap::new(),
                }],
                extra_body: HashMap::new(),
            },
            urp::Node::ToolResult {
            name: Default::default(),
            namespace: Default::default(),
            signature: Default::default(),
                id: None,
                tool_type: urp::ToolCallType::Function,
                call_id: "call_missing".to_string(),
                is_error: false,
                content: vec![urp::ToolResultContent::Text {
                    text: "orphan".to_string(),
                    extra_body: HashMap::new(),
                }],
                extra_body: HashMap::new(),
            },
            urp::Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
                id: None,
                role: urp::OrdinaryRole::User,
                content: "interrupt".to_string(),
                phase: None,
                extra_body: HashMap::new(),
            },
        ],
        stream: None,
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        reasoning: None,
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        stop: None,
        verbosity: None,
        response_format: None,
        user: None,
        extra_body: HashMap::new(),
    };

    strip_orphaned_tool_calls(&mut req);

    let call_ids: Vec<&str> = req
        .input
        .iter()
        .filter_map(|node| match node {
            urp::Node::ToolCall { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let result_ids: Vec<&str> = req
        .input
        .iter()
        .filter_map(|node| match node {
            urp::Node::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(call_ids, vec!["call_answered"]);
    assert_eq!(result_ids, vec!["call_answered"]);
    assert!(
        req.input
            .iter()
            .any(|node| matches!(node, urp::Node::Text { content, .. } if content == "interrupt"))
    );
}

#[test]
fn responses_tool_replay_preserves_plaintext_raw_cot_reasoning() {
    let req = urp::UrpRequest {
            context: Default::default(),
            image_generation: Default::default(),
            instructions_format: Default::default(),
            sampling: Default::default(),
            logprobs: Default::default(),
        model: "gpt-5.5".to_string(),
        input: vec![
            urp::Node::Reasoning {
            metadata: Default::default(),
                id: Some("rs_plain".to_string()),
                content: Some("plain summary".to_string()),
                encrypted: None,
                summary: Some("plain summary".to_string()),
                source: None,
                extra_body: HashMap::new(),
            },
            urp::Node::ToolCall {
            namespace: Default::default(),
            signature: Default::default(),
                id: Some("fc_answered".to_string()),
                tool_type: urp::ToolCallType::Function,
                call_id: "call_answered".to_string(),
                name: "tool".to_string(),
                arguments: "{}".to_string(),
                extra_body: HashMap::new(),
            },
            urp::Node::ToolResult {
            name: Default::default(),
            namespace: Default::default(),
            signature: Default::default(),
                id: None,
                tool_type: urp::ToolCallType::Function,
                call_id: "call_answered".to_string(),
                is_error: false,
                content: vec![urp::ToolResultContent::Text {
                    text: "ok".to_string(),
                    extra_body: HashMap::new(),
                }],
                extra_body: HashMap::new(),
            },
            urp::Node::Reasoning {
            metadata: Default::default(),
                id: Some("rs_encrypted".to_string()),
                content: Some("kept".to_string()),
                encrypted: Some(serde_json::json!("sig_kept")),
                summary: Some("kept".to_string()),
                source: None,
                extra_body: HashMap::new(),
            },
        ],
        stream: None,
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        reasoning: None,
        tools: None,
        tool_choice: None,
        parallel_tool_calls: None,
        stop: None,
        verbosity: None,
        response_format: None,
        user: None,
        extra_body: HashMap::new(),
    };

    let encoded = urp::encode::openai_responses::encode_request(&req, "gpt-5.5");
    let input = encoded["input"].as_array().expect("Responses input array");
    let plaintext = input
        .iter()
        .find(|item| item.get("id") == Some(&serde_json::json!("rs_plain")))
        .expect("plaintext RawCoT reasoning item");
    assert_eq!(
        plaintext["content"],
        serde_json::json!([{
            "type": "reasoning_text",
            "text": "plain summary"
        }])
    );
    assert!(
        input
            .iter()
            .any(|item| item.get("id") == Some(&serde_json::json!("rs_encrypted")))
    );
}

async fn seed_group_routing_provider(
    state: &AppState,
    name: &str,
    circuit_breaker_enabled: bool,
    group_id: String,
    channel: CreateMonoizeChannelInput,
) {
    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: Some("openai".to_string()),
            multiplier: Default::default(),
            name: name.to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            enabled: true,
            priority: Some(0),
            group_id,
            channel,
        })
        .await
        .expect("provider created");
}

async fn seed_model_pricing(state: &AppState, model: &str) {
    state
        .model_registry_store
        .upsert_model_metadata(
            model,
            crate::model_registry_store::UpsertModelMetadataInput {
                models_dev_provider: Some(Some("openai".to_string())),
                mode: Some(Some("chat".to_string())),
                input_cost_per_token_nano: Some(Some("1000".to_string())),
                output_cost_per_token_nano: Some(Some("1000".to_string())),
                cache_read_input_cost_per_token_nano: None,
                cache_creation_input_cost_per_token_nano: None,
                output_cost_per_reasoning_token_nano: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_tokens: None,
                price_currency: Some("USD".to_string()),
            },
        )
        .await
        .expect("pricing seeded");
}

#[tokio::test]
async fn routing_uses_channel_model_multiplier_and_redirect_per_attempt() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    seed_model_pricing(&state, "channel-owned-model").await;

    let channel = |id: &str, multiplier: &str, redirect: &str| CreateMonoizeChannelInput {
        name: id.to_string(),
        provider_type: MonoizeProviderType::Responses,
        base_url: format!("https://{id}.example.com"),
        api_key: Some("secret".to_string()),
        enabled: true,
        allow_missing_usage: false,
        passive_failure_count_threshold_override: None,
        passive_cooldown_seconds_override: None,
        passive_window_seconds_override: None,
        passive_rate_limit_cooldown_seconds_override: None,
        models: std::collections::HashMap::from([(
            "channel-owned-model".to_string(),
            MonoizeModelEntry {
                redirect: Some(redirect.to_string()),
                pricing_profile_mode: Default::default(),
                pricing_profile_override: None,
                multiplier_override: Some(multiplier.parse().unwrap()),
            },
        )]),
        active_probe_enabled_override: None,
        active_probe_interval_seconds_override: None,
        active_probe_success_threshold_override: None,
        active_probe_model_override: None,
        affinity_enabled_override: None,
        affinity_idle_ttl_seconds_override: None,
        affinity_failback_mode_override: None,
        affinity_failback_delay_seconds_override: None,

        proxy_url: None,
        extra_headers: None,
        session_affinity_auto: None,
    };

    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: Some("openai".to_string()),
            multiplier: Default::default(),
            name: "channel-owned-models".to_string(),
            channel: channel("cheap", "1", "cheap-upstream"),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: false,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            group_id: String::new(),
            enabled: true,
            priority: Some(0),
        })
        .await
        .expect("provider created");

    let mut request = build_test_routing_request("channel-owned-model");
    request.max_multiplier = Some("1.5".parse().unwrap());
    let attempts = build_monoize_attempts(&state, &request, &build_test_auth(None))
        .await
        .expect("routing succeeds");

    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].channel_name, "cheap");
    assert_eq!(attempts[0].upstream_model, "cheap-upstream");
    assert_eq!(attempts[0].model_multiplier, Multiplier::ONE);
}

fn attempt_channel_names(attempts: &[MonoizeAttempt]) -> BTreeSet<&str> {
    attempts
        .iter()
        .map(|attempt| attempt.channel_name.as_str())
        .collect()
}

#[test]
fn calculate_charge_nano_uses_model_price_and_multiplier() {
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 15,
        output_tokens: 5,
        input_details: None,
        output_details: None,
        extra_body: HashMap::new(),
    };
    let pricing = ModelPricing {
        input_cost_per_token_nano: 2500,
        output_cost_per_token_nano: 10000,
        cache_read_input_cost_per_token_nano: None,
        cache_creation_input_cost_per_token_nano: None,
        output_cost_per_reasoning_token_nano: None,
    };

    let charged = calculate_charge_nano(
        &usage,
        &pricing,
        Multiplier::parse("1.234567891").expect("valid multiplier"),
    );

    assert_eq!(charged, Some(108_024));
}

#[test]
fn calculate_charge_nano_handles_cached_and_reasoning_tokens() {
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 100,
        output_tokens: 80,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 60,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: Some(urp::OutputDetails {
            standard_tokens: 0,
            reasoning_tokens: 30,
            accepted_prediction_tokens: 0,
            rejected_prediction_tokens: 0,
            modality_breakdown: None,
        }),
        extra_body: HashMap::new(),
    };
    let pricing = ModelPricing {
        input_cost_per_token_nano: 1000,
        output_cost_per_token_nano: 2000,
        cache_read_input_cost_per_token_nano: Some(100),
        cache_creation_input_cost_per_token_nano: None,
        output_cost_per_reasoning_token_nano: Some(3000),
    };

    let charged = calculate_charge_nano(&usage, &pricing, Multiplier::ONE);

    assert_eq!(charged, Some(236_000));
}

#[test]
fn calculate_charge_nano_messages_treats_cache_creation_as_disjoint_bucket() {
    // Post-decode normalization: Anthropic wire input_tokens=100 + cache_creation=40
    // becomes internal input_tokens=140. Billing uniformly subtracts cache buckets.
    // See user-billing-and-model-metadata.spec.md § 5 C3-ii, C3a.
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 140,
        output_tokens: 20,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 0,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 40,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: None,
        extra_body: HashMap::new(),
    };
    let pricing = ModelPricing {
        input_cost_per_token_nano: 1000,
        output_cost_per_token_nano: 2000,
        cache_read_input_cost_per_token_nano: None,
        cache_creation_input_cost_per_token_nano: Some(250),
        output_cost_per_reasoning_token_nano: None,
    };

    let charged = calculate_charge_nano(&usage, &pricing, Multiplier::ONE);

    assert_eq!(charged, Some(150_000));
}

#[test]
fn calculate_charge_nano_responses_excludes_cache_creation_from_inclusive_input_total() {
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 100,
        output_tokens: 20,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 0,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 40,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: None,
        extra_body: HashMap::new(),
    };
    let pricing = ModelPricing {
        input_cost_per_token_nano: 1000,
        output_cost_per_token_nano: 2000,
        cache_read_input_cost_per_token_nano: Some(100),
        cache_creation_input_cost_per_token_nano: Some(250),
        output_cost_per_reasoning_token_nano: None,
    };

    let charged = calculate_charge_nano(&usage, &pricing, Multiplier::ONE);

    assert_eq!(charged, Some(110_000));
}

#[test]
fn calculate_charge_nano_responses_avoids_double_count_when_cache_read_and_creation_are_both_present()
 {
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 100,
        output_tokens: 10,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 30,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 20,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: None,
        extra_body: HashMap::new(),
    };
    let pricing = ModelPricing {
        input_cost_per_token_nano: 1000,
        output_cost_per_token_nano: 2000,
        cache_read_input_cost_per_token_nano: Some(100),
        cache_creation_input_cost_per_token_nano: Some(250),
        output_cost_per_reasoning_token_nano: None,
    };

    let charged = calculate_charge_nano(&usage, &pricing, Multiplier::ONE);

    assert_eq!(charged, Some(78_000));
}

#[test]
fn rate_matrix_selects_short_vs_long_context_tier() {
    let threshold = serde_json::json!({ "context_threshold_tokens": 128000 });
    let resolution = test_resolution(vec![
        test_rate(
            "short-input",
            "input_uncached",
            1,
            Some("short"),
            None,
            None,
            threshold.clone(),
        ),
        test_rate(
            "short-output",
            "output",
            2,
            Some("short"),
            None,
            None,
            threshold.clone(),
        ),
        test_rate(
            "long-input",
            "input_uncached",
            10,
            Some("long"),
            None,
            None,
            threshold.clone(),
        ),
        test_rate(
            "long-output",
            "output",
            20,
            Some("long"),
            None,
            None,
            threshold,
        ),
    ]);
    assert!(
        billing_rate_matrix_allows_request(&resolution, &Vec::new())
            .expect("tiered matrix has threshold")
    );
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 128001,
        output_tokens: 10,
        input_details: None,
        output_details: None,
        extra_body: HashMap::new(),
    };

    let components = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &resolution,
        Multiplier::ONE,
        &Vec::new(),
    )
    .expect("charge succeeds");

    assert_eq!(components.context_tier.as_deref(), Some("long"));
    assert_eq!(components.base_charge, 1_280_210);
}

#[test]
fn rate_matrix_bills_anthropic_cache_ttl_split_and_read() {
    let resolution = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "read",
            "cache_read",
            2,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "write-5m",
            "cache_write_5m",
            3,
            None,
            None,
            Some("5m"),
            serde_json::json!({}),
        ),
        test_rate(
            "write-1h",
            "cache_write_1h",
            4,
            None,
            None,
            Some("1h"),
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            5,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 1000,
        output_tokens: 10,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 100,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 300,
            cache_creation_5m_tokens: 200,
            cache_creation_1h_tokens: 100,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: None,
        extra_body: HashMap::new(),
    };

    let components = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &resolution,
        Multiplier::ONE,
        &Vec::new(),
    )
    .expect("charge succeeds");

    assert_eq!(components.base_charge, 1850);
    assert_eq!(components.token_line_items.len(), 5);
}

#[test]
fn rate_matrix_rejects_aggregate_cache_creation_without_ttl_split() {
    let resolution = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "write-5m",
            "cache_write_5m",
            3,
            None,
            None,
            Some("5m"),
            serde_json::json!({}),
        ),
        test_rate(
            "write-1h",
            "cache_write_1h",
            4,
            None,
            None,
            Some("1h"),
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            5,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 1000,
        output_tokens: 10,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 0,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 300,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: None,
        extra_body: HashMap::new(),
    };

    let err = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &resolution,
        Multiplier::ONE,
        &Vec::new(),
    )
    .expect_err("aggregate cache write must not be guessed");

    assert!(err.contains("requires 5m/1h split"));
}

#[test]
fn rate_matrix_uses_dimensionless_defaults_for_cache_and_missing_modality_breakdown() {
    let resolution = test_resolution(vec![
        test_rate(
            "input-default",
            "input_uncached",
            1,
            None,
            None,
            None,
            json!({}),
        ),
        test_rate(
            "input-image",
            "input_uncached",
            9,
            None,
            Some("image"),
            None,
            json!({}),
        ),
        test_rate("output-default", "output", 2, None, None, None, json!({})),
        test_rate(
            "output-image",
            "output",
            9,
            None,
            Some("image"),
            None,
            json!({}),
        ),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 100,
        output_tokens: 10,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 20,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 30,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: None,
        extra_body: HashMap::new(),
    };

    let components = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &resolution,
        Multiplier::ONE,
        &Vec::new(),
    )
    .expect("dimensionless fallback should settle every token bucket");

    assert_eq!(components.base_charge, 120);
    assert_eq!(
        components
            .token_line_items
            .iter()
            .filter(|line| line["rate_id"] == json!("input-default"))
            .count(),
        3
    );
}

#[test]
fn rate_matrix_preflight_rejects_invalid_prices_and_missing_dimensionless_defaults() {
    let mut negative = test_rate(
        "negative",
        "input_uncached",
        -1,
        None,
        None,
        None,
        json!({}),
    );
    negative.priority = 100;
    let invalid_price = test_resolution(vec![
        negative,
        test_rate("input", "input_uncached", 1, None, None, None, json!({})),
        test_rate("output", "output", 1, None, None, None, json!({})),
    ]);
    assert!(
        billing_rate_matrix_allows_request(&invalid_price, &Vec::new())
            .expect_err("negative candidate price must fail preflight")
            .contains("negative unit_price")
    );

    let modality_only = test_resolution(vec![
        test_rate(
            "input-image",
            "input_uncached",
            1,
            None,
            Some("image"),
            None,
            json!({}),
        ),
        test_rate(
            "output-image",
            "output",
            1,
            None,
            Some("image"),
            None,
            json!({}),
        ),
    ]);
    assert_eq!(
        billing_rate_matrix_allows_request(&modality_only, &Vec::new()),
        Ok(false)
    );
}

#[test]
fn rate_matrix_bills_gpt_image_2_modality_token_lines() {
    let resolution = test_resolution(vec![
        test_rate(
            "input-text",
            "input_uncached",
            1,
            None,
            Some("text"),
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "input-image",
            "input_uncached",
            2,
            None,
            Some("image"),
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "cache-text",
            "cache_read",
            4,
            None,
            Some("text"),
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "cache-image",
            "cache_read",
            5,
            None,
            Some("image"),
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "output-image",
            "output",
            3,
            None,
            Some("image"),
            None,
            serde_json::json!({}),
        ),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 160,
        output_tokens: 20,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 10,
            cache_read_modality_breakdown: Some(urp::ModalityBreakdown {
                text_tokens: Some(6),
                image_tokens: Some(4),
                audio_tokens: None,
                video_tokens: None,
                document_tokens: None,
            }),
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: Some(urp::ModalityBreakdown {
                text_tokens: Some(106),
                image_tokens: Some(54),
                audio_tokens: None,
                video_tokens: None,
                document_tokens: None,
            }),
        }),
        output_details: Some(urp::OutputDetails {
            standard_tokens: 0,
            reasoning_tokens: 0,
            accepted_prediction_tokens: 0,
            rejected_prediction_tokens: 0,
            modality_breakdown: Some(urp::ModalityBreakdown {
                text_tokens: None,
                image_tokens: Some(20),
                audio_tokens: None,
                video_tokens: None,
                document_tokens: None,
            }),
        }),
        extra_body: HashMap::new(),
    };

    let components = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &resolution,
        Multiplier::ONE,
        &Vec::new(),
    )
    .expect("charge succeeds");

    assert_eq!(components.base_charge, 304);
    assert_eq!(components.token_line_items.len(), 5);
}

#[test]
fn rate_matrix_supports_input_cached_usage_class_alias() {
    let resolution = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "cached",
            "input_cached",
            2,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            3,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 100,
        output_tokens: 10,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 40,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 0,
            modality_breakdown: None,
        }),
        output_details: None,
        extra_body: HashMap::new(),
    };

    let components = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &resolution,
        Multiplier::ONE,
        &Vec::new(),
    )
    .expect("charge succeeds");

    assert_eq!(components.base_charge, 170);
    assert_eq!(
        components.token_line_items[1]["usage_class"].as_str(),
        Some("input_cached")
    );
}

#[test]
fn rate_matrix_does_not_double_add_inclusive_tool_prompt_or_reasoning_details() {
    let resolution = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            2,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "reasoning",
            "reasoning_output",
            3,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 130,
        output_tokens: 120,
        input_details: Some(urp::InputDetails {
            tool_prompt_modality_breakdown: Default::default(),
            standard_tokens: 0,
            cache_read_tokens: 0,
            cache_read_modality_breakdown: None,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens: 30,
            modality_breakdown: None,
        }),
        output_details: Some(urp::OutputDetails {
            standard_tokens: 0,
            reasoning_tokens: 20,
            accepted_prediction_tokens: 0,
            rejected_prediction_tokens: 0,
            modality_breakdown: None,
        }),
        extra_body: HashMap::new(),
    };

    let components = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &resolution,
        Multiplier::ONE,
        &Vec::new(),
    )
    .expect("charge succeeds");

    assert_eq!(components.base_charge, 390);
}

#[test]
fn rate_matrix_counts_call_meter_from_decoded_native_events_and_requires_duration_usage() {
    let call_resolution = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_meter_rate("web", "web_search", "call", 100, serde_json::json!({})),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 1,
        output_tokens: 1,
        input_details: None,
        output_details: None,
        extra_body: HashMap::new(),
    };
    let output = vec![urp::Node::ProviderItem {
        id: None,
        origin_protocol: urp::ProviderProtocol::Responses,
        role: urp::OrdinaryRole::Assistant,
        item_type: "web_search_call".to_string(),
        body: serde_json::json!({}),
        extra_body: HashMap::new(),
    }];

    let call_components = calculate_rate_matrix_charge_components(
        &usage,
        Some(&output),
        None,
        &call_resolution,
        Multiplier::ONE,
        &["web_search".to_string()],
    )
    .expect("decoded call is billable");

    assert_eq!(call_components.base_charge, 102);

    let duration_resolution = test_resolution(vec![
        test_rate(
            "input",
            "input_uncached",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_rate(
            "output",
            "output",
            1,
            None,
            None,
            None,
            serde_json::json!({}),
        ),
        test_meter_rate(
            "code-duration",
            "code_interpreter_duration",
            "billed_minute",
            1_000,
            serde_json::json!({ "requires_authoritative_usage": true }),
        ),
    ]);
    let err = calculate_rate_matrix_charge_components(
        &usage,
        None,
        None,
        &duration_resolution,
        Multiplier::ONE,
        &["code_interpreter_duration".to_string()],
    )
    .expect_err("duration meter must require authoritative usage");

    assert!(err.contains("authoritative usage required"));
}

#[test]
fn rate_matrix_uses_only_first_dimension_matching_meter_row_per_usage_class() {
    let mut wrong_service =
        test_meter_rate("wrong-service", "web_search", "call", 10_000, json!({}));
    wrong_service.service_tier = Some("priority".to_string());
    let resolution = test_resolution(vec![
        test_rate("input", "input_uncached", 1, None, None, None, json!({})),
        test_rate("output", "output", 1, None, None, None, json!({})),
        wrong_service,
        test_meter_rate("selected", "web_search", "call", 100, json!({})),
        test_meter_rate("duplicate", "web_search", "call", 500, json!({})),
    ]);
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 1,
        output_tokens: 1,
        input_details: None,
        output_details: None,
        extra_body: HashMap::from([("service_tier".to_string(), json!("batch"))]),
    };
    let output = vec![urp::Node::ProviderItem {
        id: None,
        origin_protocol: urp::ProviderProtocol::Responses,
        role: urp::OrdinaryRole::Assistant,
        item_type: "web_search_call".to_string(),
        body: json!({}),
        extra_body: HashMap::new(),
    }];

    let components = calculate_rate_matrix_charge_components(
        &usage,
        Some(&output),
        None,
        &resolution,
        Multiplier::ONE,
        &["web_search".to_string()],
    )
    .expect("matching meter row should be selected once");

    assert_eq!(components.base_charge, 102);
    assert_eq!(components.meter_line_items.len(), 1);
    assert_eq!(components.meter_line_items[0]["rate_id"], json!("selected"));
}

#[test]
fn rate_matrix_uses_actual_response_service_tier_and_requires_exact_rates() {
    let default_rates = vec![
        test_rate(
            "input-default",
            "input_uncached",
            1,
            None,
            None,
            None,
            json!({}),
        ),
        test_rate("output-default", "output", 2, None, None, None, json!({})),
    ];
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 1,
        output_tokens: 1,
        input_details: None,
        output_details: None,
        extra_body: HashMap::from([("service_tier".to_string(), json!("default"))]),
    };

    let err = calculate_rate_matrix_charge_components(
        &usage,
        None,
        Some("priority"),
        &test_resolution(default_rates.clone()),
        Multiplier::ONE,
        &[],
    )
    .expect_err("a non-default response tier must not use default rates");
    assert!(err.contains("service_tier=\"priority\""), "{err}");

    let mut priority_input = test_rate(
        "input-priority",
        "input_uncached",
        20,
        None,
        None,
        None,
        json!({}),
    );
    priority_input.service_tier = Some("priority".to_string());
    let mut priority_output =
        test_rate("output-priority", "output", 30, None, None, None, json!({}));
    priority_output.service_tier = Some("priority".to_string());
    let mut rates = default_rates;
    rates.extend([priority_input, priority_output]);

    let components = calculate_rate_matrix_charge_components(
        &usage,
        None,
        Some("priority"),
        &test_resolution(rates),
        Multiplier::ONE,
        &[],
    )
    .expect("the exact response-tier rates are billable");
    assert_eq!(components.service_tier.as_deref(), Some("priority"));
    assert_eq!(components.base_charge, 50);
    assert_eq!(
        components.token_line_items[0]["rate_id"],
        json!("input-priority")
    );
    assert_eq!(
        components.token_line_items[1]["rate_id"],
        json!("output-priority")
    );
}

#[test]
fn scale_charge_uses_exact_decimal_and_truncates_toward_zero() {
    let base = 100_000_000i128;
    let charged = scale_charge_with_multiplier(
        base,
        Multiplier::parse("1.000000009").expect("valid multiplier"),
    );
    assert_eq!(charged, Some(100_000_000));
}

#[test]
fn scale_charge_avoids_overflow_when_final_value_is_representable() {
    let base = i128::MAX / 2;
    let charged =
        scale_charge_with_multiplier(base, Multiplier::parse("1.1").expect("valid multiplier"));

    assert_eq!(charged, base.checked_add(base / 10));
}

#[test]
fn normalize_pricing_model_key_strips_recognized_reasoning_suffix() {
    let suffix_map = std::collections::HashMap::from([
        ("-thinking".to_string(), "high".to_string()),
        ("-nothinking".to_string(), "none".to_string()),
    ]);

    assert_eq!(
        normalize_pricing_model_key("gpt-5-mini-thinking", &suffix_map),
        "gpt-5-mini"
    );
    assert_eq!(
        normalize_pricing_model_key("gpt-5-mini-high", &suffix_map),
        "gpt-5-mini"
    );
}

#[tokio::test]
async fn resolve_model_suffix_preserves_reasoning_effort_on_attempt_base_request() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");

    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: None,
            multiplier: Default::default(),
            name: "OpenAI".to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: true,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            group_id: String::new(),
            enabled: true,
            priority: Some(0),
            channel: CreateMonoizeChannelInput {
                name: "primary".to_string(),
                provider_type: MonoizeProviderType::Responses,
                base_url: "https://example.com".to_string(),
                api_key: Some("secret".to_string()),
                enabled: true,
                allow_missing_usage: false,
                passive_failure_count_threshold_override: None,
                passive_cooldown_seconds_override: None,
                passive_window_seconds_override: None,
                passive_rate_limit_cooldown_seconds_override: None,
                models: std::collections::HashMap::from([(
                    "gpt-5-mini".to_string(),
                    MonoizeModelEntry {
                        redirect: None,
                        pricing_profile_mode: Default::default(),
                        pricing_profile_override: None,
                        multiplier_override: Some(Multiplier::ONE),
                    },
                )]),
                active_probe_enabled_override: None,
                active_probe_interval_seconds_override: None,
                active_probe_success_threshold_override: None,
                active_probe_model_override: None,
                affinity_enabled_override: None,
                affinity_idle_ttl_seconds_override: None,
                affinity_failback_mode_override: None,
                affinity_failback_delay_seconds_override: None,

                proxy_url: None,
                extra_headers: None,
                session_affinity_auto: None,
            },
        })
        .await
        .expect("provider created");

    let mut req = build_test_urp_request("gpt-5-mini-thinking");
    resolve_model_suffix(&state, &mut req, crate::users::AccountClass::Standard)
        .await
        .unwrap();
    let original_req = req.clone();

    assert_eq!(original_req.model, "gpt-5-mini");
    assert_eq!(
        original_req
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.effort.as_deref()),
        Some("high")
    );

    let mut explicitly_configured_req = build_test_urp_request("gpt-5-mini-thinking");
    explicitly_configured_req.reasoning = Some(urp::ReasoningConfig {
            summary: Default::default(),
            budget_tokens: Default::default(),
            display: Default::default(),
            mode: Default::default(),
        effort: Some("low".to_string()),
        extra_body: std::collections::HashMap::new(),
    });
    resolve_model_suffix(
        &state,
        &mut explicitly_configured_req,
        crate::users::AccountClass::Standard,
    )
    .await
    .unwrap();

    assert_eq!(explicitly_configured_req.model, "gpt-5-mini");
    assert_eq!(
        explicitly_configured_req
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.effort.as_deref()),
        Some("low")
    );
}

#[test]
fn resolve_upstream_model_prefers_non_empty_redirect() {
    let entry = MonoizeModelEntry {
        redirect: Some("  gpt-5-target  ".to_string()),
        pricing_profile_mode: Default::default(),
        pricing_profile_override: None,
        multiplier_override: Some(Multiplier::ONE),
    };
    assert_eq!(
        resolve_upstream_model("gpt-5-logical", &entry),
        "gpt-5-target".to_string()
    );
}

#[test]
fn resolve_upstream_model_falls_back_to_requested_when_redirect_blank() {
    let entry = MonoizeModelEntry {
        redirect: Some("   ".to_string()),
        pricing_profile_mode: Default::default(),
        pricing_profile_override: None,
        multiplier_override: Some(Multiplier::ONE),
    };
    assert_eq!(
        resolve_upstream_model("gpt-5-logical", &entry),
        "gpt-5-logical".to_string()
    );
}

#[tokio::test]
async fn build_monoize_attempts_rejects_unpriced_models_before_forwarding() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");

    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: None,
            multiplier: Default::default(),
            name: "OpenAI".to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: true,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            group_id: String::new(),
            enabled: true,
            priority: Some(0),
            channel: CreateMonoizeChannelInput {
                name: "primary".to_string(),
                provider_type: MonoizeProviderType::Responses,
                base_url: "https://example.com".to_string(),
                api_key: Some("secret".to_string()),
                enabled: true,
                allow_missing_usage: false,
                passive_failure_count_threshold_override: None,
                passive_cooldown_seconds_override: None,
                passive_window_seconds_override: None,
                passive_rate_limit_cooldown_seconds_override: None,
                models: std::collections::HashMap::from([(
                    "gpt-unpriced".to_string(),
                    MonoizeModelEntry {
                        redirect: Some("gpt-unpriced-upstream".to_string()),
                        pricing_profile_mode: Default::default(),
                        pricing_profile_override: None,
                        multiplier_override: Some(Multiplier::ONE),
                    },
                )]),
                active_probe_enabled_override: None,
                active_probe_interval_seconds_override: None,
                active_probe_success_threshold_override: None,
                active_probe_model_override: None,
                affinity_enabled_override: None,
                affinity_idle_ttl_seconds_override: None,
                affinity_failback_mode_override: None,
                affinity_failback_delay_seconds_override: None,

                proxy_url: None,
                extra_headers: None,
                session_affinity_auto: None,
            },
        })
        .await
        .expect("provider created");

    let req = build_test_routing_request("gpt-unpriced");
    let auth = build_test_auth(None);
    let err = build_monoize_attempts(&state, &req, &auth)
        .await
        .expect_err("must reject unpriced model");

    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.code, "model_pricing_required");
    assert!(err.message.contains("gpt-unpriced-upstream"));
}

#[tokio::test]
async fn build_monoize_attempts_rejects_admin_unpriced_models_without_pricing() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");

    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: None,
            multiplier: Default::default(),
            name: "OpenAI".to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: true,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            group_id: String::new(),
            enabled: true,
            priority: Some(0),
            channel: CreateMonoizeChannelInput {
                name: "primary".to_string(),
                provider_type: MonoizeProviderType::Responses,
                base_url: "https://example.com".to_string(),
                api_key: Some("secret".to_string()),
                enabled: true,
                allow_missing_usage: false,
                passive_failure_count_threshold_override: None,
                passive_cooldown_seconds_override: None,
                passive_window_seconds_override: None,
                passive_rate_limit_cooldown_seconds_override: None,
                models: std::collections::HashMap::from([(
                    "gpt-unpriced".to_string(),
                    MonoizeModelEntry {
                        redirect: Some("gpt-unpriced-upstream".to_string()),
                        pricing_profile_mode: Default::default(),
                        pricing_profile_override: None,
                        multiplier_override: Some(Multiplier::ONE),
                    },
                )]),
                active_probe_enabled_override: None,
                active_probe_interval_seconds_override: None,
                active_probe_success_threshold_override: None,
                active_probe_model_override: None,
                affinity_enabled_override: None,
                affinity_idle_ttl_seconds_override: None,
                affinity_failback_mode_override: None,
                affinity_failback_delay_seconds_override: None,

                proxy_url: None,
                extra_headers: None,
                session_affinity_auto: None,
            },
        })
        .await
        .expect("provider created");

    let req = build_test_routing_request("gpt-unpriced");
    let auth = build_test_auth_with_role(None, UserRole::Admin);
    let err = build_monoize_attempts(&state, &req, &auth)
        .await
        .expect_err("admin unpriced request must be rejected");

    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.code, "model_pricing_required");
}

#[tokio::test]
async fn build_monoize_attempts_admits_unpriced_server_tool_meter_class() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");

    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: Some("openai".to_string()),
            multiplier: Default::default(),
            name: "OpenAI".to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: true,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            group_id: String::new(),
            enabled: true,
            priority: Some(0),
            channel: CreateMonoizeChannelInput {
                name: "primary".to_string(),
                provider_type: MonoizeProviderType::Responses,
                base_url: "https://example.com".to_string(),
                api_key: Some("secret".to_string()),
                enabled: true,
                allow_missing_usage: false,
                passive_failure_count_threshold_override: None,
                passive_cooldown_seconds_override: None,
                passive_window_seconds_override: None,
                passive_rate_limit_cooldown_seconds_override: None,
                models: std::collections::HashMap::from([(
                    "gpt-priced".to_string(),
                    MonoizeModelEntry {
                        redirect: None,
                        pricing_profile_mode: Default::default(),
                        pricing_profile_override: None,
                        multiplier_override: Some(Multiplier::ONE),
                    },
                )]),
                active_probe_enabled_override: None,
                active_probe_interval_seconds_override: None,
                active_probe_success_threshold_override: None,
                active_probe_model_override: None,
                affinity_enabled_override: None,
                affinity_idle_ttl_seconds_override: None,
                affinity_failback_mode_override: None,
                affinity_failback_delay_seconds_override: None,

                proxy_url: None,
                extra_headers: None,
                session_affinity_auto: None,
            },
        })
        .await
        .expect("provider created");
    seed_model_pricing(&state, "gpt-priced").await;

    let mut req = build_test_routing_request("gpt-priced");
    req.server_tool_usage_classes = vec!["web_search".to_string()];
    let auth = build_test_auth_with_role(None, UserRole::Admin);
    // MB-M5: an unpriced server-native tool usage class settles at zero instead of
    // excluding the mapping. A rejecting gate made every Codex request fail, because that
    // client enables `web_search` by default.
    let attempts = build_monoize_attempts(&state, &req, &auth)
        .await
        .expect("missing meter rate must not reject");

    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].server_tool_usage_classes, vec!["web_search"]);
    assert!(attempts[0].billable_pricing_available);
}

#[tokio::test]
async fn build_monoize_attempts_accepts_redirected_model_when_logical_fallback_is_priced() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");

    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: Some("openai".to_string()),
            multiplier: Default::default(),
            name: "OpenAI".to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: true,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            group_id: String::new(),
            enabled: true,
            priority: Some(0),
            channel: CreateMonoizeChannelInput {
                name: "primary".to_string(),
                provider_type: MonoizeProviderType::Responses,
                base_url: "https://example.com".to_string(),
                api_key: Some("secret".to_string()),
                enabled: true,
                allow_missing_usage: false,
                passive_failure_count_threshold_override: None,
                passive_cooldown_seconds_override: None,
                passive_window_seconds_override: None,
                passive_rate_limit_cooldown_seconds_override: None,
                models: std::collections::HashMap::from([(
                    "gpt-fallback-src".to_string(),
                    MonoizeModelEntry {
                        redirect: Some("gpt-fallback-dest".to_string()),
                        pricing_profile_mode: Default::default(),
                        pricing_profile_override: None,
                        multiplier_override: Some(Multiplier::ONE),
                    },
                )]),
                active_probe_enabled_override: None,
                active_probe_interval_seconds_override: None,
                active_probe_success_threshold_override: None,
                active_probe_model_override: None,
                affinity_enabled_override: None,
                affinity_idle_ttl_seconds_override: None,
                affinity_failback_mode_override: None,
                affinity_failback_delay_seconds_override: None,

                proxy_url: None,
                extra_headers: None,
                session_affinity_auto: None,
            },
        })
        .await
        .expect("provider created");

    state
        .model_registry_store
        .upsert_model_metadata(
            "gpt-fallback-src",
            crate::model_registry_store::UpsertModelMetadataInput {
                models_dev_provider: Some(Some("openai".to_string())),
                mode: Some(Some("chat".to_string())),
                input_cost_per_token_nano: Some(Some("1000".to_string())),
                output_cost_per_token_nano: Some(Some("1000".to_string())),
                cache_read_input_cost_per_token_nano: None,
                cache_creation_input_cost_per_token_nano: None,
                output_cost_per_reasoning_token_nano: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_tokens: None,
                price_currency: Some("USD".to_string()),
            },
        )
        .await
        .expect("logical pricing seeded");

    state
        .model_registry_store
        .upsert_model_metadata(
            "gpt-fallback-dest",
            crate::model_registry_store::UpsertModelMetadataInput {
                models_dev_provider: Some(Some("openai".to_string())),
                mode: Some(Some("chat".to_string())),
                input_cost_per_token_nano: Some(Some("500".to_string())),
                output_cost_per_token_nano: None,
                cache_read_input_cost_per_token_nano: None,
                cache_creation_input_cost_per_token_nano: None,
                output_cost_per_reasoning_token_nano: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_tokens: None,
                price_currency: Some("USD".to_string()),
            },
        )
        .await
        .expect("partial upstream pricing seeded");

    let resolution = resolve_billing_rate_matrix(
        &state,
        "gpt-fallback-dest",
        "gpt-fallback-src",
        ProviderType::Responses,
        "openai",
    )
    .await
    .expect("pricing lookup")
    .expect("logical fallback pricing");
    assert_eq!(resolution.pricing_model, "gpt-fallback-src");

    let req = build_test_routing_request("gpt-fallback-src");
    let auth = build_test_auth(None);
    let attempts = build_monoize_attempts(&state, &req, &auth)
        .await
        .expect("fallback-priced model should be allowed");

    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].upstream_model, "gpt-fallback-dest");
}

#[tokio::test]
async fn build_monoize_attempts_does_not_use_metadata_pricing_profile_fallback() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");

    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: None,
            multiplier: Default::default(),
            name: "Gateway".to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: true,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens: None,
            prompt_cache_incompatible_with_tools: None,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            group_id: String::new(),
            enabled: true,
            priority: Some(0),
            channel: CreateMonoizeChannelInput {
                name: "primary".to_string(),
                provider_type: MonoizeProviderType::ChatCompletion,
                base_url: "https://example.com".to_string(),
                api_key: Some("secret".to_string()),
                enabled: true,
                allow_missing_usage: false,
                passive_failure_count_threshold_override: None,
                passive_cooldown_seconds_override: None,
                passive_window_seconds_override: None,
                passive_rate_limit_cooldown_seconds_override: None,
                models: std::collections::HashMap::from([(
                    "claude-sonnet-4.6".to_string(),
                    MonoizeModelEntry {
                        redirect: None,
                        pricing_profile_mode: Default::default(),
                        pricing_profile_override: None,
                        multiplier_override: Some(Multiplier::ONE),
                    },
                )]),
                active_probe_enabled_override: None,
                active_probe_interval_seconds_override: None,
                active_probe_success_threshold_override: None,
                active_probe_model_override: None,
                affinity_enabled_override: None,
                affinity_idle_ttl_seconds_override: None,
                affinity_failback_mode_override: None,
                affinity_failback_delay_seconds_override: None,

                proxy_url: None,
                extra_headers: None,
                session_affinity_auto: None,
            },
        })
        .await
        .expect("provider created");

    state
        .model_registry_store
        .upsert_model_metadata(
            "claude-sonnet-4.6",
            crate::model_registry_store::UpsertModelMetadataInput {
                models_dev_provider: Some(Some("zenmux".to_string())),
                mode: Some(Some("chat".to_string())),
                input_cost_per_token_nano: Some(Some("3000".to_string())),
                output_cost_per_token_nano: Some(Some("15000".to_string())),
                cache_read_input_cost_per_token_nano: None,
                cache_creation_input_cost_per_token_nano: None,
                output_cost_per_reasoning_token_nano: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_tokens: None,
                price_currency: Some("USD".to_string()),
            },
        )
        .await
        .expect("metadata pricing seeded");

    let req = build_test_routing_request("claude-sonnet-4.6");
    let auth = build_test_auth(None);
    let error = build_monoize_attempts(&state, &req, &auth)
        .await
        .expect_err("metadata Profile must not make an unpriced mapping routable");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.code, "model_pricing_required");
}

#[tokio::test]
async fn build_monoize_attempts_filters_providers_by_effective_groups_before_health_logic() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let default_group_id = state
        .user_store
        .default_group_id()
        .await
        .expect("default group exists");
    let team_a_group = state
        .user_store
        .create_group(crate::users::CreateGroupInput {
            confirm_public_exposure: true,
            name: "team-a".to_string(),
            description: String::new(),
            user_selectable: true,
            sort_order: 1,
            account_class: Default::default(),
        })
        .await
        .expect("team-a group created");
    let team_b_group = state
        .user_store
        .create_group(crate::users::CreateGroupInput {
            confirm_public_exposure: true,
            name: "team-b".to_string(),
            description: String::new(),
            user_selectable: true,
            sort_order: 2,
            account_class: Default::default(),
        })
        .await
        .expect("team-b group created");

    // GR-I2: an empty selection binds the provider to the default group.
    seed_group_routing_provider(
        &state,
        "public-provider",
        false,
        String::new(),
        CreateMonoizeChannelInput {
            name: "public".to_string(),
            provider_type: MonoizeProviderType::Responses,
            base_url: "https://public.example.com".to_string(),
            api_key: Some("secret".to_string()),
            enabled: true,
            allow_missing_usage: false,
            passive_failure_count_threshold_override: None,
            passive_cooldown_seconds_override: None,
            passive_window_seconds_override: None,
            passive_rate_limit_cooldown_seconds_override: None,
            models: std::collections::HashMap::from([(
                GROUP_ROUTING_MODEL.to_string(),
                MonoizeModelEntry {
                    redirect: None,
                    pricing_profile_mode: Default::default(),
                    pricing_profile_override: None,
                    multiplier_override: Some(Multiplier::ONE),
                },
            )]),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            affinity_enabled_override: None,
            affinity_idle_ttl_seconds_override: None,
            affinity_failback_mode_override: None,
            affinity_failback_delay_seconds_override: None,

            proxy_url: None,
            extra_headers: None,
            session_affinity_auto: None,
        },
    )
    .await;
    seed_group_routing_provider(
        &state,
        "team-a-provider",
        false,
        team_a_group.id.clone(),
        CreateMonoizeChannelInput {
            name: "team-a".to_string(),
            provider_type: MonoizeProviderType::Responses,
            base_url: "https://team-a.example.com".to_string(),
            api_key: Some("secret".to_string()),
            enabled: true,
            allow_missing_usage: false,
            passive_failure_count_threshold_override: None,
            passive_cooldown_seconds_override: None,
            passive_window_seconds_override: None,
            passive_rate_limit_cooldown_seconds_override: None,
            models: std::collections::HashMap::from([(
                GROUP_ROUTING_MODEL.to_string(),
                MonoizeModelEntry {
                    redirect: None,
                    pricing_profile_mode: Default::default(),
                    pricing_profile_override: None,
                    multiplier_override: Some(Multiplier::ONE),
                },
            )]),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            affinity_enabled_override: None,
            affinity_idle_ttl_seconds_override: None,
            affinity_failback_mode_override: None,
            affinity_failback_delay_seconds_override: None,

            proxy_url: None,
            extra_headers: None,
            session_affinity_auto: None,
        },
    )
    .await;
    seed_group_routing_provider(
        &state,
        "team-b-provider",
        false,
        team_b_group.id.clone(),
        CreateMonoizeChannelInput {
            name: "team-b".to_string(),
            provider_type: MonoizeProviderType::Responses,
            base_url: "https://team-b.example.com".to_string(),
            api_key: Some("secret".to_string()),
            enabled: true,
            allow_missing_usage: false,
            passive_failure_count_threshold_override: None,
            passive_cooldown_seconds_override: None,
            passive_window_seconds_override: None,
            passive_rate_limit_cooldown_seconds_override: None,
            models: std::collections::HashMap::from([(
                GROUP_ROUTING_MODEL.to_string(),
                MonoizeModelEntry {
                    redirect: None,
                    pricing_profile_mode: Default::default(),
                    pricing_profile_override: None,
                    multiplier_override: Some(Multiplier::ONE),
                },
            )]),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            affinity_enabled_override: None,
            affinity_idle_ttl_seconds_override: None,
            affinity_failback_mode_override: None,
            affinity_failback_delay_seconds_override: None,

            proxy_url: None,
            extra_headers: None,
            session_affinity_auto: None,
        },
    )
    .await;
    seed_model_pricing(&state, GROUP_ROUTING_MODEL).await;

    let req = build_test_routing_request(GROUP_ROUTING_MODEL);

    // R-GRP-1: None marks internal traffic and bypasses group filtering.
    let unrestricted_auth = build_test_auth(None);
    let unrestricted = build_monoize_attempts(&state, &req, &unrestricted_auth)
        .await
        .expect("unrestricted routing succeeds");
    let team_a_auth = build_test_auth(Some(vec![team_a_group.id.clone()]));
    let team_a = build_monoize_attempts(&state, &req, &team_a_auth)
        .await
        .expect("team-a routing succeeds");
    let default_only_auth = build_test_auth(Some(vec![default_group_id]));
    let default_only = build_monoize_attempts(&state, &req, &default_only_auth)
        .await
        .expect("default-group routing succeeds");

    assert_eq!(
        attempt_channel_names(&unrestricted),
        BTreeSet::from(["public", "team-a", "team-b"])
    );
    assert_eq!(attempt_channel_names(&team_a), BTreeSet::from(["team-a"]));
    assert_eq!(
        attempt_channel_names(&default_only),
        BTreeSet::from(["public"])
    );
}

const RTA9_MODEL: &str = "gpt-rta9-guard";

/// Seeds one provider serving RTA9_MODEL with the given cache-routing guard fields.
async fn seed_rta9_provider(
    state: &AppState,
    name: &str,
    channel_name: &str,
    max_input_tokens: Option<u64>,
    prompt_cache_incompatible_with_tools: Option<bool>,
) {
    state
        .monoize_store
        .create_provider(CreateMonoizeProviderInput {
            confirm_public_exposure: true,
            pricing_profile: Some("openai".to_string()),
            multiplier: Default::default(),
            name: name.to_string(),
            channel_max_retries: 0,
            channel_retry_interval_ms: 0,
            circuit_breaker_enabled: false,
            per_model_circuit_break: false,
            transforms: Vec::new(),
            api_type_overrides: Vec::new(),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            request_timeout_ms_override: None,
            max_input_tokens,
            prompt_cache_incompatible_with_tools,
            extra_fields_whitelist: None,
            strip_cross_protocol_nested_extra: None,
            enabled: true,
            priority: Some(0),
            group_id: String::new(),
            channel: CreateMonoizeChannelInput {
                name: channel_name.to_string(),
                provider_type: MonoizeProviderType::Responses,
                base_url: format!("https://{channel_name}.example.com"),
                api_key: Some("secret".to_string()),
                enabled: true,
                allow_missing_usage: false,
                passive_failure_count_threshold_override: None,
                passive_cooldown_seconds_override: None,
                passive_window_seconds_override: None,
                passive_rate_limit_cooldown_seconds_override: None,
                models: std::collections::HashMap::from([(
                    RTA9_MODEL.to_string(),
                    MonoizeModelEntry {
                        redirect: None,
                        pricing_profile_mode: Default::default(),
                        pricing_profile_override: None,
                        multiplier_override: Some(Multiplier::ONE),
                    },
                )]),
                active_probe_enabled_override: None,
                active_probe_interval_seconds_override: None,
                active_probe_success_threshold_override: None,
                active_probe_model_override: None,
                affinity_enabled_override: None,
                affinity_idle_ttl_seconds_override: None,
                affinity_failback_mode_override: None,
                affinity_failback_delay_seconds_override: None,
                proxy_url: None,
                extra_headers: None,
                session_affinity_auto: None,
            },
        })
        .await
        .expect("provider created");
}

fn rta9_request(estimated_input_tokens: u64, has_tools: bool) -> UrpRequest {
    UrpRequest {
            audio_output_format: Default::default(),
            messages_custom_tool_names: Default::default(),
        model: RTA9_MODEL.to_string(),
        max_multiplier: None,
        server_tool_usage_classes: Vec::new(),
        affinity_explicit: None,
        affinity_prefix_hash: crate::handlers::helpers::short_xxh3_hex(RTA9_MODEL),
        estimated_input_tokens,
        has_tools,
    }
}

async fn rta9_state() -> AppState {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    seed_model_pricing(&state, RTA9_MODEL).await;
    state
}

#[tokio::test]
async fn rta9_skips_provider_when_estimated_input_tokens_exceed_the_limit() {
    let state = rta9_state().await;
    seed_rta9_provider(&state, "capped", "capped-channel", Some(500), None).await;
    seed_rta9_provider(&state, "fallback", "fallback-channel", None, None).await;

    let auth = build_test_auth(None);
    let over = rta9_request(501, false);
    let attempts = build_monoize_attempts(&state, &over, &auth)
        .await
        .expect("routing succeeds");
    assert_eq!(
        attempt_channel_names(&attempts),
        BTreeSet::from(["fallback-channel"])
    );

    let at_limit = rta9_request(500, false);
    let attempts = build_monoize_attempts(&state, &at_limit, &auth)
        .await
        .expect("routing succeeds");
    assert_eq!(
        attempt_channel_names(&attempts),
        BTreeSet::from(["capped-channel", "fallback-channel"]),
        "the limit itself must not skip: strictly greater than excludes"
    );
}

#[tokio::test]
async fn rta9_skips_provider_for_tool_bearing_requests_when_flagged() {
    let state = rta9_state().await;
    seed_rta9_provider(
        &state,
        "tools-hostile",
        "tools-hostile-channel",
        None,
        Some(true),
    )
    .await;
    seed_rta9_provider(&state, "neutral", "neutral-channel", None, None).await;

    let auth = build_test_auth(None);
    let with_tools = rta9_request(0, true);
    let attempts = build_monoize_attempts(&state, &with_tools, &auth)
        .await
        .expect("routing succeeds");
    assert_eq!(
        attempt_channel_names(&attempts),
        BTreeSet::from(["neutral-channel"])
    );

    let without_tools = rta9_request(0, false);
    let attempts = build_monoize_attempts(&state, &without_tools, &auth)
        .await
        .expect("routing succeeds");
    assert_eq!(
        attempt_channel_names(&attempts),
        BTreeSet::from(["tools-hostile-channel", "neutral-channel"])
    );
}

#[tokio::test]
async fn rta9_defaults_keep_every_provider_eligible() {
    let state = rta9_state().await;
    seed_rta9_provider(&state, "unguarded", "unguarded-channel", None, None).await;

    let auth = build_test_auth(None);
    let big_tools_request = rta9_request(u64::MAX, true);
    let attempts = build_monoize_attempts(&state, &big_tools_request, &auth)
        .await
        .expect("routing succeeds");
    assert_eq!(
        attempt_channel_names(&attempts),
        BTreeSet::from(["unguarded-channel"])
    );
}

#[tokio::test]
async fn rta9_skipping_the_only_provider_yields_zero_attempts() {
    let state = rta9_state().await;
    seed_rta9_provider(&state, "capped", "capped-channel", Some(100), None).await;

    let auth = build_test_auth(None);
    let over = rta9_request(101, false);
    let attempts = build_monoize_attempts(&state, &over, &auth)
        .await
        .expect("routing succeeds without contacting upstream");
    assert!(
        attempts.is_empty(),
        "RTA-8b zero-attempt path: no attempts remain"
    );
}

#[tokio::test]
async fn execute_nonstream_typed_keeps_bad_gateway_when_groups_filter_every_channel() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let team_a_group = state
        .user_store
        .create_group(crate::users::CreateGroupInput {
            confirm_public_exposure: true,
            name: "team-a".to_string(),
            description: String::new(),
            user_selectable: true,
            sort_order: 1,
            account_class: Default::default(),
        })
        .await
        .expect("team-a group created");
    let team_b_group = state
        .user_store
        .create_group(crate::users::CreateGroupInput {
            confirm_public_exposure: true,
            name: "team-b".to_string(),
            description: String::new(),
            user_selectable: true,
            sort_order: 2,
            account_class: Default::default(),
        })
        .await
        .expect("team-b group created");

    seed_group_routing_provider(
        &state,
        "team-a-provider",
        true,
        team_a_group.id.clone(),
        CreateMonoizeChannelInput {
            name: "team-a".to_string(),
            provider_type: MonoizeProviderType::Responses,
            base_url: "https://team-a.example.com".to_string(),
            api_key: Some("secret".to_string()),
            enabled: true,
            allow_missing_usage: false,
            passive_failure_count_threshold_override: None,
            passive_cooldown_seconds_override: None,
            passive_window_seconds_override: None,
            passive_rate_limit_cooldown_seconds_override: None,
            models: std::collections::HashMap::from([(
                GROUP_ROUTING_MODEL.to_string(),
                MonoizeModelEntry {
                    redirect: None,
                    pricing_profile_mode: Default::default(),
                    pricing_profile_override: None,
                    multiplier_override: Some(Multiplier::ONE),
                },
            )]),
            active_probe_enabled_override: None,
            active_probe_interval_seconds_override: None,
            active_probe_success_threshold_override: None,
            active_probe_model_override: None,
            affinity_enabled_override: None,
            affinity_idle_ttl_seconds_override: None,
            affinity_failback_mode_override: None,
            affinity_failback_delay_seconds_override: None,

            proxy_url: None,
            extra_headers: None,
            session_affinity_auto: None,
        },
    )
    .await;

    let err = execute_nonstream_typed(
        &state,
        &build_test_auth(Some(vec![team_b_group.id.clone()])),
        build_test_urp_request(GROUP_ROUTING_MODEL),
        None,
        DownstreamProtocol::ChatCompletions,
        None,
        None,
        None,
        RequestCaptureContext {
            raw_input: std::sync::Arc::new(json!({})),
            session: None,
        },
    )
    .await
    .expect_err("non-overlapping group restriction should leave no attempts");

    // RTA-8b: no attempt was built, so nothing upstream was contacted. The model is served
    // for this account class and only the Group restriction blocked it, which is the second
    // row of the table.
    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.code, "no_healthy_upstream");
    assert_eq!(
        err.message,
        format!("No available upstream provider for model: {GROUP_ROUTING_MODEL}")
    );
}

/// RTA-8b first row: a model string that names nothing the deployment serves is the caller's
/// error, not a gateway failure. It has to be a `4xx`, both because that is accurate and
/// because an edge that replaces the body of an origin `5xx` would otherwise hide the reason.
#[tokio::test]
async fn unknown_model_is_not_found_rather_than_a_bad_gateway() {
    let state = load_state_with_runtime(RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    })
    .await
    .expect("state loads");

    let err = execute_nonstream_typed(
        &state,
        &build_test_auth(None),
        build_test_urp_request("no-such-model@think"),
        None,
        DownstreamProtocol::ChatCompletions,
        None,
        None,
        None,
        RequestCaptureContext {
            raw_input: std::sync::Arc::new(json!({})),
            session: None,
        },
    )
    .await
    .expect_err("an unrouted model must not reach an upstream");

    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(err.code, "model_not_found");
    assert_eq!(err.message, "Model not found: no-such-model@think");
    // SAN-6: the message may name the model the caller sent and nothing else.
    for leaked in ["provider", "channel", "group", "http"] {
        assert!(
            !err.message.to_ascii_lowercase().contains(leaked),
            "{leaked} leaked into {}",
            err.message
        );
    }
}

#[test]
fn exhausted_upstream_error_preserves_final_machine_code() {
    let tried = vec![TriedProvider {
        attempt_number: 1,
        provider_id: "provider-a".to_string(),
        channel_id: "channel-a".to_string(),
        provider_name: "Provider A".to_string(),
        channel_name: "Channel A".to_string(),
        error: "upstream status 400 Bad Request: encrypted content could not be verified"
            .to_string(),
        client_error: "encrypted content could not be verified".to_string(),
        upstream_status: Some(StatusCode::BAD_REQUEST.as_u16()),
        upstream_code: Some("thinking_signature_invalid".to_string()),
        upstream_type: Some("invalid_request_error".to_string()),
        upstream_param: None,
        duration_ms: Some(12),
    }];

    let err = build_exhausted_upstream_error("gpt-5.6-sol", &tried);

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(err.code, "thinking_signature_invalid");
    assert_eq!(err.message, "encrypted content could not be verified");
    assert_eq!(
        err.internal_message.as_deref(),
        Some("upstream status 400 Bad Request: encrypted content could not be verified")
    );
    assert_eq!(
        err.upstream_code.as_deref(),
        Some("thinking_signature_invalid")
    );
}

const LEAKY_TRANSPORT_ERROR: &str = "error sending request for url (https://api.cloudflare.com/client/v4/accounts/ebb3b05a7371fbcbd62bde8264c86cfe/ai/v1/chat/completions)";

#[test]
fn upstream_error_to_app_replaces_transport_detail_with_generic_message() {
    let err = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Network,
            None,
            LEAKY_TRANSPORT_ERROR.to_string(),
        ),
        true,
    );

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    // SAN-16: the client text is Monoize-authored and interpolates no upstream value.
    assert_eq!(err.message, "the upstream provider could not be reached");
    // SAN-2: the admin-tier internal detail keeps the raw transport text.
    let internal = err.internal_message.as_deref().expect("internal detail");
    assert!(
        internal.starts_with("upstream status 502 Bad Gateway: "),
        "{internal}"
    );
    assert!(internal.contains("api.cloudflare.com"), "{internal}");
    assert!(
        internal.contains("ebb3b05a7371fbcbd62bde8264c86cfe"),
        "{internal}"
    );
}

#[test]
fn upstream_error_to_app_drops_unparsed_error_body_from_client_message() {
    let raw_body = format!("<html>502 Bad Gateway from {LEAKY_TRANSPORT_ERROR}</html>");
    let err = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Http,
            Some(StatusCode::BAD_GATEWAY),
            raw_body,
        )
        .with_source(upstream::UpstreamErrorSource::UnparsedBody),
        true,
    );

    // SAN-16: a 5xx upstream maps to one constant; the status is carried structurally.
    assert_eq!(
        err.message,
        "the upstream provider reported an internal error"
    );
    // SAN-2: the raw unparsed body stays admin-readable in internal detail.
    let internal = err.internal_message.as_deref().expect("internal detail");
    assert!(internal.contains("api.cloudflare.com"), "{internal}");
    assert!(internal.contains("<html>502 Bad Gateway"), "{internal}");
}

#[test]
fn upstream_error_to_app_maps_structured_message_to_authored_text() {
    // SAN-16: a structured upstream error no longer reaches the client as text. The 422 maps
    // to one constant, and the raw wording stays in the admin-tier internal detail.
    let err = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Http,
            Some(StatusCode::UNPROCESSABLE_ENTITY),
            "invalid request".to_string(),
        )
        .with_source(upstream::UpstreamErrorSource::StructuredBody),
        true,
    );
    assert_eq!(
        err.message,
        "the upstream provider rejected a request parameter"
    );
    assert!(
        err.internal_message
            .as_deref()
            .expect("internal detail")
            .contains("invalid request")
    );

    // A structured error naming infrastructure loses it entirely, not just to `MASK`.
    let leaky = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Http,
            Some(StatusCode::UNPROCESSABLE_ENTITY),
            "rejected by https://api.cloudflare.com/client/v4/accounts/abc123/ai".to_string(),
        )
        .with_source(upstream::UpstreamErrorSource::StructuredBody),
        true,
    );
    assert_eq!(
        leaky.message,
        "the upstream provider rejected a request parameter"
    );
    assert!(!leaky.message.contains("cloudflare"), "{}", leaky.message);
    assert!(!leaky.message.contains("***"), "{}", leaky.message);
}

// SAN-CFG5: with `mask_sensitive_info` disabled, the client message carries
// the raw upstream detail (TRUNC-bounded for transport/unparsed sources).
// SAN-15a: `monoize_mask_sensitive_info` governs `MASK` only. Disabling it MUST NOT expose
// upstream detail to a client, because SAN-16 selects the client text before `MASK` applies.
#[test]
fn upstream_error_to_app_keeps_authored_text_when_masking_disabled() {
    for (kind, status, source, raw, expected) in [
        (
            UpstreamErrorKind::Network,
            None,
            upstream::UpstreamErrorSource::Transport,
            LEAKY_TRANSPORT_ERROR.to_string(),
            "the upstream provider could not be reached",
        ),
        (
            UpstreamErrorKind::Http,
            Some(StatusCode::BAD_GATEWAY),
            upstream::UpstreamErrorSource::UnparsedBody,
            format!("<html>502 from {LEAKY_TRANSPORT_ERROR}</html>"),
            "the upstream provider reported an internal error",
        ),
        (
            UpstreamErrorKind::Http,
            Some(StatusCode::UNPROCESSABLE_ENTITY),
            upstream::UpstreamErrorSource::StructuredBody,
            "rejected by https://api.cloudflare.com/client/v4/accounts/abc123/ai".to_string(),
            "the upstream provider rejected a request parameter",
        ),
    ] {
        let err = routing::upstream_error_to_app(
            UpstreamCallError::new(kind, status, raw.clone()).with_source(source),
            false,
        );
        assert_eq!(err.message, expected, "raw was: {raw}");
        assert!(!err.message.contains("cloudflare"), "{}", err.message);
        assert!(
            !err.message.contains("ebb3b05a7371fbcbd62bde8264c86cfe"),
            "{}",
            err.message
        );
        // The operator still sees everything.
        assert!(
            err.internal_message
                .as_deref()
                .expect("internal detail")
                .contains(raw.trim_start_matches('<').split(' ').next().unwrap_or("")),
            "internal detail lost the raw text: {:?}",
            err.internal_message
        );
    }
}

// SAN-CFG5.2/5.3: transport and unparsed client text is TRUNC-bounded when
// masking is off, so an oversized raw body cannot flood the client message.
// SAN-16a: the client text is a constant, so an oversized raw body cannot reach the client at
// all. TRUNC still bounds the admin-tier internal detail.
#[test]
fn upstream_error_to_app_bounds_internal_detail_and_never_floods_the_client() {
    let oversized = "x".repeat(3000);
    let err = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Http,
            Some(StatusCode::BAD_GATEWAY),
            oversized,
        )
        .with_source(upstream::UpstreamErrorSource::UnparsedBody),
        false,
    );
    assert_eq!(
        err.message,
        "the upstream provider reported an internal error"
    );
    let internal = err.internal_message.as_deref().expect("internal detail");
    assert!(internal.ends_with("... (truncated)"), "{internal}");
    assert_eq!(
        internal.chars().count(),
        "upstream status 502 Bad Gateway: ".chars().count()
            + 2048
            + "... (truncated)".chars().count()
    );
}

#[test]
fn exhausted_error_message_is_one_constant_and_names_nothing() {
    let attempt = affinity_test_attempt(
        "provider-leak",
        "channel-leak",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    let app_err = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Network,
            Some(StatusCode::BAD_GATEWAY),
            LEAKY_TRANSPORT_ERROR.to_string(),
        ),
        true,
    );
    let tried = vec![
        TriedProvider::from_app_error(1, &attempt, &app_err, Some(10), true),
        TriedProvider::from_app_error(2, &attempt, &app_err, Some(11), true),
    ];

    let err = build_exhausted_upstream_error("deepseek-v4-flash", &tried);

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    // SAN-16b: no model, no attempt count, no upstream text.
    assert_eq!(err.message, "no upstream provider could serve this request");
    assert!(
        !err.message.contains("deepseek-v4-flash"),
        "{}",
        err.message
    );
    assert!(!err.message.contains("cloudflare"), "{}", err.message);

    // SAN-7 / SAN-16a: the admin-tier internal detail keeps the model, the attempt count,
    // and the raw last-attempt error.
    let internal = err.internal_message.as_deref().expect("internal detail");
    assert!(
        internal.starts_with("All 2 upstream attempt(s) failed for model: deepseek-v4-flash."),
        "{internal}"
    );
    assert!(internal.contains("api.cloudflare.com"), "{internal}");
    assert!(
        internal.contains("ebb3b05a7371fbcbd62bde8264c86cfe"),
        "{internal}"
    );
}

// SAN-CFG5 item 1: with masking disabled `client_error` equals the raw
// AppError message, so the exhausted downstream error carries full detail.
// SAN-15a / SAN-17: with masking disabled the client-facing attempt text is still authored by
// Monoize, and the exhausted message still names no infrastructure.
#[test]
fn tried_provider_client_error_stays_authored_when_masking_disabled() {
    let attempt = affinity_test_attempt(
        "provider-raw",
        "channel-raw",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    let app_err = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Network,
            Some(StatusCode::BAD_GATEWAY),
            LEAKY_TRANSPORT_ERROR.to_string(),
        ),
        false,
    );
    let tried = vec![TriedProvider::from_app_error(
        1,
        &attempt,
        &app_err,
        Some(10),
        false,
    )];

    // The attempt carried a 502, so SAN-16 selects the 5xx arm rather than the transport one.
    assert_eq!(
        tried[0].client_error,
        "the upstream provider reported an internal error"
    );
    assert!(!tried[0].client_error.contains("cloudflare"));
    // SAN-5: the persisted internal detail is untouched.
    assert!(
        tried[0].error.contains("api.cloudflare.com"),
        "{}",
        tried[0].error
    );

    let err = build_exhausted_upstream_error("deepseek-v4-flash", &tried);
    assert!(
        !err.message.contains("api.cloudflare.com"),
        "{}",
        err.message
    );
}

// SAN-2a: when the last attempt was a quota failure, the exhausted-routing
// downstream message composes the fixed generic quota text instead of the
// upstream wording, even with masking disabled.
// SAN-16b: the exhausted message is one constant, so quota wording cannot appear in it
// either. SAN-16 keeps the per-attempt quota text on the attempt's own client_error.
#[test]
fn exhausted_error_carries_no_quota_wording() {
    let attempt = affinity_test_attempt(
        "provider-quota",
        "channel-quota",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    let app_err = routing::upstream_error_to_app(
        UpstreamCallError::new(
            UpstreamErrorKind::Http,
            Some(StatusCode::TOO_MANY_REQUESTS),
            "You have exceeded your 5 hour quota; resets 2026-09-15T21:00:00Z".to_string(),
        ),
        false,
    );
    // SAN-D2a still classifies the attempt itself as a quota failure.
    assert_eq!(app_err.message, crate::error_sanitize::GENERIC_QUOTA_TEXT);
    assert!(!app_err.message.contains("5 hour"), "{}", app_err.message);

    let tried = vec![TriedProvider::from_app_error(
        1,
        &attempt,
        &app_err,
        Some(10),
        false,
    )];
    let err = build_exhausted_upstream_error("glm-5.3", &tried);
    assert_eq!(err.message, "no upstream provider could serve this request");
    assert!(!err.message.contains("5 hour"), "{}", err.message);
    assert!(!err.message.contains("resets 2026"), "{}", err.message);
    assert!(!err.message.contains("glm-5.3"), "{}", err.message);
}

#[test]
fn channel_origin_key_groups_same_host_independent_of_path_and_case() {
    assert_eq!(
        routing::channel_origin_key("https://input.codes"),
        routing::channel_origin_key("https://INPUT.CODES/v1/")
    );
    assert_eq!(
        routing::channel_origin_key("https://input.codes").as_deref(),
        Some("https://input.codes:443")
    );
    assert_ne!(
        routing::channel_origin_key("https://input.codes"),
        routing::channel_origin_key("https://codex.ciii.club")
    );
    assert!(routing::channel_origin_key("not a url").is_none());
}

#[test]
fn upstream_path_for_model_percent_encodes_injected_segments() {
    use crate::config::ProviderType;

    // Gemini: the `models/` prefix is structural, the name itself is a single
    // segment, so a `/` inside the name must not open a new path segment.
    assert_eq!(
        routing::upstream_path_for_model(ProviderType::Gemini, "models/gemini-2.5-pro", false),
        "/v1beta/models/models/gemini-2.5-pro:generateContent"
    );
    assert_eq!(
        routing::upstream_path_for_model(ProviderType::Gemini, "gemini-2.5-pro/extra?x=1#f", true),
        "/v1beta/models/gemini-2.5-pro%2Fextra%3Fx=1%23f:streamGenerateContent?alt=sse"
    );

    // Replicate plain model: encode as a single segment, so a `/` inside the key
    // cannot open a new path segment. (A `:` routes to the version-qualified
    // `/v1/predictions` form instead, so use a colon-free key here.)
    assert_eq!(
        routing::upstream_path_for_model(ProviderType::Replicate, "owner/na me?k=v", false),
        "/v1/models/owner%2Fna%20me%3Fk=v/predictions"
    );

    // Replicate deployment: owner and name stay independent segments, only the
    // owner/name contents are encoded.
    assert_eq!(
        routing::upstream_path_for_model(ProviderType::Replicate, "deployment:owner/name", false),
        "/v1/deployments/owner/name/predictions"
    );
    assert_eq!(
        routing::upstream_path_for_model(
            ProviderType::Replicate,
            "deployment:owner/na me#1",
            false
        ),
        "/v1/deployments/owner/na%20me%231/predictions"
    );
}

#[test]
fn shared_origin_status_covers_502_503_524_only() {
    assert!(routing::is_shared_origin_status(Some(502)));
    assert!(routing::is_shared_origin_status(Some(503)));
    assert!(routing::is_shared_origin_status(Some(524)));
    assert!(!routing::is_shared_origin_status(Some(429)));
    assert!(!routing::is_shared_origin_status(Some(500)));
    assert!(!routing::is_shared_origin_status(None));
}

#[tokio::test]
async fn shared_origin_blast_marks_peer_channels_and_skips_without_clearing_affinity() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "og86dfgj",
        "input-1",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.channel_id = "input-1".to_string();
    attempt.base_url = "https://input.codes".to_string();
    attempt.origin_key = routing::channel_origin_key(&attempt.base_url);
    attempt.origin_peer_channel_ids = vec!["input-1".to_string(), "input-2".to_string()];
    attempt.passive_failure_count_threshold = 1;
    attempt.affinity_key = Some("v1|api_key:k|model:gpt-affinity|prefix:x".to_string());
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);
    state.channel_affinity.lock().await.insert(
        attempt.affinity_key.clone().expect("key"),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: attempt.provider_id.clone(),
            channel_id: attempt.channel_id.clone(),
            bound_at: now_ts(),
            last_used_at: now_ts(),
            expires_at: now_ts() + 1800,
        },
    );

    let err = AppError::new(
        StatusCode::BAD_GATEWAY,
        "upstream_error",
        "upstream status 502 Bad Gateway: error code: 502",
    )
    .with_upstream_error(Some(StatusCode::BAD_GATEWAY), None, None, None);
    let mut tried = Vec::new();
    let mut execution_state = AttemptExecutionState::default();
    routing::record_upstream_attempt_failure(
        &state,
        &attempt,
        1,
        &err,
        Some(routing::RetryableFailureClass::Transient),
        &mut tried,
        &mut execution_state,
    )
    .await;

    assert!(execution_state.should_skip(&attempt));
    let health = state.channel_health.lock().await;
    assert!(!health.get("input-1").expect("input-1 health").healthy);
    assert!(!health.get("input-2").expect("input-2 health").healthy);
    drop(health);
    assert!(
        state
            .channel_affinity
            .lock()
            .await
            .contains_key(attempt.affinity_key.as_ref().expect("key"))
    );
}

#[test]
fn midstream_terminal_failure_class_maps_breaker_relevant_signals() {
    assert_eq!(
        routing::midstream_terminal_failure_class(429, None, None),
        Some(routing::RetryableFailureClass::RateLimited)
    );
    assert_eq!(
        routing::midstream_terminal_failure_class(408, None, None),
        Some(routing::RetryableFailureClass::Transient)
    );
    for status in [500, 502, 503, 529, 599] {
        assert_eq!(
            routing::midstream_terminal_failure_class(status, None, None),
            Some(routing::RetryableFailureClass::Transient),
            "{status}"
        );
    }
    for status in [401, 402, 403, 404, 405, 407, 410, 415, 426, 451] {
        assert_eq!(
            routing::midstream_terminal_failure_class(status, None, None),
            Some(routing::RetryableFailureClass::Persistent),
            "{status}"
        );
    }
    for status in [200, 400, 409, 422] {
        assert_eq!(
            routing::midstream_terminal_failure_class(status, None, None),
            None,
            "{status}"
        );
    }

    // RTA-5c: model-scoped signals never update Channel health in any class.
    assert_eq!(
        routing::midstream_terminal_failure_class(400, None, Some("model_not_found")),
        None
    );
    assert_eq!(
        routing::midstream_terminal_failure_class(400, Some("rate_limit_exceeded"), None),
        Some(routing::RetryableFailureClass::RateLimited)
    );
    assert_eq!(
        routing::midstream_terminal_failure_class(400, Some("overloaded_error"), None),
        Some(routing::RetryableFailureClass::Transient)
    );
    assert_eq!(
        routing::midstream_terminal_failure_class(403, Some("rate_limit_exceeded"), None),
        Some(routing::RetryableFailureClass::Persistent)
    );
    assert_eq!(
        routing::midstream_terminal_failure_class(503, None, Some("model_not_found")),
        None
    );
    assert_eq!(
        routing::midstream_terminal_failure_class(404, Some("model_not_found"), None),
        None
    );
    assert_eq!(
        routing::midstream_terminal_failure_class(503, None, Some("unsupported_model")),
        None
    );
    // Non-model-scoped signals keep their RTA-5a classes.
    assert_eq!(
        routing::midstream_terminal_failure_class(503, Some("invalid_api_key"), None),
        Some(routing::RetryableFailureClass::Persistent)
    );
}

#[tokio::test]
async fn persistent_failure_trips_breaker_before_sample_threshold() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "persistent-failure-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.passive_failure_count_threshold = 3;
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);

    routing::mark_channel_retryable_failure(
        &state,
        &attempt,
        routing::RetryableFailureClass::Persistent,
    )
    .await;

    let health = state.channel_health.lock().await;
    let entry = health
        .get("persistent-failure-channel")
        .expect("health entry exists");
    assert!(!entry.healthy);
    assert_eq!(entry.passive_failure_timestamps.len(), 1);
}

#[test]
fn upstream_adapter_failure_classification_uses_upstream_prefix() {
    for code in [
        "upstream_idle_timeout",
        "upstream_stream_error",
        "upstream_incomplete_stream",
    ] {
        let err = AppError::new(StatusCode::BAD_GATEWAY, code, "adapter failure");
        assert!(routing::is_upstream_adapter_failure(&err), "{code}");
    }
    for code in [
        "invalid_upstream_response",
        "encode_error",
        "internal_error",
    ] {
        let err = AppError::new(StatusCode::BAD_GATEWAY, code, "internal failure");
        assert!(!routing::is_upstream_adapter_failure(&err), "{code}");
    }
}

#[test]
fn same_channel_attempt_slots_reserve_one_extra_for_affinity_hit() {
    let mut attempt = affinity_test_attempt(
        "provider",
        "slots-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.channel_max_retries = 0;
    assert_eq!(routing::same_channel_attempt_slots(&attempt), 1);

    attempt.channel_max_retries = 2;
    assert_eq!(routing::same_channel_attempt_slots(&attempt), 3);

    attempt.affinity_hit = Some(true);
    assert_eq!(routing::same_channel_attempt_slots(&attempt), 4);

    attempt.affinity_hit = Some(false);
    assert_eq!(routing::same_channel_attempt_slots(&attempt), 3);
}

#[tokio::test]
async fn bound_target_earns_one_extra_transient_attempt_but_not_for_rate_limits() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "extra-slot-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.channel_max_retries = 0;
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);
    let execution_state = AttemptExecutionState::default();

    // RTA-4: without an affinity hit, channel_max_retries = 0 permits no retry.
    assert!(
        !routing::allow_same_channel_retry(
            &state,
            &attempt,
            &execution_state,
            1,
            Some(routing::RetryableFailureClass::Transient),
        )
        .await
    );

    // RTA-4a: the bound target earns exactly one extra Transient attempt.
    attempt.affinity_hit = Some(true);
    assert!(
        routing::allow_same_channel_retry(
            &state,
            &attempt,
            &execution_state,
            1,
            Some(routing::RetryableFailureClass::Transient),
        )
        .await
    );
    assert!(
        !routing::allow_same_channel_retry(
            &state,
            &attempt,
            &execution_state,
            2,
            Some(routing::RetryableFailureClass::Transient),
        )
        .await
    );

    // RTA-4a: a 429 never consumes the extra slot.
    assert!(
        !routing::allow_same_channel_retry(
            &state,
            &attempt,
            &execution_state,
            1,
            Some(routing::RetryableFailureClass::RateLimited),
        )
        .await
    );

    // A non-retryable failure never re-enters the same Channel.
    assert!(!routing::allow_same_channel_retry(&state, &attempt, &execution_state, 1, None).await);
}

#[tokio::test]
async fn same_channel_retry_requires_healthy_channel_and_no_shared_origin_skip() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "gated-retry-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.channel_max_retries = 1;
    attempt.passive_failure_count_threshold = 1;
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);

    let mut execution_state = AttemptExecutionState::default();
    attempt.origin_key = Some("https|origin.example|443".to_string());
    execution_state.mark_shared_origin_skip(&attempt);
    assert!(
        !routing::allow_same_channel_retry(
            &state,
            &attempt,
            &execution_state,
            1,
            Some(routing::RetryableFailureClass::Transient),
        )
        .await
    );

    let execution_state = AttemptExecutionState::default();
    routing::mark_channel_retryable_failure(
        &state,
        &attempt,
        routing::RetryableFailureClass::Transient,
    )
    .await;
    assert!(
        !routing::allow_same_channel_retry(
            &state,
            &attempt,
            &execution_state,
            1,
            Some(routing::RetryableFailureClass::Transient),
        )
        .await
    );
}

#[tokio::test]
async fn sub_threshold_failure_on_bound_target_keeps_affinity_binding() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "sticky-keep-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.passive_failure_count_threshold = 3;
    attempt.affinity_hit = Some(true);
    attempt.affinity_key = Some("v1|api_key:k|model:gpt-affinity|prefix:keep".to_string());
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);
    state.channel_affinity.lock().await.insert(
        attempt.affinity_key.clone().expect("key"),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: attempt.provider_id.clone(),
            channel_id: attempt.channel_id.clone(),
            bound_at: now_ts(),
            last_used_at: now_ts(),
            expires_at: now_ts() + 1800,
        },
    );

    // 500 is Transient but not a shared-origin status; one sample stays below
    // the threshold of 3, so AFF-9 keeps the binding.
    let err = AppError::new(
        StatusCode::BAD_GATEWAY,
        "upstream_error",
        "upstream status 500",
    )
    .with_upstream_error(Some(StatusCode::INTERNAL_SERVER_ERROR), None, None, None);
    let mut tried = Vec::new();
    let mut execution_state = AttemptExecutionState::default();
    routing::record_upstream_attempt_failure(
        &state,
        &attempt,
        1,
        &err,
        Some(routing::RetryableFailureClass::Transient),
        &mut tried,
        &mut execution_state,
    )
    .await;

    assert!(
        state
            .channel_affinity
            .lock()
            .await
            .contains_key(attempt.affinity_key.as_ref().expect("key"))
    );
    let health = state.channel_health.lock().await;
    assert!(health.get("sticky-keep-channel").expect("entry").healthy);
}

#[tokio::test]
async fn breaker_tripping_failure_on_bound_target_clears_affinity_binding() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "sticky-clear-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.passive_failure_count_threshold = 1;
    attempt.affinity_hit = Some(true);
    attempt.affinity_key = Some("v1|api_key:k|model:gpt-affinity|prefix:clear".to_string());
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);
    state.channel_affinity.lock().await.insert(
        attempt.affinity_key.clone().expect("key"),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: attempt.provider_id.clone(),
            channel_id: attempt.channel_id.clone(),
            bound_at: now_ts(),
            last_used_at: now_ts(),
            expires_at: now_ts() + 1800,
        },
    );

    let err = AppError::new(
        StatusCode::BAD_GATEWAY,
        "upstream_error",
        "upstream status 500",
    )
    .with_upstream_error(Some(StatusCode::INTERNAL_SERVER_ERROR), None, None, None);
    let mut tried = Vec::new();
    let mut execution_state = AttemptExecutionState::default();
    routing::record_upstream_attempt_failure(
        &state,
        &attempt,
        1,
        &err,
        Some(routing::RetryableFailureClass::Transient),
        &mut tried,
        &mut execution_state,
    )
    .await;

    assert!(
        !state
            .channel_affinity
            .lock()
            .await
            .contains_key(attempt.affinity_key.as_ref().expect("key"))
    );
    let health = state.channel_health.lock().await;
    assert!(!health.get("sticky-clear-channel").expect("entry").healthy);
}

#[tokio::test]
async fn midstream_terminal_failure_samples_health_and_clears_affinity_only_on_trip() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "midstream-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.passive_failure_count_threshold = 2;
    attempt.affinity_hit = Some(true);
    attempt.affinity_key = Some("v1|api_key:k|model:gpt-affinity|prefix:mid".to_string());
    attempt.origin_key = routing::channel_origin_key("https://midstream.example");
    attempt.origin_peer_channel_ids = vec![
        "midstream-channel".to_string(),
        "midstream-peer".to_string(),
    ];
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);
    state.channel_affinity.lock().await.insert(
        attempt.affinity_key.clone().expect("key"),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: attempt.provider_id.clone(),
            channel_id: attempt.channel_id.clone(),
            bound_at: now_ts(),
            last_used_at: now_ts(),
            expires_at: now_ts() + 1800,
        },
    );

    // STRM-4a: the first sample stays below the threshold of 2 and keeps the binding.
    routing::record_midstream_terminal_failure(
        &state,
        &attempt,
        routing::RetryableFailureClass::Transient,
    )
    .await;
    assert!(
        state
            .channel_affinity
            .lock()
            .await
            .contains_key(attempt.affinity_key.as_ref().expect("key"))
    );

    // The second sample trips the breaker and clears the binding.
    routing::record_midstream_terminal_failure(
        &state,
        &attempt,
        routing::RetryableFailureClass::Transient,
    )
    .await;
    assert!(
        !state
            .channel_affinity
            .lock()
            .await
            .contains_key(attempt.affinity_key.as_ref().expect("key"))
    );
    let health = state.channel_health.lock().await;
    assert!(!health.get("midstream-channel").expect("entry").healthy);
    // STRM-4: mid-stream failures never blast shared-origin peers.
    assert!(!health.contains_key("midstream-peer"));
}

#[tokio::test]
async fn affinity_keeps_unexpired_binding_when_target_is_absent() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let request = build_test_routing_request("gpt-affinity");
    let auth = affinity_test_auth();
    let (key, _) = affinity_key_for_request(&request, &auth).expect("affinity key");
    let now = now_ts();
    state.channel_affinity.lock().await.insert(
        key.clone(),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: "provider-b".to_string(),
            channel_id: "channel-b1".to_string(),
            bound_at: now,
            last_used_at: now,
            expires_at: now + 1800,
        },
    );

    let attempts = apply_channel_affinity(
        &state,
        &request,
        &auth,
        vec![affinity_test_attempt(
            "provider-a",
            "channel-a1",
            crate::monoize_routing::AffinityFailbackMode::Sticky,
            30,
        )],
    )
    .await
    .expect("affinity applies");

    assert_eq!(attempts[0].affinity_hit, Some(false));
    assert_eq!(
        attempts[0].affinity_target.as_deref(),
        Some("provider-b/channel-b1")
    );
    assert!(state.channel_affinity.lock().await.contains_key(&key));
}

#[test]
fn apply_model_redirects_to_model_uses_first_match_wins() {
    let mut model = "claude-opus-4-6-20250610".to_string();
    apply_model_redirects_to_model(
        &mut model,
        &[
            build_model_redirect_rule(".*opus.*", "gpt-5.4"),
            build_model_redirect_rule("claude-.*", "gpt-5.4-mini"),
        ],
        &[],
    );

    assert_eq!(model, "gpt-5.4");
}

#[test]
fn apply_model_redirects_to_model_uses_global_rule_when_api_key_rules_miss() {
    let mut model = "claude-sonnet-5".to_string();
    apply_model_redirects_to_model(
        &mut model,
        &[build_model_redirect_rule(".*opus.*", "key-target")],
        &[build_model_redirect_rule("claude-.*", "global-target")],
    );

    assert_eq!(model, "global-target");
}

#[test]
fn apply_model_redirects_to_model_does_not_chain_global_after_api_key_match() {
    let mut model = "claude-sonnet-5".to_string();
    apply_model_redirects_to_model(
        &mut model,
        &[build_model_redirect_rule("claude-.*", "intermediate")],
        &[build_model_redirect_rule("intermediate", "global-target")],
    );

    assert_eq!(model, "intermediate");
}

#[test]
fn apply_model_redirects_to_model_leaves_unmatched_model_unchanged() {
    let mut model = "gpt-5-mini".to_string();
    apply_model_redirects_to_model(
        &mut model,
        &[build_model_redirect_rule(".*opus.*", "gpt-5.4")],
        &[build_model_redirect_rule("claude-.*", "gpt-5.4-mini")],
    );

    assert_eq!(model, "gpt-5-mini");
}

fn affinity_test_attempt(
    provider_id: &str,
    channel_id: &str,
    failback_mode: crate::monoize_routing::AffinityFailbackMode,
    failback_delay_seconds: u64,
) -> MonoizeAttempt {
    MonoizeAttempt {
        group_id: "group-1".to_string(),
        provider_id: provider_id.to_string(),
        provider_name: provider_id.to_string(),
        provider_type: ProviderType::Responses,
        channel_id: channel_id.to_string(),
        channel_name: channel_id.to_string(),
        base_url: "https://example.com".to_string(),
        api_key: "secret".to_string(),
        logical_model: "gpt-affinity".to_string(),
        upstream_model: "gpt-affinity".to_string(),
        pricing_profile: None,
        model_multiplier: Multiplier::ONE,
        server_tool_usage_classes: Vec::new(),
        provider_transforms: Vec::new(),
        passive_failure_count_threshold: 3,
        passive_cooldown_seconds: 60,
        passive_window_seconds: 30,
        passive_rate_limit_cooldown_seconds: 15,
        channel_max_retries: 0,
        channel_retry_interval_ms: 0,
        circuit_breaker_enabled: true,
        per_model_circuit_break: false,
        request_timeout_ms: 30_000,
        extra_fields_whitelist: None,
        strip_cross_protocol_nested_extra: true,
        billable_pricing_available: true,
        billing_rate_resolution: None,
        affinity_key: None,
        affinity_key_hash: None,
        affinity_hit: None,
        affinity_target: None,
        affinity_enabled: true,
        affinity_idle_ttl_seconds: 1800,
        affinity_failback_mode: failback_mode,
        affinity_failback_delay_seconds: failback_delay_seconds,
        routing_config_revision: 0,
        proxy_url: None,
        extra_headers: None,
        session_affinity_auto: false,
        allow_missing_usage: false,
        client_session_id: None,
        derived_session_affinity: None,
        session_affinity_value: None,
        origin_key: None,
        origin_peer_channel_ids: Vec::new(),
    }
}

#[test]
fn empty_cross_family_encoding_preserves_native_responses_continuation() {
    let native = affinity_test_attempt(
        "native-provider",
        "native-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        0,
    );
    let mut cross = native.clone();
    cross.provider_type = ProviderType::ChatCompletion;

    for reference in [
        json!({"previous_response_id": "resp_prior"}),
        json!({"conversation": "conv_prior"}),
    ] {
        let mut body = reference.clone();
        body["model"] = json!("gpt-affinity");
        body["input"] = json!([{
            "type": "function_call_output",
            "call_id": "call_prior",
            "output": "result"
        }]);
        let original = urp::decode::openai_responses::decode_request(&body).expect("decode");
        let error = nonstream::encode_request_for_provider(
            &mut original.clone(),
            &cross,
            DownstreamProtocol::Responses,
        )
        .expect_err("a cross-family result without its call must become empty");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "empty_input_after_conversion");
        assert!(!error.routing_exhausted);

        let encoded = nonstream::encode_request_for_provider(
            &mut original.clone(),
            &native,
            DownstreamProtocol::Responses,
        )
        .expect("the original stateful request remains encodable for Responses");
        assert_eq!(encoded["input"], body["input"]);
        for (key, value) in reference.as_object().unwrap() {
            assert_eq!(&encoded[key], value);
        }
    }
}

#[test]
fn session_affinity_resolution_priority_matches_spec() {
    let body = serde_json::json!({
        "prompt_cache_key": "from-cache-key",
        "messages": [{"role": "system", "content": "s"}, {"role": "user", "content": "u"}]
    });
    let base = affinity_test_attempt(
        "p",
        "c",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        0,
    );

    // Disabled channels produce no value (CM-AFF-4 null).
    assert_eq!(resolve_session_affinity_value(&base, &body), None);

    // Auto-enabled without client header derives from the body.
    let mut auto = base.clone();
    auto.session_affinity_auto = true;
    assert_eq!(
        resolve_session_affinity_value(&auto, &body).as_deref(),
        Some("from-cache-key")
    );

    // CM-AFF-1a: client session id wins over derivation.
    let mut client = auto.clone();
    client.client_session_id = Some("client-ses-1".to_string());
    assert_eq!(
        resolve_session_affinity_value(&client, &body).as_deref(),
        Some("client-ses-1")
    );

    // CM-AFF-1: explicit static header wins over client and derivation.
    let mut explicit = client.clone();
    explicit.extra_headers = Some(std::collections::BTreeMap::from([(
        "x-session-affinity".to_string(),
        "static-1".to_string(),
    )]));
    assert_eq!(
        resolve_session_affinity_value(&explicit, &body).as_deref(),
        Some("static-1")
    );

    // The header set applies the resolved value exactly once.
    let headers = attempt_extra_headers(&explicit, &body);
    assert_eq!(
        headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-session-affinity"))
            .count(),
        1
    );
    assert!(headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("x-session-affinity") && value == "static-1"
    }));
}

#[test]
fn session_affinity_null_setting_auto_detects_direct_workers_ai_urls() {
    for base_url in [
        "https://api.cloudflare.com/client/v4/accounts/account-id/ai",
        "https://api.cloudflare.com/client/v4/accounts/account-id/ai/",
        "https://api.cloudflare.com/client/v4/accounts/account-id/ai/v1",
    ] {
        assert!(
            routing::effective_session_affinity_auto(base_url, None),
            "{base_url}"
        );
    }

    for base_url in [
        "http://api.cloudflare.com/client/v4/accounts/account-id/ai",
        "https://api.cloudflare.com.example/client/v4/accounts/account-id/ai",
        "https://api.cloudflare.com/client//v4/accounts/account-id/ai",
        "https://api.cloudflare.com/client/v4/accounts/account-id/ai/run/model",
        "https://api.cloudflare.com/client/v4/accounts/account-id/ai//",
        "https://api.cloudflare.com/client/v4/accounts/account-id/ai?debug=1",
        "https://example.com/client/v4/accounts/account-id/ai",
    ] {
        assert!(
            !routing::effective_session_affinity_auto(base_url, None),
            "{base_url}"
        );
    }
}

#[test]
fn session_affinity_explicit_setting_overrides_url_detection() {
    let workers_ai = "https://api.cloudflare.com/client/v4/accounts/account-id/ai";
    assert!(!routing::effective_session_affinity_auto(
        workers_ai,
        Some(false)
    ));
    assert!(routing::effective_session_affinity_auto(
        "https://example.com/v1",
        Some(true)
    ));
}

#[test]
fn client_session_id_header_extraction_prefers_codex_session_id() {
    let mut headers = HeaderMap::new();
    headers.insert("x-session-affinity", "ses-a".parse().unwrap());
    headers.insert("session_id", "codex-b".parse().unwrap());
    assert_eq!(
        extract_client_session_id(&headers).as_deref(),
        Some("codex-b")
    );

    let mut hyphenated = HeaderMap::new();
    hyphenated.insert("session-id", "codex-hyphen".parse().unwrap());
    hyphenated.insert("x-session-affinity", "ses-a".parse().unwrap());
    assert_eq!(
        extract_client_session_id(&hyphenated).as_deref(),
        Some("codex-hyphen")
    );

    let mut x_session_id = HeaderMap::new();
    x_session_id.insert("x-session-id", "sid-1".parse().unwrap());
    assert_eq!(
        extract_client_session_id(&x_session_id).as_deref(),
        Some("sid-1")
    );

    let mut only_affinity = HeaderMap::new();
    only_affinity.insert("x-session-affinity", "ses-a".parse().unwrap());
    assert_eq!(
        extract_client_session_id(&only_affinity).as_deref(),
        Some("ses-a")
    );

    let empty = HeaderMap::new();
    assert_eq!(extract_client_session_id(&empty), None);
}

#[test]
fn session_affinity_sanitizer_strips_controls_and_truncates() {
    let dirty = format!("  ok{}junk", "x".repeat(200));
    let sanitized = sanitize_session_affinity(&dirty);
    assert!(sanitized.starts_with("ok"));
    assert_eq!(sanitized.len(), 128);
    assert!(
        sanitized
            .chars()
            .all(|c| ('\u{20}'..='\u{7e}').contains(&c))
    );

    assert_eq!(sanitize_session_affinity("\u{7}\u{7}"), "");
}

#[test]
fn passive_failure_window_prunes_only_timestamps_before_cutoff() {
    let mut timestamps = std::collections::VecDeque::from([69, 70, 100]);
    routing::prune_passive_failure_timestamps(&mut timestamps, 100, 30);
    assert_eq!(timestamps.into_iter().collect::<Vec<_>>(), vec![70, 100]);
}

#[test]
fn same_channel_retry_classification_covers_all_5xx_and_excludes_client_errors() {
    for status in [
        StatusCode::REQUEST_TIMEOUT,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::INSUFFICIENT_STORAGE,
    ] {
        let err = UpstreamCallError::new(
            UpstreamErrorKind::Http,
            Some(status),
            "upstream failure".to_string(),
        );
        assert!(routing::is_same_channel_retryable_error(&err), "{status}");
    }

    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::UNPROCESSABLE_ENTITY,
    ] {
        let err = UpstreamCallError::new(
            UpstreamErrorKind::Http,
            Some(status),
            "upstream rejection".to_string(),
        );
        assert!(!routing::is_same_channel_retryable_error(&err), "{status}");
    }

    let network = UpstreamCallError::new(
        UpstreamErrorKind::Network,
        None,
        "connection reset".to_string(),
    );
    assert!(routing::is_same_channel_retryable_error(&network));

    let decode_error = AppError::new(
        StatusCode::BAD_GATEWAY,
        "invalid_upstream_response",
        "invalid response",
    );
    assert!(!routing::is_same_channel_retryable_app_error(&decode_error));

    let embedded_server_error = AppError::new(
        StatusCode::BAD_GATEWAY,
        "upstream_chat_error",
        "embedded server error",
    )
    .with_upstream_error(
        Some(StatusCode::SERVICE_UNAVAILABLE),
        Some("503".to_string()),
        None,
        None,
    );
    assert!(routing::is_same_channel_retryable_app_error(
        &embedded_server_error
    ));
}

#[tokio::test]
async fn disabled_breaker_success_and_failure_do_not_insert_health_state() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "disabled-breaker-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.circuit_breaker_enabled = false;
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);

    routing::mark_channel_success(&state, &attempt).await;
    routing::mark_channel_retryable_failure(
        &state,
        &attempt,
        routing::RetryableFailureClass::Transient,
    )
    .await;

    assert!(state.channel_health.lock().await.is_empty());
}

#[tokio::test]
async fn passive_failure_queue_stops_at_threshold_and_success_adds_no_sample() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let mut attempt = affinity_test_attempt(
        "provider",
        "bounded-passive-channel",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        30,
    );
    attempt.passive_failure_count_threshold = 2;
    attempt.routing_config_revision = state
        .routing_config_revision
        .load(std::sync::atomic::Ordering::Acquire);

    for _ in 0..3 {
        routing::mark_channel_retryable_failure(
            &state,
            &attempt,
            routing::RetryableFailureClass::Transient,
        )
        .await;
    }
    {
        let health = state.channel_health.lock().await;
        let entry = health
            .get("bounded-passive-channel")
            .expect("health entry exists");
        assert!(!entry.healthy);
        assert_eq!(entry.passive_failure_timestamps.len(), 2);
    }

    routing::mark_channel_success(&state, &attempt).await;
    let health = state.channel_health.lock().await;
    let entry = health
        .get("bounded-passive-channel")
        .expect("health entry exists");
    assert!(entry.healthy);
    assert_eq!(entry.passive_failure_timestamps.len(), 2);
}

fn affinity_test_auth() -> AuthResult {
    let mut auth = build_test_auth(None);
    auth.user_id = Some("affinity-user".to_string());
    auth
}

#[test]
fn affinity_capacity_refreshes_existing_key_and_rejects_new_key() {
    let binding =
        |channel_id: &str, last_used_at: i64| crate::monoize_routing::ChannelAffinityBinding {
            provider_id: "provider".to_string(),
            channel_id: channel_id.to_string(),
            bound_at: 1,
            last_used_at,
            expires_at: last_used_at + 100,
        };
    let mut cache = HashMap::from([("existing".to_string(), binding("old", 1))]);
    routing::insert_channel_affinity_with_limit(
        &mut cache,
        "new".to_string(),
        binding("new", 2),
        1,
    );
    assert_eq!(cache.len(), 1);
    assert!(!cache.contains_key("new"));

    routing::insert_channel_affinity_with_limit(
        &mut cache,
        "existing".to_string(),
        binding("refreshed", 3),
        1,
    );
    assert_eq!(cache["existing"].channel_id, "refreshed");
    assert_eq!(cache["existing"].last_used_at, 3);

    assert_eq!(
        crate::monoize_routing::cleanup_channel_affinity(&mut cache, 103),
        1
    );
    routing::insert_channel_affinity_with_limit(
        &mut cache,
        "new".to_string(),
        binding("new", 104),
        1,
    );
    assert!(cache.contains_key("new"));
}

#[tokio::test]
async fn affinity_lookup_removes_only_the_requested_expired_binding() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let request = build_test_routing_request("gpt-affinity");
    let auth = affinity_test_auth();
    let (key, _) = affinity_key_for_request(&request, &auth).expect("affinity key");
    let now = now_ts();
    let live_key = "unrelated-live".to_string();
    let binding =
        |channel_id: &str, expires_at: i64| crate::monoize_routing::ChannelAffinityBinding {
            provider_id: "provider-b".to_string(),
            channel_id: channel_id.to_string(),
            bound_at: now - 60,
            last_used_at: now - 30,
            expires_at,
        };
    state.channel_affinity.lock().await.extend([
        (key.clone(), binding("channel-b1", now)),
        (live_key.clone(), binding("channel-live", now + 60)),
    ]);

    let attempts = apply_channel_affinity(
        &state,
        &request,
        &auth,
        vec![affinity_test_attempt(
            "provider-b",
            "channel-b1",
            crate::monoize_routing::AffinityFailbackMode::Sticky,
            30,
        )],
    )
    .await
    .expect("affinity applies");

    assert_eq!(attempts[0].affinity_hit, Some(false));
    let cache = state.channel_affinity.lock().await;
    assert!(!cache.contains_key(&key));
    assert!(cache.contains_key(&live_key));
}

#[tokio::test]
async fn affinity_failback_mode_controls_recovered_provider_order() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let request = build_test_routing_request("gpt-affinity");
    let auth = affinity_test_auth();
    let (key, _) = affinity_key_for_request(&request, &auth).expect("affinity key");
    let now = now_ts();
    state.channel_affinity.lock().await.insert(
        key.clone(),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: "provider-b".to_string(),
            channel_id: "channel-b1".to_string(),
            bound_at: now - 60,
            last_used_at: now,
            expires_at: now + 1800,
        },
    );

    let sticky = apply_channel_affinity(
        &state,
        &request,
        &auth,
        vec![
            affinity_test_attempt(
                "provider-a",
                "channel-a1",
                crate::monoize_routing::AffinityFailbackMode::Sticky,
                30,
            ),
            affinity_test_attempt(
                "provider-b",
                "channel-b1",
                crate::monoize_routing::AffinityFailbackMode::Sticky,
                30,
            ),
        ],
    )
    .await
    .expect("sticky affinity applies");
    assert_eq!(sticky[0].channel_id, "channel-b1");
    assert_eq!(sticky[0].affinity_hit, Some(true));

    let failback = apply_channel_affinity(
        &state,
        &request,
        &auth,
        vec![
            affinity_test_attempt(
                "provider-a",
                "channel-a1",
                crate::monoize_routing::AffinityFailbackMode::Sticky,
                30,
            ),
            affinity_test_attempt(
                "provider-b",
                "channel-b1",
                crate::monoize_routing::AffinityFailbackMode::PreferHigherPriority,
                30,
            ),
        ],
    )
    .await
    .expect("failback affinity applies");
    assert_eq!(failback[0].channel_id, "channel-a1");
    assert_eq!(failback[0].affinity_hit, Some(false));
    assert_eq!(
        failback[0].affinity_target.as_deref(),
        Some("provider-b/channel-b1")
    );
}

#[tokio::test]
async fn affinity_disabled_override_removes_existing_binding() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let request = build_test_routing_request("gpt-affinity");
    let auth = affinity_test_auth();
    let (key, _) = affinity_key_for_request(&request, &auth).expect("affinity key");
    let now = now_ts();
    state.channel_affinity.lock().await.insert(
        key.clone(),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: "provider-b".to_string(),
            channel_id: "channel-b1".to_string(),
            bound_at: now,
            last_used_at: now,
            expires_at: now + 1800,
        },
    );
    let mut disabled = affinity_test_attempt(
        "provider-b",
        "channel-b1",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        300,
    );
    disabled.affinity_enabled = false;

    let attempts = apply_channel_affinity(&state, &request, &auth, vec![disabled])
        .await
        .expect("disabled affinity applies");

    assert_eq!(attempts[0].affinity_hit, Some(false));
    assert!(!state.channel_affinity.lock().await.contains_key(&key));
}

#[tokio::test]
async fn response_id_affinity_inherits_source_binding_age_on_hit() {
    let runtime = RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,

        node: crate::node_config::NodeSettings::primary_default(),
    };
    let state = load_state_with_runtime(runtime).await.expect("state loads");
    let request = build_test_routing_request("gpt-affinity");
    let auth = affinity_test_auth();
    let (key, _) = affinity_key_for_request(&request, &auth).expect("affinity key");
    let now = now_ts();
    let original_bound_at = now - 120;
    state.channel_affinity.lock().await.insert(
        key.clone(),
        crate::monoize_routing::ChannelAffinityBinding {
            provider_id: "provider-b".to_string(),
            channel_id: "channel-b1".to_string(),
            bound_at: original_bound_at,
            last_used_at: now,
            expires_at: now + 1800,
        },
    );
    let mut attempt = affinity_test_attempt(
        "provider-b",
        "channel-b1",
        crate::monoize_routing::AffinityFailbackMode::PreferHigherPriority,
        300,
    );
    attempt.affinity_key = Some(key);
    attempt.affinity_hit = Some(true);

    refresh_channel_affinity(&state, &attempt).await;
    refresh_response_id_affinity(&state, &auth, "gpt-affinity", "resp-next", &attempt).await;

    let response_key = response_id_affinity_key("gpt-affinity", "resp-next", &auth)
        .expect("response affinity key");
    let response_binding = state
        .channel_affinity
        .lock()
        .await
        .get(&response_key)
        .cloned()
        .expect("response binding");
    assert_eq!(response_binding.bound_at, original_bound_at);
    assert_eq!(
        response_binding.expires_at - response_binding.last_used_at,
        attempt.affinity_idle_ttl_seconds as i64
    );
}

#[test]
fn session_affinity_is_stable_within_conversation_and_differs_across_sessions() {
    let base = serde_json::json!({
        "model": "cf-model",
        "messages": [
            { "role": "system", "content": "You are a helpful assistant." },
            { "role": "user", "content": "solve task A" }
        ],
        "tools": [{ "type": "function", "function": { "name": "calc" } }]
    });
    let grown = {
        let mut body = base.clone();
        body["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "role": "assistant", "content": "working" }));
        body
    };
    let first = routing::derive_session_affinity(&base).unwrap();
    let second = routing::derive_session_affinity(&grown).unwrap();
    assert_eq!(
        first, second,
        "appending messages must keep affinity stable"
    );

    let mut tools_changed = base.clone();
    tools_changed["tools"] = serde_json::json!([
        { "type": "function", "function": { "name": "calc" } },
        { "type": "function", "function": { "name": "search" } }
    ]);
    assert_eq!(
        routing::derive_session_affinity(&tools_changed).unwrap(),
        first,
        "tool-definition changes must not split session affinity"
    );

    let other_session = serde_json::json!({
        "model": "cf-model",
        "messages": [
            { "role": "system", "content": "You are a helpful assistant." },
            { "role": "user", "content": "solve task B" }
        ]
    });
    let other = routing::derive_session_affinity(&other_session).unwrap();
    assert_ne!(
        first, other,
        "distinct sessions must derive distinct affinities"
    );
}

#[test]
fn session_affinity_body_session_id_wins_over_derived_digest() {
    let req = urp::decode::openai_chat::decode_request(&serde_json::json!({
        "model": "cf-model",
        "messages": [
            { "role": "system", "content": "shared system prompt" },
            { "role": "user", "content": "turn one" }
        ],
        "tools": [{ "type": "function", "function": { "name": "calc" } }],
        "session_id": "019ffeb5-e6ed-7180-89b6-df6e938625a6"
    }))
    .expect("decode request");
    let mut attempt = affinity_test_attempt(
        "p",
        "c",
        crate::monoize_routing::AffinityFailbackMode::Sticky,
        0,
    );
    attempt.session_affinity_auto = true;
    attach_client_session_id(std::slice::from_mut(&mut attempt), None, Some(&req));
    assert_eq!(
        attempt.client_session_id.as_deref(),
        Some("019ffeb5-e6ed-7180-89b6-df6e938625a6")
    );

    let mut later = req.clone();
    later.input.push(urp::Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
        id: None,
        role: urp::OrdinaryRole::User,
        content: "turn two".to_string(),
        phase: None,
        extra_body: HashMap::new(),
    });
    later.tools = Some(Vec::new());
    let mut later_attempt = attempt.clone();
    attach_client_session_id(std::slice::from_mut(&mut later_attempt), None, Some(&later));
    assert_eq!(later_attempt.client_session_id, attempt.client_session_id);
    assert_eq!(
        resolve_session_affinity_value(&attempt, &serde_json::json!({})).as_deref(),
        Some("019ffeb5-e6ed-7180-89b6-df6e938625a6")
    );
}

#[test]
fn session_affinity_urp_digest_ignores_tools_and_appended_nodes() {
    let first = urp::decode::openai_chat::decode_request(&serde_json::json!({
        "model": "cf-model",
        "messages": [
            { "role": "system", "content": "shared system prompt" },
            { "role": "user", "content": "session A question" }
        ],
        "tools": [{ "type": "function", "function": { "name": "calc" } }]
    }))
    .expect("decode first");
    let second = urp::decode::openai_chat::decode_request(&serde_json::json!({
        "model": "cf-model",
        "messages": [
            { "role": "system", "content": "shared system prompt" },
            { "role": "user", "content": "session A question" },
            { "role": "assistant", "content": "working" },
            { "role": "user", "content": "continue" }
        ],
        "tools": [
            { "type": "function", "function": { "name": "calc" } },
            { "type": "function", "function": { "name": "search" } }
        ]
    }))
    .expect("decode second");
    assert_eq!(
        routing::derive_session_affinity_from_urp(&first),
        routing::derive_session_affinity_from_urp(&second)
    );

    let other = urp::decode::openai_chat::decode_request(&serde_json::json!({
        "model": "cf-model",
        "messages": [
            { "role": "system", "content": "shared system prompt" },
            { "role": "user", "content": "session B question" }
        ]
    }))
    .expect("decode other");
    assert_ne!(
        routing::derive_session_affinity_from_urp(&first),
        routing::derive_session_affinity_from_urp(&other)
    );
}

#[test]
fn session_affinity_prefers_prompt_cache_key_and_sanitizes() {
    let keyed = serde_json::json!({ "prompt_cache_key": "  sess-42  " });
    assert_eq!(routing::derive_session_affinity(&keyed).unwrap(), "sess-42");
}

// AFF-5a: nodes decoded from the same wire bytes carry multi-entry
// `extra_body` HashMaps whose iteration order differs per instance, so a
// naive serde serialization would hash the same request to different keys.
#[test]
fn affinity_prefix_hash_is_deterministic_across_decodes() {
    let body = serde_json::json!({
        "model": "gpt-5.6-terra",
        "stream": true,
        "store": false,
        "instructions": "You are Codex, a coding agent.",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "<environment_context>cwd=/repo</environment_context>" }]
            },
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "fix the failing test" }]
            },
            {
                "type": "reasoning",
                "id": "rs_0001",
                "summary": [{ "type": "summary_text", "text": "plan the fix" }],
                "encrypted_content": "gAAAAABencrypted0001",
                "status": "completed"
            },
            {
                "type": "message",
                "role": "assistant",
                "id": "msg_0001",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "done", "annotations": [], "logprobs": [] }]
            },
            {
                "type": "function_call",
                "id": "fc_0001",
                "call_id": "call_0001",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}",
                "status": "completed"
            },
            {
                "type": "function_call_output",
                "id": "fco_0001",
                "call_id": "call_0001",
                "output": "src tests",
                "status": "completed"
            }
        ]
    });
    let reference = build_routing_stub(
        &urp::decode::openai_responses::decode_request(&body).expect("decode"),
        None,
    );
    // A single re-decode can coincidentally produce the same HashMap
    // iteration order; 32 independent decodes make a false pass negligible.
    for _ in 0..32 {
        let redecoded = build_routing_stub(
            &urp::decode::openai_responses::decode_request(&body).expect("decode"),
            None,
        );
        assert_eq!(
            reference.affinity_prefix_hash, redecoded.affinity_prefix_hash,
            "affinity prefix hash must be identical for identical wire requests"
        );
    }
}

#[test]
fn usage_breakdown_persists_only_normalized_members() {
    use crate::handlers::billing::build_usage_breakdown;
    use std::collections::HashMap;

    // RL15c: the upstream usage echo must never reach the persisted snapshot, no matter
    // how large the attribution map inside extra_body is.
    let mut extra = HashMap::new();
    extra.insert(
        "attribution".to_string(),
        serde_json::json!({ "items": { "at_1": { "input_tokens": 5 }, "at_2": { "input_tokens": 7 } } }),
    );
    extra.insert("cache_read_input_tokens".to_string(), serde_json::json!(11));
    let usage = urp::Usage {
            iterations: Default::default(),
        input_tokens: 100,
        output_tokens: 4,
        input_details: None,
        output_details: None,
        extra_body: extra,
    };

    let snapshot = build_usage_breakdown(&usage);
    assert!(snapshot.get("raw_usage_extra").is_none());
    assert!(snapshot.get("attribution").is_none());
    assert_eq!(snapshot["input"]["total_tokens"], serde_json::json!(100));
    assert_eq!(snapshot["output"]["total_tokens"], serde_json::json!(4));
    // Normalized fallbacks that read extra_body still land in the normalized object.
    assert_eq!(snapshot["input"]["cached_tokens"], serde_json::json!(11));
}

async fn spawn_porn_judge_server() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use axum::extract::State;
    use axum::routing::post;
    use std::sync::atomic::AtomicUsize;

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);

    async fn judge(
        State(counter): State<Arc<AtomicUsize>>,
        Json(_body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Json(serde_json::json!({
            "choices": [{
                "message": {
                    "content": "该请求要求生成色情内容。\n{\"category\":\"porn\",\"reason\":\"请求明确要求撰写成人小说内容。\"}"
                }
            }]
        }))
    }

    let router = axum::Router::new()
        .route("/v1/chat/completions", post(judge))
        .with_state(counter);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind judge server");
    let addr = listener.local_addr().expect("judge server addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("judge server");
    });
    (format!("http://{addr}"), calls)
}

async fn firewall_test_state(judge_base_url: String) -> AppState {
    let state = load_state_with_runtime(RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn: "sqlite::memory:".to_string(),
        request_log_spool_dir: None,
        node: crate::node_config::NodeSettings::primary_default(),
    })
    .await
    .expect("state loads");
    let mut runtime = crate::monoize_routing::MonoizeRuntimeConfig::default();
    runtime.moderation_enabled = true;
    runtime.content_firewall = crate::content_firewall::ContentFirewall::compile(
        crate::content_firewall::DEFAULT_BLOCKED_WORDS,
    );
    runtime.moderation_judge = crate::moderation_judge::JudgeConfig {
        enabled: true,
        base_url: judge_base_url,
        api_key: "judge-key".to_string(),
        model: "judge-model".to_string(),
        timeout_ms: 5000,
    };
    *state.monoize_runtime.write().await = runtime;
    state
}

#[tokio::test]
async fn content_firewall_blocks_porn_verdict_for_user_role() {
    use std::sync::atomic::Ordering;

    let (judge_url, judge_calls) = spawn_porn_judge_server().await;
    let state = firewall_test_state(judge_url).await;
    let auth = build_test_auth_with_role(None, UserRole::User);
    let headers = HeaderMap::new();

    let error = ensure_content_allowed(
        &state,
        &headers,
        &auth,
        "chat_completions",
        "test-model",
        &["请写一篇成人小说"],
    )
    .await
    .expect_err("user role must be blocked by the porn verdict");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.code, "content_blocked");
    assert_eq!(judge_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn content_firewall_exempts_admin_and_super_admin_roles() {
    use std::sync::atomic::Ordering;

    let (judge_url, judge_calls) = spawn_porn_judge_server().await;
    let state = firewall_test_state(judge_url).await;
    let headers = HeaderMap::new();

    for role in [UserRole::Admin, UserRole::SuperAdmin] {
        let auth = build_test_auth_with_role(None, role);
        ensure_content_allowed(
            &state,
            &headers,
            &auth,
            "chat_completions",
            "test-model",
            &["请写一篇成人小说"],
        )
        .await
        .expect("admin roles are exempt from the content firewall");
    }
    assert_eq!(
        judge_calls.load(Ordering::SeqCst),
        0,
        "exempt roles must not trigger any judge call"
    );
}

#[test]
fn usage_iterations_are_the_complete_accounting_total() {
    let usage = urp::Usage {
        input_tokens: 10,
        output_tokens: 2,
        iterations: Some(vec![
            urp::UsageIteration { kind: "message".into(), usage: urp::Usage { input_tokens: 10, output_tokens: 2, ..Default::default() } },
            urp::UsageIteration { kind: "compaction".into(), usage: urp::Usage { input_tokens: 5, output_tokens: 3, ..Default::default() } },
        ]),
        ..Default::default()
    };
    let pricing = ModelPricing {
        input_cost_per_token_nano: 1,
        output_cost_per_token_nano: 1,
        cache_read_input_cost_per_token_nano: None,
        cache_creation_input_cost_per_token_nano: None,
        output_cost_per_reasoning_token_nano: None,
    };
    assert_eq!(calculate_charge_nano(&usage, &pricing, Multiplier::ONE), Some(20));
    let breakdown = build_usage_breakdown(&usage);
    assert_eq!(breakdown["input"]["total_tokens"], 15);
    assert_eq!(breakdown["output"]["total_tokens"], 5);
}

use crate::db::DbPool;
use crate::exact_decimal::Multiplier;
use crate::settings::{
    PricingProfilePattern, default_pricing_profile_model_patterns, default_reasoning_suffix_map,
};
use crate::transforms::{TransformRuleConfig, canonicalize_transform_rules};
use chrono::{DateTime, Utc};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use sea_orm::{ConnectionTrait, QueryResult, Value as SeaValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Process-wide override for `MONOIZE_ALLOW_PRIVATE_UPSTREAM`, exposed so integration
/// tests that bind a mock upstream to loopback can create Channels. The override only
/// relaxes the private/loopback address classification (CP-INV-16); scheme and host
/// requirements are still enforced.
/// True when the operator has explicitly permitted private upstream addresses.
/// Both the create-time Channel check and the per-dispatch address guard read
/// this single decision so the two cannot drift apart.
pub fn private_upstream_addresses_allowed() -> bool {
    let allowed = std::env::var("MONOIZE_ALLOW_PRIVATE_UPSTREAM")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || ALLOW_PRIVATE_UPSTREAM_OVERRIDE.load(Ordering::Relaxed);
    #[cfg(test)]
    let allowed = allowed || TEST_ALLOW_PRIVATE_UPSTREAM.with(|cell| cell.get());
    allowed
}

pub static ALLOW_PRIVATE_UPSTREAM_OVERRIDE: AtomicBool = AtomicBool::new(false);

/// Disable CP-INV-16 private/loopback address rejection for the lifetime of the
/// process. Intended for tests whose mock upstream listens on a loopback address.
pub fn set_allow_private_upstream_override(allow: bool) {
    ALLOW_PRIVATE_UPSTREAM_OVERRIDE.store(allow, Ordering::Relaxed);
}

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    /// Test-only override for `MONOIZE_ALLOW_PRIVATE_UPSTREAM`, kept thread-local so
    /// tests that bind a mock upstream to loopback can create Channels without racing
    /// the process-global environment. Production code never touches this.
    static TEST_ALLOW_PRIVATE_UPSTREAM: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn test_set_allow_private_upstream(allow: bool) {
    TEST_ALLOW_PRIVATE_UPSTREAM.with(|cell| cell.set(allow));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonoizeProviderType {
    Responses,
    ChatCompletion,
    Messages,
    Gemini,
    OpenaiImage,
    Replicate,
    OpenaiVideo,
    FalVideo,
}

impl MonoizeProviderType {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "responses" => Some(Self::Responses),
            "chat_completion" => Some(Self::ChatCompletion),
            "messages" => Some(Self::Messages),
            "gemini" => Some(Self::Gemini),
            "openai_image" => Some(Self::OpenaiImage),
            "replicate" => Some(Self::Replicate),
            "openai_video" => Some(Self::OpenaiVideo),
            "fal_video" => Some(Self::FalVideo),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletion => "chat_completion",
            Self::Messages => "messages",
            Self::Gemini => "gemini",
            Self::OpenaiImage => "openai_image",
            Self::Replicate => "replicate",
            Self::OpenaiVideo => "openai_video",
            Self::FalVideo => "fal_video",
        }
    }

    pub fn to_config_type(&self) -> crate::config::ProviderType {
        match self {
            Self::Responses => crate::config::ProviderType::Responses,
            Self::ChatCompletion => crate::config::ProviderType::ChatCompletion,
            Self::Messages => crate::config::ProviderType::Messages,
            Self::Gemini => crate::config::ProviderType::Gemini,
            Self::OpenaiImage => crate::config::ProviderType::OpenaiImage,
            Self::Replicate => crate::config::ProviderType::Replicate,
            Self::OpenaiVideo => crate::config::ProviderType::OpenaiVideo,
            Self::FalVideo => crate::config::ProviderType::FalVideo,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffinityFailbackMode {
    #[default]
    Sticky,
    PreferHigherPriority,
}

impl AffinityFailbackMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sticky => "sticky",
            Self::PreferHigherPriority => "prefer_higher_priority",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "sticky" => Some(Self::Sticky),
            "prefer_higher_priority" => Some(Self::PreferHigherPriority),
            _ => None,
        }
    }
}

const OPENCODE_PROBE_SESSION_VALUE: &str = "mono-probe";
const SESSION_CACHE_HEADER_NAMES: &[&str] = &["x-session-affinity", "x-opencode-session"];

/// CM-AFF-0: null `session_affinity_auto` enables affinity for these URLs.
pub(crate) fn default_session_affinity_auto(base_url: &str) -> bool {
    is_direct_cloudflare_workers_ai_url(base_url) || is_direct_opencode_zen_url(base_url)
}

/// CM-AFF-0: `https://api.cloudflare.com/client/v4/accounts/{id}/ai` or `.../ai/v1`.
fn is_direct_cloudflare_workers_ai_url(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("api.cloudflare.com")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }

    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let Some(account_and_suffix) = path.strip_prefix("/client/v4/accounts/") else {
        return false;
    };
    let account_id = account_and_suffix
        .strip_suffix("/ai/v1")
        .or_else(|| account_and_suffix.strip_suffix("/ai"));
    account_id.is_some_and(|account_id| !account_id.is_empty() && !account_id.contains('/'))
}

/// CM-AFF-0b: `https://opencode.ai/zen` and paths under `/zen/`.
fn is_direct_opencode_zen_url(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("opencode.ai")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    path == "/zen" || path.starts_with("/zen/")
}

fn extra_headers_contain(headers: Option<&BTreeMap<String, String>>, name: &str) -> bool {
    headers.is_some_and(|headers| headers.keys().any(|key| key.eq_ignore_ascii_case(name)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTypeOverride {
    pub pattern: String,
    pub api_type: MonoizeProviderType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonoizeModelEntry {
    pub redirect: Option<String>,
    #[serde(default)]
    pub pricing_profile_mode: PricingProfileMode,
    #[serde(default)]
    pub pricing_profile_override: Option<String>,
    #[serde(default)]
    pub multiplier_override: Option<Multiplier>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PricingProfileMode {
    #[default]
    Inherit,
    Override,
    Unpriced,
}

pub fn effective_pricing_profile<'a>(
    provider: &'a MonoizeProvider,
    entry: &'a MonoizeModelEntry,
) -> Option<&'a str> {
    match entry.pricing_profile_mode {
        PricingProfileMode::Inherit => provider.pricing_profile.as_deref(),
        PricingProfileMode::Override => entry.pricing_profile_override.as_deref(),
        PricingProfileMode::Unpriced => None,
    }
}

pub fn effective_model_multiplier(
    provider: &MonoizeProvider,
    entry: &MonoizeModelEntry,
) -> Multiplier {
    entry.multiplier_override.unwrap_or(provider.multiplier)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonoizeChannel {
    pub id: String,
    pub name: String,
    pub provider_type: MonoizeProviderType,
    pub base_url: String,
    #[serde(skip_serializing)]
    pub api_key: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_missing_usage: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_failure_count_threshold_override: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_cooldown_seconds_override: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_window_seconds_override: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passive_rate_limit_cooldown_seconds_override: Option<u64>,
    #[serde(default)]
    pub models: HashMap<String, MonoizeModelEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_probe_enabled_override: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_probe_interval_seconds_override: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_probe_success_threshold_override: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_probe_model_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affinity_enabled_override: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affinity_idle_ttl_seconds_override: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affinity_failback_mode_override: Option<AffinityFailbackMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affinity_failback_delay_seconds_override: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// CP-INV-15: static headers injected into every upstream request for this Channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<BTreeMap<String, String>>,
    /// CM-AFF-0: explicit override for URL-based automatic session affinity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _healthy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _last_success_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _health_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _unhealthy_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _probing_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _cooldown_until: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonoizeProvider {
    pub id: String,
    pub name: String,
    pub channel: MonoizeChannel,
    pub pricing_profile: Option<String>,
    pub multiplier: Multiplier,
    pub channel_max_retries: i32,
    pub channel_retry_interval_ms: i32,
    pub circuit_breaker_enabled: bool,
    pub per_model_circuit_break: bool,
    #[serde(default)]
    pub transforms: Vec<TransformRuleConfig>,
    #[serde(default)]
    pub api_type_overrides: Vec<ApiTypeOverride>,
    pub active_probe_enabled_override: Option<bool>,
    pub active_probe_interval_seconds_override: Option<u64>,
    pub active_probe_success_threshold_override: Option<u32>,
    pub active_probe_model_override: Option<String>,
    pub request_timeout_ms_override: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub prompt_cache_incompatible_with_tools: bool,
    #[serde(default)]
    pub extra_fields_whitelist: Option<Vec<String>>,
    #[serde(default)]
    pub strip_cross_protocol_nested_extra: Option<bool>,
    pub group_id: String,
    pub enabled: bool,
    pub priority: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMonoizeChannelInput {
    pub name: String,
    pub provider_type: MonoizeProviderType,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_missing_usage: bool,
    #[serde(default)]
    pub passive_failure_count_threshold_override: Option<u32>,
    #[serde(default)]
    pub passive_cooldown_seconds_override: Option<u64>,
    #[serde(default)]
    pub passive_window_seconds_override: Option<u64>,
    #[serde(default)]
    pub passive_rate_limit_cooldown_seconds_override: Option<u64>,
    #[serde(default)]
    pub models: HashMap<String, MonoizeModelEntry>,
    pub active_probe_enabled_override: Option<bool>,
    pub active_probe_interval_seconds_override: Option<u64>,
    pub active_probe_success_threshold_override: Option<u32>,
    pub active_probe_model_override: Option<String>,
    #[serde(default)]
    pub affinity_enabled_override: Option<bool>,
    #[serde(default)]
    pub affinity_idle_ttl_seconds_override: Option<u64>,
    #[serde(default)]
    pub affinity_failback_mode_override: Option<AffinityFailbackMode>,
    #[serde(default)]
    pub affinity_failback_delay_seconds_override: Option<u64>,
    /// CP-INV-14: None/empty = follow-global; Some(url) = custom http(s) egress proxy.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// CP-INV-15: static upstream headers; None/empty map = none.
    #[serde(default)]
    pub extra_headers: Option<BTreeMap<String, String>>,
    /// CM-AFF-2: enable derived per-request session affinity.
    #[serde(default)]
    pub session_affinity_auto: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMonoizeProviderInput {
    pub name: String,
    pub channel: CreateMonoizeChannelInput,
    #[serde(default)]
    pub confirm_public_exposure: bool,
    #[serde(default)]
    pub pricing_profile: Option<String>,
    #[serde(default)]
    pub multiplier: Multiplier,
    #[serde(default)]
    pub channel_max_retries: i32,
    #[serde(default)]
    pub channel_retry_interval_ms: i32,
    #[serde(default = "default_enabled")]
    pub circuit_breaker_enabled: bool,
    #[serde(default)]
    pub per_model_circuit_break: bool,
    #[serde(default)]
    pub transforms: Vec<TransformRuleConfig>,
    pub active_probe_enabled_override: Option<bool>,
    #[serde(default)]
    pub api_type_overrides: Vec<ApiTypeOverride>,
    pub active_probe_interval_seconds_override: Option<u64>,
    pub active_probe_success_threshold_override: Option<u32>,
    pub active_probe_model_override: Option<String>,
    pub request_timeout_ms_override: Option<u64>,
    #[serde(default)]
    pub max_input_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_cache_incompatible_with_tools: Option<bool>,
    #[serde(default)]
    pub extra_fields_whitelist: Option<Vec<String>>,
    #[serde(default)]
    pub strip_cross_protocol_nested_extra: Option<bool>,
    #[serde(default)]
    pub group_id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub priority: Option<i32>,
}

/// PP-W2: wholesale create request. Copies a source Provider of a non-agent class into an
/// agent-class Group with wholesale multipliers.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWholesaleProviderInput {
    pub group_id: String,
    pub source_provider_id: String,
    #[serde(default)]
    pub multiplier: Option<Multiplier>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub channel_name: Option<String>,
    #[serde(default)]
    pub model_multipliers: Option<HashMap<String, Multiplier>>,
    #[serde(default)]
    pub confirm_public_exposure: bool,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMonoizeProviderInput {
    pub name: Option<String>,
    pub channel: Option<CreateMonoizeChannelInput>,
    #[serde(default)]
    pub confirm_public_exposure: bool,
    pub pricing_profile: Option<Option<String>>,
    pub multiplier: Option<Multiplier>,
    pub channel_max_retries: Option<i32>,
    pub channel_retry_interval_ms: Option<i32>,
    pub circuit_breaker_enabled: Option<bool>,
    pub per_model_circuit_break: Option<bool>,
    pub transforms: Option<Vec<TransformRuleConfig>>,
    pub active_probe_enabled_override: Option<Option<bool>>,
    pub api_type_overrides: Option<Vec<ApiTypeOverride>>,
    pub active_probe_interval_seconds_override: Option<Option<u64>>,
    pub active_probe_success_threshold_override: Option<Option<u32>>,
    pub active_probe_model_override: Option<Option<String>>,
    pub request_timeout_ms_override: Option<Option<u64>>,
    pub max_input_tokens: Option<Option<u64>>,
    pub prompt_cache_incompatible_with_tools: Option<bool>,
    pub extra_fields_whitelist: Option<Option<Vec<String>>>,
    pub strip_cross_protocol_nested_extra: Option<Option<bool>>,
    pub group_id: Option<String>,
    pub enabled: Option<bool>,
    pub priority: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReorderProvidersInput {
    pub group_id: String,
    pub provider_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonoizeRuntimeConfig {
    pub request_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    pub enable_estimated_billing: bool,
    pub passive_failure_count_threshold: u32,
    pub passive_cooldown_seconds: u64,
    pub passive_window_seconds: u64,
    pub passive_rate_limit_cooldown_seconds: u64,
    pub active_enabled: bool,
    pub active_interval_seconds: u64,
    pub active_success_threshold: u32,
    pub active_method: String,
    pub active_probe_model: Option<String>,
    pub global_transforms: Vec<TransformRuleConfig>,
    pub global_model_redirects: Vec<crate::users::ModelRedirectRule>,
    #[serde(skip)]
    pub(crate) compiled_global_model_redirects: Vec<crate::users::CompiledModelRedirectRule>,
    pub reasoning_suffix_map: HashMap<String, String>,
    pub codex_model_ids: Vec<String>,
    pub pricing_profile_model_patterns: Vec<PricingProfilePattern>,
    pub extra_fields_whitelist: HashMap<String, Vec<String>>,
    pub strip_cross_protocol_nested_extra: bool,
    pub request_capture_enabled: bool,
    pub request_capture_retention_days: u64,
    pub mask_sensitive_info: bool,
    pub affinity_enabled: bool,
    pub affinity_idle_ttl_seconds: u64,
    pub affinity_failback_mode: AffinityFailbackMode,
    pub affinity_failback_delay_seconds: u64,
    pub moderation_enabled: bool,
    #[serde(skip)]
    pub content_firewall: Option<crate::content_firewall::ContentFirewall>,
    pub moderation_judge: crate::moderation_judge::JudgeConfig,
}

impl Default for MonoizeRuntimeConfig {
    fn default() -> Self {
        Self {
            request_timeout_ms: 30_000,
            stream_idle_timeout_ms: 120_000,
            enable_estimated_billing: true,
            passive_failure_count_threshold: 100,
            passive_cooldown_seconds: 60,
            passive_window_seconds: 30,
            passive_rate_limit_cooldown_seconds: 15,
            active_enabled: true,
            active_interval_seconds: 30,
            active_success_threshold: 1,
            active_method: "completion".to_string(),
            active_probe_model: None,
            global_transforms: Vec::new(),
            global_model_redirects: Vec::new(),
            compiled_global_model_redirects: Vec::new(),
            reasoning_suffix_map: default_reasoning_suffix_map(),
            codex_model_ids: Vec::new(),
            pricing_profile_model_patterns: default_pricing_profile_model_patterns(),
            extra_fields_whitelist: HashMap::new(),
            strip_cross_protocol_nested_extra: true,
            request_capture_enabled: false,
            request_capture_retention_days: 1,
            mask_sensitive_info: true,
            affinity_enabled: true,
            affinity_idle_ttl_seconds: 30 * 60,
            affinity_failback_mode: AffinityFailbackMode::Sticky,
            affinity_failback_delay_seconds: 5 * 60,
            moderation_enabled: true,
            content_firewall: crate::content_firewall::ContentFirewall::compile(
                crate::content_firewall::DEFAULT_BLOCKED_WORDS,
            ),
            moderation_judge: crate::moderation_judge::JudgeConfig {
                enabled: false,
                base_url: String::new(),
                api_key: String::new(),
                model: String::new(),
                timeout_ms: 8000,
            },
        }
    }
}

impl MonoizeRuntimeConfig {
    pub fn set_global_model_redirects(
        &mut self,
        rules: Vec<crate::users::ModelRedirectRule>,
    ) -> Result<(), String> {
        let compiled = crate::users::compile_model_redirects(&rules)?;
        self.global_model_redirects = rules;
        self.compiled_global_model_redirects = compiled;
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChannelHealthState {
    pub healthy: bool,
    pub last_success_at: Option<i64>,
    pub cooldown_until: Option<i64>,
    pub probe_success_count: u32,
    pub last_probe_at: Option<i64>,
    pub passive_failure_timestamps: VecDeque<i64>,
}

#[derive(Debug, Clone)]
pub struct ChannelAffinityBinding {
    pub provider_id: String,
    pub channel_id: String,
    pub bound_at: i64,
    pub last_used_at: i64,
    pub expires_at: i64,
}

pub const DEFAULT_CHANNEL_AFFINITY_MAX_ENTRIES: usize = 4096;
pub const DEFAULT_CHANNEL_AFFINITY_CLEANUP_INTERVAL_SECONDS: u64 = 60;
pub const DEFAULT_CHANNEL_HEALTH_MAX_ENTRIES: usize = 10_000;
pub const DEFAULT_CHANNEL_PASSIVE_FAILURE_SAMPLE_MAX_ENTRIES: usize = 1024;
pub const DEFAULT_PROVIDER_REORDER_MAX_IDS: usize = 199;
const TRANSFORM_MIGRATION_BATCH_SIZE: usize = 199;
const TRANSFORM_MIGRATION_MARKER: &str = "migration.provider_transform_rule_ids.v2";
const OBSOLETE_TRANSFORM_MIGRATION_MARKER: &str = "migration.provider_transform_rule_ids.v1";

fn parse_positive_entry_limit(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_provider_reorder_limit(raw: Option<&str>) -> usize {
    parse_positive_entry_limit(raw, DEFAULT_PROVIDER_REORDER_MAX_IDS)
        .min(DEFAULT_PROVIDER_REORDER_MAX_IDS)
}

fn provider_reorder_max_ids() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        parse_provider_reorder_limit(
            std::env::var("MONOIZE_PROVIDER_REORDER_MAX_IDS")
                .ok()
                .as_deref(),
        )
    })
}

pub fn channel_affinity_max_entries() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        parse_positive_entry_limit(
            std::env::var("MONOIZE_CHANNEL_AFFINITY_MAX_ENTRIES")
                .ok()
                .as_deref(),
            DEFAULT_CHANNEL_AFFINITY_MAX_ENTRIES,
        )
    })
}

pub fn channel_affinity_cleanup_interval() -> Duration {
    static INTERVAL: OnceLock<Duration> = OnceLock::new();
    *INTERVAL.get_or_init(|| {
        parse_channel_affinity_cleanup_interval(
            std::env::var("MONOIZE_CHANNEL_AFFINITY_CLEANUP_INTERVAL_SECONDS")
                .ok()
                .as_deref(),
        )
    })
}

fn parse_channel_affinity_cleanup_interval(raw: Option<&str>) -> Duration {
    let seconds = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CHANNEL_AFFINITY_CLEANUP_INTERVAL_SECONDS);
    Duration::from_secs(seconds)
}

pub fn cleanup_channel_affinity(
    cache: &mut HashMap<String, ChannelAffinityBinding>,
    now_ts: i64,
) -> usize {
    let previous_len = cache.len();
    cache.retain(|_, binding| now_ts < binding.expires_at);
    previous_len - cache.len()
}

pub fn channel_health_max_entries() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        parse_positive_entry_limit(
            std::env::var("MONOIZE_CHANNEL_HEALTH_MAX_ENTRIES")
                .ok()
                .as_deref(),
            DEFAULT_CHANNEL_HEALTH_MAX_ENTRIES,
        )
    })
}

pub fn channel_passive_failure_sample_max_entries() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        parse_positive_entry_limit(
            std::env::var("MONOIZE_CHANNEL_PASSIVE_FAILURE_SAMPLE_MAX_ENTRIES")
                .ok()
                .as_deref(),
            DEFAULT_CHANNEL_PASSIVE_FAILURE_SAMPLE_MAX_ENTRIES,
        )
    })
}

pub fn effective_passive_failure_threshold(resolved_threshold: u32) -> usize {
    effective_passive_failure_threshold_with_limit(
        resolved_threshold,
        channel_passive_failure_sample_max_entries(),
    )
}

fn effective_passive_failure_threshold_with_limit(resolved_threshold: u32, limit: usize) -> usize {
    (resolved_threshold.max(1) as usize).min(limit.max(1))
}

pub fn prepare_channel_health_insert(
    health: &mut HashMap<String, ChannelHealthState>,
    key: &str,
) -> bool {
    prepare_channel_health_insert_with_limit(health, key, channel_health_max_entries())
}

fn prepare_channel_health_insert_with_limit(
    health: &mut HashMap<String, ChannelHealthState>,
    key: &str,
    limit: usize,
) -> bool {
    health.contains_key(key) || health.len() < limit
}

pub fn missing_channel_health_is_saturated(
    health: &HashMap<String, ChannelHealthState>,
    key: &str,
) -> bool {
    missing_channel_health_is_saturated_with_limit(health, key, channel_health_max_entries())
}

fn missing_channel_health_is_saturated_with_limit(
    health: &HashMap<String, ChannelHealthState>,
    key: &str,
    limit: usize,
) -> bool {
    !health.contains_key(key) && health.len() >= limit
}

/// HSK-7a: capacity saturation makes a Channel ineligible without any other visible signal,
/// so the condition is published as a metric on every observation and as a single warning
/// per saturation episode. The warning resets once the map drops below capacity, which
/// keeps a permanently full map from flooding the log.
static CHANNEL_HEALTH_SATURATION_WARNED: AtomicBool = AtomicBool::new(false);

pub fn note_channel_health_saturation(limit: usize) {
    metrics::counter!("monoize_channel_health_saturated_total").increment(1);
    if !CHANNEL_HEALTH_SATURATION_WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            limit,
            "channel health map is at capacity; every unkeyed channel is treated as ineligible until an entry slot frees up"
        );
    }
}

pub fn reset_channel_health_saturation_warning() {
    CHANNEL_HEALTH_SATURATION_WARNED.store(false, Ordering::Relaxed);
}

impl ChannelHealthState {
    pub fn new() -> Self {
        Self {
            healthy: true,
            last_success_at: None,
            cooldown_until: None,
            probe_success_count: 0,
            last_probe_at: None,
            passive_failure_timestamps: VecDeque::new(),
        }
    }

    pub fn status(&self, now_ts: i64) -> &'static str {
        if self.healthy {
            return "healthy";
        }
        if let Some(until) = self.cooldown_until {
            if now_ts < until {
                return "unhealthy";
            }
        }
        "probing"
    }
}

#[derive(Clone)]
pub struct MonoizeRoutingStore {
    db: DbPool,
    snapshot: RegistrySnapshotState,
}

/// Process-wide registry generation (DPT-RR3): every write to the provider,
/// channel-model, or group registry bumps it so routing snapshots rebuild.
static REGISTRY_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub fn bump_registry_generation() {
    REGISTRY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

pub fn registry_generation() -> u64 {
    REGISTRY_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

#[allow(dead_code)]
struct RegistrySnapshot {
    generation: u64,
    built_at: std::time::Instant,
    /// `list_providers()` result: priority ASC, created_at ASC.
    providers: Vec<MonoizeProvider>,
    /// `load_channels_bulk(None)` result keyed by provider id.
    channels: HashMap<String, MonoizeChannel>,
    /// group_id -> account_class string, for class-scoped filtering.
    group_class: HashMap<String, String>,
}

#[derive(Clone, Default)]
struct RegistrySnapshotState {
    installed: std::sync::Arc<
        std::sync::RwLock<Option<std::sync::Arc<RegistrySnapshot>>>,
    >,
    rebuild: std::sync::Arc<tokio::sync::Mutex<()>>,
}

fn registry_snapshot_ttl() -> Option<std::time::Duration> {
    match std::env::var("MONOIZE_REGISTRY_SNAPSHOT_TTL_MS") {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(ms) => Some(std::time::Duration::from_millis(ms)),
            Err(_) => Some(std::time::Duration::from_millis(60_000)),
        },
        Err(_) => Some(std::time::Duration::from_millis(60_000)),
    }
}

fn default_enabled() -> bool {
    true
}

fn decode_database_bool(
    entity: &str,
    entity_id: &str,
    field: &str,
    value: i32,
) -> Result<bool, String> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(format!(
            "{entity} {entity_id} invalid {field} boolean: expected 0 or 1, got {value}"
        )),
    }
}

fn generate_provider_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// CP-INV-14: trim and treat empty as NULL (follow-global).
fn normalized_proxy_url(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// CP-INV-15: reserved header names that must not be overridden by Channel extras.
const EXTRA_HEADERS_RESERVED: &[&str] = &[
    "authorization",
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "expect",
    "te",
    "trailer",
];

const EXTRA_HEADERS_MAX_ENTRIES: usize = 16;
const EXTRA_HEADERS_MAX_KEY_LEN: usize = 128;
const EXTRA_HEADERS_MAX_VALUE_LEN: usize = 4096;

fn validate_channel_extra_headers(
    channel_name: &str,
    headers: &BTreeMap<String, String>,
) -> Result<(), String> {
    if headers.len() > EXTRA_HEADERS_MAX_ENTRIES {
        return Err(format!(
            "channel '{channel_name}' extra_headers must contain at most {EXTRA_HEADERS_MAX_ENTRIES} entries"
        ));
    }
    let mut seen_lower: HashSet<String> = HashSet::new();
    for (key, value) in headers {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "channel '{channel_name}' extra_headers key must not be empty"
            ));
        }
        if trimmed.len() > EXTRA_HEADERS_MAX_KEY_LEN {
            return Err(format!(
                "channel '{channel_name}' extra_headers key exceeds {EXTRA_HEADERS_MAX_KEY_LEN} characters"
            ));
        }
        let valid_token = trimmed.bytes().all(|byte| {
            matches!(byte,
                b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.'
                | b'^' | b'_' | b'`' | b'|' | b'~'
                | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
        });
        if !valid_token {
            return Err(format!(
                "channel '{channel_name}' extra_headers key '{trimmed}' contains invalid characters"
            ));
        }
        let lower = trimmed.to_ascii_lowercase();
        // Case-insensitive duplicate keys would make the effective value ambiguous.
        if !seen_lower.insert(lower.clone()) {
            return Err(format!(
                "channel '{channel_name}' extra_headers contains duplicate key '{trimmed}'"
            ));
        }
        if EXTRA_HEADERS_RESERVED.contains(&lower.as_str()) {
            return Err(format!(
                "channel '{channel_name}' extra_headers key '{trimmed}' is reserved and must not be set"
            ));
        }
        if value.len() > EXTRA_HEADERS_MAX_VALUE_LEN {
            return Err(format!(
                "channel '{channel_name}' extra_headers value for '{trimmed}' exceeds {EXTRA_HEADERS_MAX_VALUE_LEN} characters"
            ));
        }
        if value.contains('\r') || value.contains('\n') {
            return Err(format!(
                "channel '{channel_name}' extra_headers value for '{trimmed}' must not contain CR or LF"
            ));
        }
    }
    Ok(())
}

/// CP-INV-15a: trim keys, drop nothing else, canonical JSON with sorted keys;
/// an empty map persists as NULL.
fn normalized_extra_headers_json(raw: Option<&BTreeMap<String, String>>) -> Option<String> {
    let headers = raw?;
    let mut trimmed: BTreeMap<&str, &String> = BTreeMap::new();
    for (key, value) in headers {
        trimmed.insert(key.trim(), value);
    }
    if trimmed.is_empty() {
        return None;
    }
    serde_json::to_string(&trimmed).ok()
}

fn decode_extra_headers(raw: Option<String>) -> Result<Option<BTreeMap<String, String>>, String> {
    let Some(text) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("invalid stored extra_headers JSON: {e}"))
}

fn decode_channel_row(
    row: &QueryResult,
    models: HashMap<String, MonoizeModelEntry>,
) -> Result<MonoizeChannel, String> {
    let id: String = row.try_get("", "id").map_err(|e| e.to_string())?;
    let provider_type_raw: String = row
        .try_get("", "provider_type")
        .map_err(|e| format!("channel {id} missing provider_type: {e}"))?;
    let provider_type = MonoizeProviderType::from_str(&provider_type_raw)
        .ok_or_else(|| format!("channel {id} invalid provider type: {provider_type_raw}"))?;
    Ok(MonoizeChannel {
        id: id.clone(),
        name: row.try_get("", "name").map_err(|e| e.to_string())?,
        provider_type,
        base_url: row.try_get("", "base_url").map_err(|e| e.to_string())?,
        api_key: row.try_get("", "api_key").map_err(|e| e.to_string())?,
        enabled: decode_database_bool(
            "channel",
            &id,
            "enabled",
            row.try_get::<i32>("", "enabled")
                .map_err(|e| e.to_string())?,
        )?,
        passive_failure_count_threshold_override: row
            .try_get::<Option<i32>>("", "passive_failure_count_threshold_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u32(
                    &id,
                    "passive_failure_count_threshold_override",
                    i64::from(value),
                )
            })
            .transpose()?,
        passive_cooldown_seconds_override: row
            .try_get::<Option<i32>>("", "passive_cooldown_seconds_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u64(&id, "passive_cooldown_seconds_override", i64::from(value))
            })
            .transpose()?,
        passive_window_seconds_override: row
            .try_get::<Option<i32>>("", "passive_window_seconds_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u64(&id, "passive_window_seconds_override", i64::from(value))
            })
            .transpose()?,
        passive_rate_limit_cooldown_seconds_override: row
            .try_get::<Option<i32>>("", "passive_rate_limit_cooldown_seconds_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u64(
                    &id,
                    "passive_rate_limit_cooldown_seconds_override",
                    i64::from(value),
                )
            })
            .transpose()?,
        models,
        active_probe_enabled_override: row
            .try_get::<Option<i32>>("", "active_probe_enabled_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_database_bool("channel", &id, "active_probe_enabled_override", value)
            })
            .transpose()?,
        active_probe_interval_seconds_override: row
            .try_get::<Option<i32>>("", "active_probe_interval_seconds_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u64(
                    &id,
                    "active_probe_interval_seconds_override",
                    i64::from(value),
                )
            })
            .transpose()?,
        active_probe_success_threshold_override: row
            .try_get::<Option<i32>>("", "active_probe_success_threshold_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u32(
                    &id,
                    "active_probe_success_threshold_override",
                    i64::from(value),
                )
            })
            .transpose()?,
        active_probe_model_override: row
            .try_get("", "active_probe_model_override")
            .map_err(|e| e.to_string())?,
        affinity_enabled_override: row
            .try_get::<Option<i32>>("", "affinity_enabled_override")
            .map_err(|e| e.to_string())?
            .map(|value| decode_database_bool("channel", &id, "affinity_enabled_override", value))
            .transpose()?,
        affinity_idle_ttl_seconds_override: row
            .try_get::<Option<i32>>("", "affinity_idle_ttl_seconds_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u64(&id, "affinity_idle_ttl_seconds_override", i64::from(value))
            })
            .transpose()?,
        affinity_failback_mode_override: row
            .try_get::<Option<String>>("", "affinity_failback_mode_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                AffinityFailbackMode::from_str(&value).ok_or_else(|| {
                    format!("channel {id} invalid affinity_failback_mode_override: {value}")
                })
            })
            .transpose()?,
        affinity_failback_delay_seconds_override: row
            .try_get::<Option<i32>>("", "affinity_failback_delay_seconds_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_nonnegative_u64(
                    &id,
                    "affinity_failback_delay_seconds_override",
                    i64::from(value),
                )
            })
            .transpose()?,
        proxy_url: row
            .try_get::<Option<String>>("", "proxy_url")
            .map_err(|e| e.to_string())?
            .filter(|value| !value.trim().is_empty()),
        extra_headers: decode_extra_headers(
            row.try_get::<Option<String>>("", "extra_headers")
                .map_err(|e| e.to_string())?,
        )?,
        session_affinity_auto: row
            .try_get::<Option<i32>>("", "session_affinity_auto")
            .map_err(|e| e.to_string())?
            .map(|value| decode_database_bool("channel", &id, "session_affinity_auto", value))
            .transpose()?,
        allow_missing_usage: decode_database_bool(
            "channel",
            &id,
            "allow_missing_usage",
            row.try_get::<i32>("", "allow_missing_usage")
                .map_err(|e| e.to_string())?,
        )?,
        _healthy: None,
        _last_success_at: None,
        _health_status: None,
        _unhealthy_models: None,
        _probing_models: None,
        _cooldown_until: None,
    })
}

fn decode_provider_row(
    row: &QueryResult,
    channel: MonoizeChannel,
) -> Result<MonoizeProvider, String> {
    let id: String = row.try_get("", "id").map_err(|e| e.to_string())?;
    let mut transforms: Vec<TransformRuleConfig> = serde_json::from_str(
        &row.try_get::<String>("", "transforms")
            .map_err(|e| format!("provider {id} missing transforms column: {e}"))?,
    )
    .map_err(|e| format!("provider {id} invalid transforms JSON: {e}"))?;
    canonicalize_transform_rules(&mut transforms);
    let api_type_overrides: Vec<ApiTypeOverride> = serde_json::from_str(
        &row.try_get::<String>("", "api_type_overrides")
            .map_err(|e| format!("provider {id} missing api_type_overrides column: {e}"))?,
    )
    .map_err(|e| format!("provider {id} invalid api_type_overrides JSON: {e}"))?;
    let created_at = DateTime::parse_from_rfc3339(
        &row.try_get::<String>("", "created_at")
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("provider {id} invalid created_at RFC3339: {e}"))?
    .with_timezone(&Utc);
    let updated_at = DateTime::parse_from_rfc3339(
        &row.try_get::<String>("", "updated_at")
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("provider {id} invalid updated_at RFC3339: {e}"))?
    .with_timezone(&Utc);
    Ok(MonoizeProvider {
        id: id.clone(),
        name: row.try_get("", "name").map_err(|e| e.to_string())?,
        channel,
        pricing_profile: row
            .try_get("", "pricing_profile")
            .map_err(|e| e.to_string())?,
        multiplier: row
            .try_get::<String>("", "multiplier")
            .map_err(|e| e.to_string())?
            .parse()?,
        channel_max_retries: row
            .try_get("", "channel_max_retries")
            .map_err(|e| e.to_string())?,
        channel_retry_interval_ms: row
            .try_get("", "channel_retry_interval_ms")
            .map_err(|e| e.to_string())?,
        circuit_breaker_enabled: decode_database_bool(
            "provider",
            &id,
            "circuit_breaker_enabled",
            row.try_get::<i32>("", "circuit_breaker_enabled")
                .map_err(|e| e.to_string())?,
        )?,
        per_model_circuit_break: decode_database_bool(
            "provider",
            &id,
            "per_model_circuit_break",
            row.try_get::<i32>("", "per_model_circuit_break")
                .map_err(|e| e.to_string())?,
        )?,
        transforms,
        api_type_overrides,
        active_probe_enabled_override: row
            .try_get::<Option<i32>>("", "active_probe_enabled_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_database_bool("provider", &id, "active_probe_enabled_override", value)
            })
            .transpose()?,
        active_probe_interval_seconds_override: row
            .try_get::<Option<i32>>("", "active_probe_interval_seconds_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u64(
                    &id,
                    "active_probe_interval_seconds_override",
                    i64::from(value),
                )
            })
            .transpose()?,
        active_probe_success_threshold_override: row
            .try_get::<Option<i32>>("", "active_probe_success_threshold_override")
            .map_err(|e| e.to_string())?
            .map(|value| {
                decode_positive_u32(
                    &id,
                    "active_probe_success_threshold_override",
                    i64::from(value),
                )
            })
            .transpose()?,
        active_probe_model_override: row
            .try_get("", "active_probe_model_override")
            .map_err(|e| e.to_string())?,
        request_timeout_ms_override: row
            .try_get::<Option<i32>>("", "request_timeout_ms_override")
            .map_err(|e| e.to_string())?
            .map(|value| decode_positive_u64(&id, "request_timeout_ms_override", i64::from(value)))
            .transpose()?,
        max_input_tokens: row
            .try_get::<Option<i64>>("", "max_input_tokens")
            .map_err(|e| format!("provider {id} invalid max_input_tokens column: {e}"))?
            .map(|value| decode_positive_u64(&id, "max_input_tokens", value))
            .transpose()?,
        prompt_cache_incompatible_with_tools: row
            .try_get::<Option<i32>>("", "prompt_cache_incompatible_with_tools")
            .map_err(|e| {
                format!("provider {id} invalid prompt_cache_incompatible_with_tools column: {e}")
            })?
            .map(|value| {
                decode_database_bool(
                    "provider",
                    &id,
                    "prompt_cache_incompatible_with_tools",
                    value,
                )
            })
            .transpose()?
            .unwrap_or(false),
        extra_fields_whitelist: row
            .try_get::<Option<String>>("", "extra_fields_whitelist")
            .map_err(|e| format!("provider {id} invalid extra_fields_whitelist column: {e}"))?
            .map(|raw| {
                serde_json::from_str::<Vec<String>>(&raw)
                    .map_err(|e| format!("provider {id} invalid extra_fields_whitelist JSON: {e}"))
            })
            .transpose()?,
        strip_cross_protocol_nested_extra: row
            .try_get::<Option<i32>>("", "strip_cross_protocol_nested_extra")
            .map_err(|e| {
                format!("provider {id} invalid strip_cross_protocol_nested_extra column: {e}")
            })?
            .map(|value| {
                decode_database_bool("provider", &id, "strip_cross_protocol_nested_extra", value)
            })
            .transpose()?,
        group_id: row.try_get("", "group_id").map_err(|e| e.to_string())?,
        enabled: decode_database_bool(
            "provider",
            &id,
            "enabled",
            row.try_get::<i32>("", "enabled")
                .map_err(|e| e.to_string())?,
        )?,
        priority: row.try_get("", "priority").map_err(|e| e.to_string())?,
        created_at,
        updated_at,
    })
}

fn generate_channel_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn provider_projection(alias: &str) -> String {
    let p = if alias.is_empty() {
        String::new()
    } else {
        format!("{alias}.")
    };
    format!(
        "SELECT {p}id, {p}name, {p}pricing_profile, {p}multiplier, {p}channel_max_retries,
                {p}channel_retry_interval_ms, {p}circuit_breaker_enabled,
                {p}per_model_circuit_break, {p}transforms, {p}api_type_overrides,
                {p}active_probe_enabled_override, {p}active_probe_interval_seconds_override,
                {p}active_probe_success_threshold_override, {p}active_probe_model_override,
                {p}request_timeout_ms_override, {p}max_input_tokens,
                {p}prompt_cache_incompatible_with_tools, {p}extra_fields_whitelist,
                {p}strip_cross_protocol_nested_extra, {p}group_id,
                {p}enabled, {p}priority, {p}created_at, {p}updated_at"
    )
}

impl MonoizeRoutingStore {
    pub async fn new(db: DbPool) -> Result<Self, String> {
        let store = Self {
            db,
            snapshot: RegistrySnapshotState::default(),
        };
        store.migrate_transform_rule_ids().await?;
        Ok(store)
    }

    /// Replica-side constructor per PRP11: skips canonicalization writes that the
    /// primary already performed on the shared database.
    pub async fn new_read_only(db: DbPool) -> Result<Self, String> {
        Ok(Self {
            db,
            snapshot: RegistrySnapshotState::default(),
        })
    }

    /// DPT-RR2/RR4: serve the installed snapshot when it is younger than the TTL
    /// and was built at the current generation; otherwise rebuild it once while
    /// waiters keep being served by the previous snapshot.
    async fn registry_snapshot(&self) -> Result<std::sync::Arc<RegistrySnapshot>, String> {
        let ttl = registry_snapshot_ttl();
        if let Some(inst) = Self::snapshot_is_current(&self.snapshot, ttl) {
            return Ok(inst);
        }
        let _guard = self.snapshot.rebuild.lock().await;
        if let Some(inst) = Self::snapshot_is_current(&self.snapshot, ttl) {
            return Ok(inst);
        }
        let generation = registry_generation();
        let channels = self.load_channels_bulk_uncached(None).await?;
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "{} FROM monoize_providers ORDER BY priority ASC, created_at ASC",
                    provider_projection("")
                ),
                vec![],
            ))
            .await
            .map_err(|e| e.to_string())?;
        let mut channels_by_provider = channels.clone();
        let mut providers = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.try_get("", "id").map_err(|e| e.to_string())?;
            let channel = channels_by_provider
                .remove(&id)
                .ok_or_else(|| format!("provider {id} missing embedded channel"))?;
            providers.push(decode_provider_row(row, channel)?);
        }
        let group_rows = self
            .db
            .read()
            .query_all(
                self.db
                    .stmt("SELECT id, account_class FROM monoize_groups", vec![]),
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut group_class = HashMap::with_capacity(group_rows.len());
        for row in group_rows {
            let id: String = row.try_get("", "id").map_err(|e| e.to_string())?;
            let class: String = row.try_get("", "account_class").map_err(|e| e.to_string())?;
            group_class.insert(id, class);
        }
        let inst = std::sync::Arc::new(RegistrySnapshot {
            generation,
            built_at: std::time::Instant::now(),
            providers,
            channels,
            group_class,
        });
        *self.snapshot.installed.write().expect("registry snapshot lock") = Some(inst.clone());
        Ok(inst)
    }

    fn snapshot_is_current(
        state: &RegistrySnapshotState,
        ttl: Option<std::time::Duration>,
    ) -> Option<std::sync::Arc<RegistrySnapshot>> {
        let installed = state.installed.read().expect("registry snapshot lock");
        let inst = installed.as_ref()?;
        let fresh = ttl
            .map(|ttl| inst.built_at.elapsed() <= ttl)
            .unwrap_or(false);
        if fresh && inst.generation == registry_generation() {
            Some(inst.clone())
        } else {
            None
        }
    }

    async fn migrate_transform_rule_ids(&self) -> Result<(), String> {
        let marker = self
            .db
            .read()
            .query_one(self.db.stmt(
                "SELECT value FROM system_settings WHERE key = $1",
                vec![TRANSFORM_MIGRATION_MARKER.into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        let marker_value = marker
            .map(|row| {
                row.try_get::<String>("", "value")
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        if marker_value.as_deref() == Some("complete") {
            return Ok(());
        }

        let mut last_id: Option<String> = None;
        loop {
            let tx = self.db.begin_write().await.map_err(|e| e.to_string())?;
            let (sql, values) = match last_id.as_deref() {
                Some(last_id) => (
                    format!(
                        "SELECT id, transforms FROM monoize_providers
                         WHERE id > $1 ORDER BY id ASC LIMIT {TRANSFORM_MIGRATION_BATCH_SIZE}"
                    ),
                    vec![last_id.into()],
                ),
                None => (
                    format!(
                        "SELECT id, transforms FROM monoize_providers
                         ORDER BY id ASC LIMIT {TRANSFORM_MIGRATION_BATCH_SIZE}"
                    ),
                    vec![],
                ),
            };
            let rows = tx
                .query_all(self.db.stmt(&sql, values))
                .await
                .map_err(|e| e.to_string())?;
            if rows.is_empty() {
                tx.commit().await.map_err(|e| e.to_string())?;
                break;
            }
            let batch_len = rows.len();
            let next_last_id: String = rows
                .last()
                .expect("non-empty transform migration batch")
                .try_get("", "id")
                .map_err(|e| e.to_string())?;
            let mut updates = Vec::with_capacity(batch_len);
            for row in rows {
                let id: String = row.try_get("", "id").map_err(|e| e.to_string())?;
                let raw: String = row.try_get("", "transforms").map_err(|e| e.to_string())?;
                let Ok(mut transforms) = serde_json::from_str::<Vec<TransformRuleConfig>>(&raw)
                else {
                    tracing::warn!(provider_id = %id, "skip invalid provider transforms during transform id migration");
                    continue;
                };
                if !canonicalize_transform_rules(&mut transforms) {
                    continue;
                }
                let encoded = serde_json::to_string(&transforms).map_err(|e| e.to_string())?;
                updates.push((id, encoded));
            }

            if !updates.is_empty() {
                let mut values: Vec<sea_orm::Value> = Vec::with_capacity(updates.len() * 2);
                let mut cases = Vec::with_capacity(updates.len());
                let mut ids = Vec::with_capacity(updates.len());
                for (id, transforms) in &updates {
                    let id_index = values.len() + 1;
                    values.push(id.clone().into());
                    ids.push(format!("${id_index}"));
                    let transforms_index = values.len() + 1;
                    values.push(transforms.clone().into());
                    cases.push(format!("WHEN ${id_index} THEN ${transforms_index}"));
                }
                tx.execute(self.db.stmt(
                    &format!(
                        "UPDATE monoize_providers
                         SET transforms = CASE id {} ELSE transforms END
                         WHERE id IN ({})",
                        cases.join(" "),
                        ids.join(", ")
                    ),
                    values,
                ))
                .await
                .map_err(|e| e.to_string())?;
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            last_id = Some(next_last_id);
            if batch_len < TRANSFORM_MIGRATION_BATCH_SIZE {
                break;
            }
        }

        let tx = self.db.begin_write().await.map_err(|e| e.to_string())?;
        tx.execute(self.db.stmt(
            "INSERT INTO system_settings (key, value, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            vec![
                TRANSFORM_MIGRATION_MARKER.into(),
                "complete".into(),
                Utc::now().to_rfc3339().into(),
            ],
        ))
        .await
        .map_err(|e| e.to_string())?;
        tx.execute(self.db.stmt(
            "DELETE FROM system_settings WHERE key = $1",
            vec![OBSOLETE_TRANSFORM_MIGRATION_MARKER.into()],
        ))
        .await
        .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())
    }

    pub async fn provider_count(&self) -> Result<i64, String> {
        let row = self
            .db
            .read()
            .query_one(
                self.db
                    .stmt("SELECT COUNT(*) as cnt FROM monoize_providers", vec![]),
            )
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "count query returned no rows".to_string())?;
        row.try_get("", "cnt").map_err(|e| e.to_string())
    }

    async fn load_channels_bulk(
        &self,
        provider_id: Option<&str>,
    ) -> Result<HashMap<String, MonoizeChannel>, String> {
        match provider_id {
            None => Ok(self.registry_snapshot().await?.channels.clone()),
            Some(id) => self.load_channels_bulk_uncached(Some(id)).await,
        }
    }

    async fn load_channels_bulk_uncached(
        &self,
        provider_id: Option<&str>,
    ) -> Result<HashMap<String, MonoizeChannel>, String> {
        let filter = provider_id.map(|_| " WHERE id = $1").unwrap_or("");
        let values = provider_id.map(|id| vec![id.into()]).unwrap_or_default();
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "SELECT channel_id AS id, id AS provider_id, channel_name AS name,
                            channel_provider_type AS provider_type, channel_base_url AS base_url,
                            channel_api_key AS api_key, channel_enabled AS enabled,
                            channel_passive_failure_count_threshold_override AS passive_failure_count_threshold_override,
                            channel_passive_cooldown_seconds_override AS passive_cooldown_seconds_override,
                            channel_passive_window_seconds_override AS passive_window_seconds_override,
                            channel_passive_rate_limit_cooldown_seconds_override AS passive_rate_limit_cooldown_seconds_override,
                            channel_active_probe_enabled_override AS active_probe_enabled_override,
                            channel_active_probe_interval_seconds_override AS active_probe_interval_seconds_override,
                            channel_active_probe_success_threshold_override AS active_probe_success_threshold_override,
                            channel_active_probe_model_override AS active_probe_model_override,
                            channel_affinity_enabled_override AS affinity_enabled_override,
                            channel_affinity_idle_ttl_seconds_override AS affinity_idle_ttl_seconds_override,
                            channel_affinity_failback_mode_override AS affinity_failback_mode_override,
                            channel_affinity_failback_delay_seconds_override AS affinity_failback_delay_seconds_override,
                            channel_proxy_url AS proxy_url, channel_extra_headers AS extra_headers,
                            channel_session_affinity_auto AS session_affinity_auto,
                            channel_allow_missing_usage AS allow_missing_usage
                     FROM monoize_providers{filter}
                     ORDER BY created_at ASC, id ASC"
                ),
                values,
            ))
            .await
            .map_err(|e| e.to_string())?;
        let model_filter = provider_id
            .map(|_| " WHERE pm.provider_id = $1")
            .unwrap_or("");
        let model_values = provider_id.map(|id| vec![id.into()]).unwrap_or_default();
        let model_rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "SELECT p.channel_id, pm.model_name, pm.redirect,
                            pm.pricing_profile_mode, pm.pricing_profile_override,
                            pm.multiplier_override
                     FROM monoize_provider_models pm
                     JOIN monoize_providers p ON p.id = pm.provider_id{model_filter}
                     ORDER BY p.channel_id ASC, pm.model_name ASC"
                ),
                model_values,
            ))
            .await
            .map_err(|e| e.to_string())?;
        let mut models_by_channel = HashMap::<String, HashMap<String, MonoizeModelEntry>>::new();
        for row in model_rows {
            let channel_id: String = row.try_get("", "channel_id").map_err(|e| e.to_string())?;
            let model_name: String = row.try_get("", "model_name").map_err(|e| e.to_string())?;
            let pricing_profile_mode = match row
                .try_get::<String>("", "pricing_profile_mode")
                .map_err(|e| e.to_string())?
                .as_str()
            {
                "inherit" => PricingProfileMode::Inherit,
                "override" => PricingProfileMode::Override,
                "unpriced" => PricingProfileMode::Unpriced,
                value => return Err(format!("invalid pricing_profile_mode: {value}")),
            };
            let multiplier_override = row
                .try_get::<Option<String>>("", "multiplier_override")
                .map_err(|e| e.to_string())?
                .map(|value| value.parse())
                .transpose()?;
            models_by_channel.entry(channel_id).or_default().insert(
                model_name,
                MonoizeModelEntry {
                    redirect: row.try_get("", "redirect").map_err(|e| e.to_string())?,
                    pricing_profile_mode,
                    pricing_profile_override: row
                        .try_get("", "pricing_profile_override")
                        .map_err(|e| e.to_string())?,
                    multiplier_override,
                },
            );
        }
        let mut result = HashMap::<String, MonoizeChannel>::new();
        for row in rows {
            let provider_id: String = row.try_get("", "provider_id").map_err(|e| e.to_string())?;
            let channel_id: String = row.try_get("", "id").map_err(|e| e.to_string())?;
            let channel = decode_channel_row(
                &row,
                models_by_channel.remove(&channel_id).unwrap_or_default(),
            )?;
            if result.insert(provider_id.clone(), channel).is_some() {
                return Err(format!(
                    "provider {provider_id} returned multiple embedded channels"
                ));
            }
        }
        Ok(result)
    }

    pub async fn list_providers(&self) -> Result<Vec<MonoizeProvider>, String> {
        // DPT-RR1: the routing hot path must not scan the registry tables per
        // request; serve the process snapshot instead.
        Ok(self.registry_snapshot().await?.providers.clone())
    }

    /// PP-ENT6: reports every account class that already reaches each named pricing Profile,
    /// through a Provider-level Profile or a model-level override. `exclude_provider_id` skips
    /// the Provider being updated, so keeping its own Profile is never a conflict.
    pub async fn pricing_profile_account_classes(
        &self,
        profiles: &[String],
        exclude_provider_id: Option<&str>,
    ) -> Result<Vec<(String, crate::users::AccountClass)>, String> {
        if profiles.is_empty() {
            return Ok(Vec::new());
        }
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(profiles.len() * 2 + 2);
        let provider_placeholders = (0..profiles.len())
            .map(|index| format!("${}", index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        values.extend(profiles.iter().cloned().map(Into::into));
        let model_placeholders = (0..profiles.len())
            .map(|index| format!("${}", profiles.len() + index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        values.extend(profiles.iter().cloned().map(Into::into));

        let exclude_index = profiles.len() * 2 + 1;
        let (provider_filter, model_filter) = match exclude_provider_id {
            Some(id) => {
                values.push(id.into());
                values.push(id.into());
                (
                    format!(" AND p.id <> ${exclude_index}"),
                    format!(" AND p.id <> ${}", exclude_index + 1),
                )
            }
            None => (String::new(), String::new()),
        };

        let sql = format!(
            "SELECT p.pricing_profile AS profile, g.account_class AS account_class              FROM monoize_providers p JOIN monoize_groups g ON g.id = p.group_id              WHERE p.pricing_profile IN ({provider_placeholders}){provider_filter}              UNION              SELECT pm.pricing_profile_override AS profile, g.account_class AS account_class              FROM monoize_provider_models pm              JOIN monoize_providers p ON p.id = pm.provider_id              JOIN monoize_groups g ON g.id = p.group_id              WHERE pm.pricing_profile_mode = 'override'                AND pm.pricing_profile_override IN ({model_placeholders}){model_filter}"
        );
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(&sql, values))
            .await
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .map(|row| {
                let profile: String = row.try_get("", "profile").map_err(|e| e.to_string())?;
                let raw: String = row
                    .try_get("", "account_class")
                    .map_err(|e| e.to_string())?;
                let account_class = crate::users::AccountClass::from_str(&raw)
                    .ok_or_else(|| format!("invalid persisted account_class: {raw:?}"))?;
                Ok((profile, account_class))
            })
            .collect()
    }

    pub async fn list_providers_by_account_class(
        &self,
        account_class: crate::users::AccountClass,
    ) -> Result<Vec<MonoizeProvider>, String> {
        let snapshot = self.registry_snapshot().await?;
        let class = account_class.as_str();
        Ok(snapshot
            .providers
            .iter()
            .filter(|provider| {
                snapshot
                    .group_class
                    .get(&provider.group_id)
                    .map(|value| value.as_str())
                    == Some(class)
            })
            .cloned()
            .collect())
    }

    pub async fn available_model_names(
        &self,
        candidates: &[String],
    ) -> Result<HashSet<String>, String> {
        self.available_model_names_filtered(candidates, None).await
    }

    pub async fn available_model_names_for_account_class(
        &self,
        candidates: &[String],
        account_class: crate::users::AccountClass,
    ) -> Result<HashSet<String>, String> {
        self.available_model_names_filtered(candidates, Some(account_class.as_str()))
            .await
    }

    /// Snapshot-served intersection of the candidate models with the models of
    /// enabled providers (optionally restricted to one group account class).
    /// Equivalent to the previous per-request DISTINCT SQL scan (DPT-RR1).
    async fn available_model_names_filtered(
        &self,
        candidates: &[String],
        account_class: Option<&str>,
    ) -> Result<HashSet<String>, String> {
        if candidates.is_empty() {
            return Ok(HashSet::new());
        }
        let snapshot = self.registry_snapshot().await?;
        let wanted: HashSet<&str> = candidates.iter().map(String::as_str).collect();
        let mut available = HashSet::new();
        for provider in &snapshot.providers {
            if !provider.enabled || !provider.channel.enabled {
                continue;
            }
            if let Some(class) = account_class
                && snapshot
                    .group_class
                    .get(&provider.group_id)
                    .map(|value| value.as_str())
                    != Some(class)
            {
                continue;
            }
            for model in provider.channel.models.keys() {
                if wanted.contains(model.as_str()) {
                    available.insert(model.clone());
                }
            }
        }
        Ok(available)
    }

    pub async fn list_available_model_names(&self) -> Result<Vec<String>, String> {
        let sql = "SELECT DISTINCT pm.model_name FROM monoize_provider_models pm
                   JOIN monoize_providers p ON p.id = pm.provider_id
                   WHERE p.enabled = 1 AND p.channel_enabled = 1
                   ORDER BY pm.model_name ASC";
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(sql, vec![]))
            .await
            .map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|row| row.try_get("", "model_name").map_err(|e| e.to_string()))
            .collect()
    }

    /// Model names from `candidates` that at least one enabled Provider of the given Groups
    /// serves through an enabled Channel. Group scoping for model discovery (AKG-M1).
    pub async fn available_model_names_for_groups(
        &self,
        candidates: &[String],
        group_ids: &[String],
    ) -> Result<HashSet<String>, String> {
        if candidates.is_empty() || group_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let candidates = candidates
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let groups = group_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut available = HashSet::new();
        // The bound mirrors the routing index chunk limit: one statement stays below the
        // portable SQLite parameter ceiling with both IN lists expanded.
        const LOOKUP_CHUNK_SIZE: usize = 199;
        for chunk in candidates.chunks(LOOKUP_CHUNK_SIZE) {
            for group_chunk in groups.chunks(LOOKUP_CHUNK_SIZE) {
                let model_placeholders = (0..chunk.len())
                    .map(|index| format!("${}", index + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let group_placeholders = (0..group_chunk.len())
                    .map(|index| format!("${}", chunk.len() + index + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT DISTINCT pm.model_name FROM monoize_provider_models pm
                     JOIN monoize_providers p ON p.id = pm.provider_id
                     WHERE p.enabled = 1 AND p.channel_enabled = 1
                       AND p.group_id IN ({group_placeholders})
                       AND pm.model_name IN ({model_placeholders})"
                );
                let mut values: Vec<sea_orm::Value> =
                    chunk.iter().cloned().map(Into::into).collect();
                values.extend(group_chunk.iter().cloned().map(Into::into));
                let rows = self
                    .db
                    .read()
                    .query_all(self.db.stmt(&sql, values))
                    .await
                    .map_err(|error| error.to_string())?;
                for row in rows {
                    available.insert(
                        row.try_get("", "model_name")
                            .map_err(|error| error.to_string())?,
                    );
                }
            }
        }
        Ok(available)
    }

    pub async fn list_providers_for_model(
        &self,
        model: &str,
    ) -> Result<Vec<MonoizeProvider>, String> {
        let providers = self.list_providers().await?;
        Ok(providers
            .into_iter()
            .filter(|provider| {
                provider.enabled
                    && provider.channel.enabled
                    && provider.channel.models.contains_key(model)
            })
            .collect())
    }

    pub async fn list_providers_for_model_and_account_class(
        &self,
        model: &str,
        account_class: crate::users::AccountClass,
    ) -> Result<Vec<MonoizeProvider>, String> {
        let providers = self.list_providers_for_model(model).await?;
        if providers.is_empty() {
            return Ok(providers);
        }
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                "SELECT p.id FROM monoize_providers p JOIN monoize_groups g ON g.id = p.group_id WHERE g.account_class = $1",
                vec![account_class.as_str().into()],
            ))
            .await
            .map_err(|error| error.to_string())?;
        let allowed = rows
            .into_iter()
            .map(|row| {
                row.try_get::<String>("", "id")
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<HashSet<_>, _>>()?;
        Ok(providers
            .into_iter()
            .filter(|provider| allowed.contains(&provider.id))
            .collect())
    }

    pub async fn list_active_probe_candidates(&self) -> Result<Vec<MonoizeProvider>, String> {
        let providers = self.list_providers().await?;
        Ok(providers
            .into_iter()
            .filter(|provider| {
                provider.enabled
                    && provider.circuit_breaker_enabled
                    && provider.channel.enabled
                    && !provider.channel.models.is_empty()
            })
            .collect())
    }

    /// Resolves the Group a Provider write targets. An empty id selects the default Group,
    /// so callers that must agree with the write on the target Group use this instead of
    /// reading the request field directly.
    pub async fn resolve_provider_group_id(&self, group_id: &str) -> Result<String, String> {
        let group_id = group_id.trim();
        if group_id.is_empty() {
            let row = self
                .db
                .read()
                .query_one(self.db.stmt(
                    "SELECT id FROM monoize_groups WHERE is_default = 1 LIMIT 1",
                    vec![],
                ))
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "default group row missing (GR-D2 violated)".to_string())?;
            let default_id: String = row.try_get("", "id").map_err(|e| e.to_string())?;
            return Ok(default_id);
        }
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(
                "SELECT 1 AS one FROM monoize_groups WHERE id = $1",
                vec![group_id.into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        if row.is_none() {
            return Err(format!("unknown group id: {group_id}"));
        }
        Ok(group_id.to_string())
    }

    pub async fn get_provider(&self, id: &str) -> Result<Option<MonoizeProvider>, String> {
        Ok(self
            .list_providers()
            .await?
            .into_iter()
            .find(|provider| provider.id == id))
    }

    pub async fn create_provider(
        &self,
        input: CreateMonoizeProviderInput,
    ) -> Result<MonoizeProvider, String> {
        validate_provider_input(&input.name, &input.channel, &input.api_type_overrides)?;
        if !input.confirm_public_exposure {
            return Err("public_exposure_confirmation_required:provider".to_string());
        }
        let public_name = crate::public_name::canonicalize_public_name(&input.name)?;
        let channel_public_name =
            crate::public_name::canonicalize_public_name(&input.channel.name)?;
        if let Some(v) = input.active_probe_interval_seconds_override {
            if !(1..=i32::MAX as u64).contains(&v) {
                return Err(
                    "active_probe_interval_seconds_override must be between 1 and 2147483647"
                        .to_string(),
                );
            }
        }
        if let Some(v) = input.active_probe_success_threshold_override {
            if !(1..=i32::MAX as u32).contains(&v) {
                return Err(
                    "active_probe_success_threshold_override must be between 1 and 2147483647"
                        .to_string(),
                );
            }
        }
        if let Some(v) = input.request_timeout_ms_override {
            if !(1..=i32::MAX as u64).contains(&v) {
                return Err(
                    "request_timeout_ms_override must be between 1 and 2147483647".to_string(),
                );
            }
        }
        if let Some(v) = input.max_input_tokens
            && !(1..=i32::MAX as u64).contains(&v)
        {
            return Err("max_input_tokens must be between 1 and 2147483647".to_string());
        }
        if input.channel_retry_interval_ms < 0 {
            return Err("channel_retry_interval_ms must be >= 0".to_string());
        }

        let id = generate_provider_id();
        let now = Utc::now();
        let pricing_profile = normalize_pricing_profile(input.pricing_profile.as_deref())?;
        // Resolve before begin_write: the registry lookup uses the read pool,
        // which on single-connection SQLite would deadlock behind our own
        // write transaction.
        let group_id = self.resolve_provider_group_id(&input.group_id).await?;
        let txn = self.db.begin_write().await.map_err(|e| e.to_string())?;

        let priority = match input.priority {
            Some(v) => v,
            None => {
                if self.db.is_postgres() {
                    txn.execute_unprepared(
                        "LOCK TABLE monoize_providers IN SHARE ROW EXCLUSIVE MODE",
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                }
                let row = txn
                    .query_one(self.db.stmt(
                        "SELECT CAST(MAX(priority) AS BIGINT) AS max_p FROM monoize_providers",
                        vec![],
                    ))
                    .await
                    .map_err(|e| e.to_string())?;
                let max_priority = row
                    .map(|row| {
                        row.try_get::<Option<i64>>("", "max_p")
                            .map_err(|e| e.to_string())
                    })
                    .transpose()?
                    .flatten();
                let next_priority = max_priority
                    .unwrap_or(-1)
                    .checked_add(1)
                    .ok_or_else(|| "provider priority overflow".to_string())?;
                i32::try_from(next_priority)
                    .map_err(|_| "provider priority exceeds signed 32-bit range".to_string())?
            }
        };

        let mut transforms = input.transforms.clone();
        canonicalize_transform_rules(&mut transforms);
        let transforms_json = serde_json::to_string(&transforms).map_err(|e| e.to_string())?;
        let api_type_overrides_json =
            serde_json::to_string(&input.api_type_overrides).map_err(|e| e.to_string())?;
        let extra_fields_whitelist_json: Option<String> = input
            .extra_fields_whitelist
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string()));
        let strip_cross_proto = input.strip_cross_protocol_nested_extra;

        let channel = &input.channel;
        txn.execute(self.db.stmt(
                r#"INSERT INTO monoize_providers (
                     id, group_id, name, public_name, public_name_key, priority, enabled,
                     pricing_profile, multiplier, configuration_generation, created_at, updated_at,
                     channel_id, channel_name, channel_public_name,
                     channel_public_name_key, channel_provider_type, channel_base_url, channel_api_key,
                     channel_enabled, channel_max_retries,
                     channel_passive_failure_count_threshold_override,
                     channel_passive_cooldown_seconds_override, channel_passive_window_seconds_override,
                     channel_passive_rate_limit_cooldown_seconds_override,
                     channel_active_probe_enabled_override, channel_active_probe_interval_seconds_override,
                     channel_active_probe_success_threshold_override, channel_active_probe_model_override,
                     channel_affinity_enabled_override, channel_affinity_idle_ttl_seconds_override,
                     channel_affinity_failback_mode_override, channel_affinity_failback_delay_seconds_override,
                     channel_proxy_url, channel_extra_headers, channel_session_affinity_auto,
                     channel_allow_missing_usage, transforms, api_type_overrides,
                     active_probe_enabled_override, active_probe_interval_seconds_override,
                     active_probe_success_threshold_override, active_probe_model_override,
                     request_timeout_ms_override, max_input_tokens,
                     prompt_cache_incompatible_with_tools, extra_fields_whitelist,
                     strip_cross_protocol_nested_extra, circuit_breaker_enabled,
                     per_model_circuit_break, channel_retry_interval_ms
                   ) VALUES (
                     $1, $2, $3, $4, $5, $6, $7, NULL, '1', 1, $8, $8, $9, $10, $11,
                     $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25,
                     $26, $27, $28, $29, $30, $31, $32, $33, $34, $35, $36, $37, $38, $39,
                     $40, $41, $42, $43, $44, $45, $46, $47)"#,
                vec![
                    id.clone().into(),
                    group_id.into(),
                    input.name.clone().into(),
                    public_name.value.clone().into(),
                    SeaValue::Bytes(Some(Box::new(public_name.key))),
                    SeaValue::Int(Some(priority)),
                    SeaValue::Int(Some(if input.enabled { 1 } else { 0 })),
                    now.to_rfc3339().into(),
                    generate_channel_id().into(),
                    channel.name.clone().into(),
                    channel_public_name.value.clone().into(),
                    SeaValue::Bytes(Some(Box::new(channel_public_name.key))),
                    channel.provider_type.as_str().into(),
                    channel.base_url.clone().into(),
                    channel.api_key.clone().unwrap_or_default().into(),
                    SeaValue::Int(Some(if channel.enabled { 1 } else { 0 })),
                    SeaValue::Int(Some(input.channel_max_retries)),
                    opt_u64_to_value(channel.passive_failure_count_threshold_override.map(u64::from)),
                    opt_u64_to_value(channel.passive_cooldown_seconds_override),
                    opt_u64_to_value(channel.passive_window_seconds_override),
                    opt_u64_to_value(channel.passive_rate_limit_cooldown_seconds_override),
                    opt_bool_to_value(channel.active_probe_enabled_override),
                    opt_u64_to_value(channel.active_probe_interval_seconds_override),
                    opt_u64_to_value(channel.active_probe_success_threshold_override.map(u64::from)),
                    channel.active_probe_model_override.clone().into(),
                    opt_bool_to_value(channel.affinity_enabled_override),
                    opt_u64_to_value(channel.affinity_idle_ttl_seconds_override),
                    channel.affinity_failback_mode_override.map(|m| m.as_str().to_string()).into(),
                    opt_u64_to_value(channel.affinity_failback_delay_seconds_override),
                    normalized_proxy_url(channel.proxy_url.as_deref()).into(),
                    normalized_extra_headers_json(channel.extra_headers.as_ref()).into(),
                    opt_bool_to_value(channel.session_affinity_auto),
                    SeaValue::Int(Some(if channel.allow_missing_usage { 1 } else { 0 })),
                    transforms_json.clone().into(),
                    api_type_overrides_json.clone().into(),
                    opt_bool_to_value(input.active_probe_enabled_override),
                    opt_u64_to_value(input.active_probe_interval_seconds_override),
                    opt_u64_to_value(input.active_probe_success_threshold_override.map(u64::from)),
                    input.active_probe_model_override.clone().into(),
                    opt_u64_to_value(input.request_timeout_ms_override),
                    opt_u64_to_value(input.max_input_tokens),
                    opt_bool_to_value(input.prompt_cache_incompatible_with_tools),
                    extra_fields_whitelist_json.clone().into(),
                    opt_bool_to_value(strip_cross_proto),
                    SeaValue::Int(Some(if input.circuit_breaker_enabled { 1 } else { 0 })),
                    SeaValue::Int(Some(if input.per_model_circuit_break { 1 } else { 0 })),
                    SeaValue::Int(Some(input.channel_retry_interval_ms)),
                ],
            )).await.map_err(|error| map_provider_public_name_write_error(
                &error.to_string(),
                &public_name.value,
                &channel_public_name.value,
            ))?;

        txn.execute(self.db.stmt(
            "UPDATE monoize_providers SET pricing_profile = $1, multiplier = $2 WHERE id = $3",
            vec![
                pricing_profile.into(),
                input.multiplier.to_string().into(),
                id.clone().into(),
            ],
        ))
        .await
        .map_err(|e| e.to_string())?;

        self.replace_channel_on(&*txn, &id, &input.channel).await?;
        txn.commit().await.map_err(|e| e.to_string())?;

        bump_registry_generation();
        self.get_provider(&id)
            .await?
            .ok_or_else(|| "provider not found after create".to_string())
    }

    pub async fn update_provider(
        &self,
        id: &str,
        input: UpdateMonoizeProviderInput,
    ) -> Result<MonoizeProvider, String> {
        let existing_provider = self
            .get_provider(id)
            .await?
            .ok_or_else(|| "provider not found".to_string())?;
        if let Some(channel) = &input.channel {
            validate_channel(channel, false)?;
        }
        let provider_public_name = input
            .name
            .as_deref()
            .map(crate::public_name::canonicalize_public_name)
            .transpose()?;
        let channel_public_name = input
            .channel
            .as_ref()
            .map(|channel| crate::public_name::canonicalize_public_name(&channel.name))
            .transpose()?;
        let existing_public_name =
            crate::public_name::canonicalize_public_name(&existing_provider.name)?;
        let existing_channel_public_name =
            crate::public_name::canonicalize_public_name(&existing_provider.channel.name)?;
        let changes_public_name = provider_public_name
            .as_ref()
            .is_some_and(|name| name.value != existing_public_name.value)
            || channel_public_name
                .as_ref()
                .is_some_and(|name| name.value != existing_channel_public_name.value);
        if changes_public_name && !input.confirm_public_exposure {
            return Err("public_exposure_confirmation_required:provider".to_string());
        }
        if let Some(Some(v)) = input.active_probe_interval_seconds_override {
            if !(1..=i32::MAX as u64).contains(&v) {
                return Err(
                    "active_probe_interval_seconds_override must be between 1 and 2147483647"
                        .to_string(),
                );
            }
        }
        if let Some(Some(v)) = input.active_probe_success_threshold_override {
            if !(1..=i32::MAX as u32).contains(&v) {
                return Err(
                    "active_probe_success_threshold_override must be between 1 and 2147483647"
                        .to_string(),
                );
            }
        }
        if let Some(Some(v)) = input.request_timeout_ms_override {
            if !(1..=i32::MAX as u64).contains(&v) {
                return Err(
                    "request_timeout_ms_override must be between 1 and 2147483647".to_string(),
                );
            }
        }
        if let Some(Some(v)) = input.max_input_tokens
            && !(1..=i32::MAX as u64).contains(&v)
        {
            return Err("max_input_tokens must be between 1 and 2147483647".to_string());
        }
        if let Some(v) = input.channel_retry_interval_ms {
            if v < 0 {
                return Err("channel_retry_interval_ms must be >= 0".to_string());
            }
        }

        if let Some(api_type_overrides) = &input.api_type_overrides {
            validate_api_type_overrides(api_type_overrides)?;
        }

        let mut set_clauses = Vec::new();
        let mut values: Vec<SeaValue> = Vec::new();
        let mut push_value = |column: &str, value: SeaValue| {
            let index = values.len() + 1;
            set_clauses.push(format!("{column} = ${index}"));
            values.push(value);
        };
        if let Some(value) = &input.name {
            push_value("name", value.clone().into());
            let public_name = provider_public_name
                .as_ref()
                .expect("input name has canonical public name");
            push_value("public_name", public_name.value.clone().into());
            push_value(
                "public_name_key",
                SeaValue::Bytes(Some(Box::new(public_name.key.clone()))),
            );
        }
        if let Some(value) = input.channel_max_retries {
            push_value("channel_max_retries", SeaValue::Int(Some(value)));
        }
        if let Some(value) = input.channel_retry_interval_ms {
            push_value("channel_retry_interval_ms", SeaValue::Int(Some(value)));
        }
        if let Some(value) = input.circuit_breaker_enabled {
            push_value(
                "circuit_breaker_enabled",
                SeaValue::Int(Some(if value { 1 } else { 0 })),
            );
        }
        if let Some(value) = input.per_model_circuit_break {
            push_value(
                "per_model_circuit_break",
                SeaValue::Int(Some(if value { 1 } else { 0 })),
            );
        }
        if let Some(mut transforms) = input.transforms.clone() {
            canonicalize_transform_rules(&mut transforms);
            push_value(
                "transforms",
                serde_json::to_string(&transforms)
                    .map_err(|e| e.to_string())?
                    .into(),
            );
        }
        if let Some(value) = &input.api_type_overrides {
            push_value(
                "api_type_overrides",
                serde_json::to_string(value)
                    .map_err(|e| e.to_string())?
                    .into(),
            );
        }
        if let Some(value) = input.active_probe_enabled_override {
            push_value("active_probe_enabled_override", opt_bool_to_value(value));
        }
        if let Some(value) = input.active_probe_interval_seconds_override {
            push_value(
                "active_probe_interval_seconds_override",
                opt_u64_to_value(value),
            );
        }
        if let Some(value) = input.active_probe_success_threshold_override {
            push_value(
                "active_probe_success_threshold_override",
                opt_u64_to_value(value.map(u64::from)),
            );
        }
        if let Some(value) = &input.active_probe_model_override {
            push_value("active_probe_model_override", value.clone().into());
        }
        if let Some(value) = input.request_timeout_ms_override {
            push_value("request_timeout_ms_override", opt_u64_to_value(value));
        }
        if let Some(value) = input.max_input_tokens {
            push_value("max_input_tokens", opt_u64_to_value(value));
        }
        if let Some(value) = input.prompt_cache_incompatible_with_tools {
            push_value(
                "prompt_cache_incompatible_with_tools",
                SeaValue::Int(Some(if value { 1 } else { 0 })),
            );
        }
        if let Some(value) = &input.extra_fields_whitelist {
            let encoded = value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| e.to_string())?;
            push_value("extra_fields_whitelist", encoded.into());
        }
        if let Some(value) = input.strip_cross_protocol_nested_extra {
            push_value(
                "strip_cross_protocol_nested_extra",
                opt_bool_to_value(value),
            );
        }
        if let Some(value) = &input.group_id {
            let resolved = self.resolve_provider_group_id(value).await?;
            push_value("group_id", resolved.into());
        }
        if let Some(value) = input.enabled {
            push_value("enabled", SeaValue::Int(Some(if value { 1 } else { 0 })));
        }
        if let Some(value) = input.priority {
            push_value("priority", SeaValue::Int(Some(value)));
        }
        if let Some(value) = &input.pricing_profile {
            push_value(
                "pricing_profile",
                normalize_pricing_profile(value.as_deref())?.into(),
            );
        }
        if let Some(value) = input.multiplier {
            push_value("multiplier", value.to_string().into());
        }
        push_value("updated_at", Utc::now().to_rfc3339().into());
        drop(push_value);
        set_clauses.push("configuration_generation = configuration_generation + 1".to_string());

        let id_index = values.len() + 1;
        values.push(id.into());
        let txn = self.db.begin_write().await.map_err(|e| e.to_string())?;
        let result = txn
            .execute(self.db.stmt(
                &format!(
                    "UPDATE monoize_providers SET {} WHERE id = ${id_index}",
                    set_clauses.join(", ")
                ),
                values,
            ))
            .await
            .map_err(|error| {
                let public_name = provider_public_name
                    .as_ref()
                    .map(|name| name.value.as_str())
                    .unwrap_or(existing_public_name.value.as_str());
                map_provider_public_name_write_error(
                    &error.to_string(),
                    public_name,
                    existing_channel_public_name.value.as_str(),
                )
            })?;
        if result.rows_affected() == 0 {
            return Err("provider not found".to_string());
        }

        if let Some(channel) = &input.channel {
            self.replace_channel_on(&*txn, id, channel).await?;
        }

        txn.commit().await.map_err(|e| e.to_string())?;

        bump_registry_generation();
        self.get_provider(id)
            .await?
            .ok_or_else(|| "provider not found after update".to_string())
    }

    pub async fn delete_provider(&self, id: &str) -> Result<(), String> {
        let result = self
            .db
            .write()
            .await
            .execute(self.db.stmt(
                "DELETE FROM monoize_providers WHERE id = $1",
                vec![id.into()],
            ))
            .await
            .map_err(|e| e.to_string())?;

        if result.rows_affected() == 0 {
            return Err("provider not found".to_string());
        }

        bump_registry_generation();
        Ok(())
    }

    pub async fn reorder_providers(&self, input: ReorderProvidersInput) -> Result<(), String> {
        if input.group_id.is_empty() {
            return Err("group_id must not be empty".to_string());
        }
        if input.provider_ids.len() > provider_reorder_max_ids() {
            return Err(format!(
                "provider reorder accepts at most {} ids",
                provider_reorder_max_ids()
            ));
        }
        let mut uniq = HashSet::new();
        for id in &input.provider_ids {
            if !uniq.insert(id.clone()) {
                return Err("provider_ids contains duplicates".to_string());
            }
        }

        let txn = self.db.begin_write().await.map_err(|e| e.to_string())?;
        if self.db.is_postgres() {
            txn.execute_unprepared("LOCK TABLE monoize_providers IN SHARE ROW EXCLUSIVE MODE")
                .await
                .map_err(|e| e.to_string())?;
        }
        let group_exists = txn
            .query_one(self.db.stmt(
                "SELECT id FROM monoize_groups WHERE id = $1",
                vec![input.group_id.clone().into()],
            ))
            .await
            .map_err(|e| e.to_string())?
            .is_some();
        if !group_exists {
            return Err("group_id does not identify a Group".to_string());
        }
        let rows = txn
            .query_all(self.db.stmt(
                "SELECT id FROM monoize_providers WHERE group_id = $1 ORDER BY id",
                vec![input.group_id.clone().into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        if rows.len() != input.provider_ids.len() {
            return Err("provider_ids must contain all providers exactly once".to_string());
        }

        let existing_ids: HashSet<String> = rows
            .into_iter()
            .map(|row| row.try_get("", "id").map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        let input_ids: HashSet<String> = input.provider_ids.iter().cloned().collect();
        if existing_ids != input_ids {
            return Err("provider_ids must contain all providers exactly once".to_string());
        }
        if input.provider_ids.is_empty() {
            return txn.commit().await.map_err(|e| e.to_string());
        }

        let mut values = Vec::with_capacity(input.provider_ids.len() * 2 + 1);
        let mut cases = Vec::with_capacity(input.provider_ids.len());
        for (priority, id) in input.provider_ids.iter().enumerate() {
            let id_index = values.len() + 1;
            values.push(id.clone().into());
            let priority_index = values.len() + 1;
            values.push(SeaValue::Int(Some(priority as i32)));
            cases.push(format!("WHEN ${id_index} THEN ${priority_index}"));
        }
        let updated_at_index = values.len() + 1;
        values.push(Utc::now().to_rfc3339().into());
        let group_id_index = values.len() + 1;
        values.push(input.group_id.into());
        txn.execute(self.db.stmt(
            &format!(
                "UPDATE monoize_providers
                 SET priority = CASE id {} END,
                     updated_at = ${updated_at_index},
                     configuration_generation = configuration_generation + 1
                 WHERE group_id = ${group_id_index}",
                cases.join(" ")
            ),
            values,
        ))
        .await
        .map_err(|e| e.to_string())?;
        txn.commit().await.map_err(|e| e.to_string())
    }

    async fn replace_channel_on(
        &self,
        conn: &impl ConnectionTrait,
        provider_id: &str,
        channel: &CreateMonoizeChannelInput,
    ) -> Result<(), String> {
        let existing = conn
                .query_one(self.db.stmt(
                    "SELECT channel_id, channel_api_key, channel_max_retries FROM monoize_providers WHERE id = $1",
                    vec![provider_id.into()],
                ))
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "provider not found".to_string())?;
        let channel_id: String = existing
            .try_get("", "channel_id")
            .map_err(|e| e.to_string())?;
        let api_key = channel
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .map(str::to_string)
            .or_else(|| existing.try_get("", "channel_api_key").ok())
            .ok_or_else(|| "channel api_key must not be empty".to_string())?;
        let public_name = crate::public_name::canonicalize_public_name(&channel.name)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(self.db.stmt(
            "UPDATE monoize_providers SET
                    channel_id = $1, channel_name = $2, channel_public_name = $3,
                    channel_public_name_key = $4, channel_provider_type = $5,
                    channel_base_url = $6, channel_api_key = $7, channel_enabled = $8,
                    channel_max_retries = $9,
                    channel_passive_failure_count_threshold_override = $10,
                    channel_passive_cooldown_seconds_override = $11,
                    channel_passive_window_seconds_override = $12,
                    channel_passive_rate_limit_cooldown_seconds_override = $13,
                    channel_active_probe_enabled_override = $14,
                    channel_active_probe_interval_seconds_override = $15,
                    channel_active_probe_success_threshold_override = $16,
                    channel_active_probe_model_override = $17,
                    channel_affinity_enabled_override = $18,
                    channel_affinity_idle_ttl_seconds_override = $19,
                    channel_affinity_failback_mode_override = $20,
                    channel_affinity_failback_delay_seconds_override = $21,
                    channel_proxy_url = $22, channel_extra_headers = $23,
                    channel_session_affinity_auto = $24, channel_allow_missing_usage = $25,
                    updated_at = $26 WHERE id = $27",
            vec![
                        channel_id.into(),
                        channel.name.clone().into(),
                        public_name.value.clone().into(),
                        SeaValue::Bytes(Some(Box::new(public_name.key))),
                        channel.provider_type.as_str().into(),
                        channel.base_url.clone().into(),
                        api_key.into(),
                        SeaValue::Int(Some(if channel.enabled { 1 } else { 0 })),
                        existing
                            .try_get::<i32>("", "channel_max_retries")
                            .unwrap_or(0)
                            .into(),
                        opt_u64_to_value(
                            channel
                                .passive_failure_count_threshold_override
                                .map(u64::from),
                        ),
                        opt_u64_to_value(channel.passive_cooldown_seconds_override),
                        opt_u64_to_value(channel.passive_window_seconds_override),
                        opt_u64_to_value(channel.passive_rate_limit_cooldown_seconds_override),
                        opt_bool_to_value(channel.active_probe_enabled_override),
                        opt_u64_to_value(channel.active_probe_interval_seconds_override),
                        opt_u64_to_value(
                            channel
                                .active_probe_success_threshold_override
                                .map(u64::from),
                        ),
                        channel.active_probe_model_override.clone().into(),
                        opt_bool_to_value(channel.affinity_enabled_override),
                        opt_u64_to_value(channel.affinity_idle_ttl_seconds_override),
                        channel
                            .affinity_failback_mode_override
                            .map(|m| m.as_str().to_string())
                            .into(),
                        opt_u64_to_value(channel.affinity_failback_delay_seconds_override),
                        normalized_proxy_url(channel.proxy_url.as_deref()).into(),
                        normalized_extra_headers_json(channel.extra_headers.as_ref()).into(),
                        opt_bool_to_value(channel.session_affinity_auto),
                        SeaValue::Int(Some(if channel.allow_missing_usage { 1 } else { 0 })),
                        now.into(),
                        provider_id.into(),
                    ],
        ))
        .await
        .map_err(|error| {
            map_provider_public_name_write_error(
                &error.to_string(),
                "unchanged",
                &public_name.value,
            )
        })?;
        conn.execute(self.db.stmt(
            "DELETE FROM monoize_provider_models WHERE provider_id = $1",
            vec![provider_id.into()],
        ))
        .await
        .map_err(|e| e.to_string())?;
        let now = Utc::now().to_rfc3339();
        for (model, entry) in canonicalize_models(&channel.models)? {
            let key = model.as_bytes().to_vec();
            let search = model.to_ascii_lowercase().into_bytes();
            conn.execute(self.db.stmt(
                    "INSERT INTO monoize_provider_models
                     (provider_id, model_name, model_name_key, model_search_key, redirect,
                      pricing_profile_mode, pricing_profile_override, multiplier_override, created_at)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                    vec![provider_id.into(), model.into(), SeaValue::Bytes(Some(Box::new(key))),
                         SeaValue::Bytes(Some(Box::new(search))), entry.redirect.into(),
                         pricing_profile_mode_name(entry.pricing_profile_mode).into(),
                         entry.pricing_profile_override.into(),
                         entry.multiplier_override.map(|value| value.to_string()).into(),
                         now.clone().into()],
                )).await.map_err(|e| e.to_string())?;
        }
        bump_registry_generation();
        Ok(())
    }
}

fn opt_bool_to_value(v: Option<bool>) -> SeaValue {
    match v {
        Some(b) => SeaValue::Int(Some(if b { 1 } else { 0 })),
        None => SeaValue::Int(None),
    }
}

fn opt_u64_to_value(v: Option<u64>) -> SeaValue {
    match v {
        Some(n) => SeaValue::Int(Some(n as i32)),
        None => SeaValue::Int(None),
    }
}

fn decode_positive_u32(provider_id: &str, field: &str, value: i64) -> Result<u32, String> {
    u32::try_from(value)
        .ok()
        .filter(|v| *v >= 1)
        .ok_or_else(|| format!("provider {provider_id} invalid {field}: must be >= 1"))
}

fn decode_positive_u64(provider_id: &str, field: &str, value: i64) -> Result<u64, String> {
    u64::try_from(value)
        .ok()
        .filter(|v| *v >= 1)
        .ok_or_else(|| format!("provider {provider_id} invalid {field}: must be >= 1"))
}

fn decode_nonnegative_u64(provider_id: &str, field: &str, value: i64) -> Result<u64, String> {
    u64::try_from(value)
        .map_err(|_| format!("provider {provider_id} invalid {field}: must be >= 0"))
}

fn canonicalize_models(
    models: &HashMap<String, MonoizeModelEntry>,
) -> Result<HashMap<String, MonoizeModelEntry>, String> {
    let mut out = HashMap::new();
    for (model, entry) in models {
        let model = canonical_model_name(model)?;
        out.insert(
            model,
            MonoizeModelEntry {
                redirect: entry
                    .redirect
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                pricing_profile_mode: entry.pricing_profile_mode,
                pricing_profile_override: entry
                    .pricing_profile_override
                    .as_deref()
                    .map(str::trim)
                    .map(str::to_string),
                multiplier_override: entry.multiplier_override,
            },
        );
    }
    Ok(out)
}

/// CP-INV-16: a channel `base_url` MUST be an absolute `http` or `https` URL whose
/// host resolves (or, for a literal IP, maps) only to globally routable addresses.
/// Loopback, link-local, unspecified, multicast, and RFC 1918 / ULA ranges are
/// rejected unless `MONOIZE_ALLOW_PRIVATE_UPSTREAM=1` is set. The check exists to stop
/// a misconfigured Channel from turning the gateway into a request forwarder into the
/// machine's own network (SSRF).
fn validate_channel_base_url(base_url: &str) -> Result<(), String> {
    let trimmed = base_url.trim();
    let parsed = reqwest::Url::parse(trimmed).map_err(|_| {
        "channel base_url must be an absolute URL with scheme http or https".to_string()
    })?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("channel base_url must use scheme http or https".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "channel base_url must include a host".to_string())?;

    if private_upstream_addresses_allowed() {
        return Ok(());
    }

    // A literal IP is checked directly; a hostname is resolved so a private address
    // hidden behind a name (including a DNS rebind target) is caught at write time.
    // `Url::host_str` keeps IPv6 brackets ("[::1]"), which neither IpAddr::parse nor
    // ToSocketAddrs accept — strip them or every bracketed literal would fall
    // through to the unresolvable-name allowance.
    let bare_host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<IpAddr> = if let Ok(ip) = bare_host.parse::<IpAddr>() {
        vec![ip]
    } else {
        // The host may carry an explicit port; `ToSocketAddrs` requires a service.
        let port = parsed
            .port()
            .unwrap_or_else(|| if scheme == "https" { 443 } else { 80 });
        let mut resolved = Vec::new();
        match (host, port).to_socket_addrs() {
            Ok(iter) => {
                for addr in iter {
                    resolved.push(addr.ip());
                    if resolved.len() >= 8 {
                        break;
                    }
                }
            }
            Err(_) => {
                // Unresolvable names never reach a private network; the upstream call
                // will fail on its own. Validation must not reject on DNS failure here.
                return Ok(());
            }
        }
        resolved
    };

    if addresses.is_empty() {
        return Ok(());
    }
    for address in addresses {
        if is_private_or_local_ip(address) {
            return Err(format!(
                "channel base_url must not target a loopback, link-local, private, or reserved address (got {address})"
            ));
        }
    }
    Ok(())
}

/// True for loopback, link-local, unspecified, multicast, documentation, and the
/// private IPv4 (RFC 1918) / unique-local IPv6 (RFC 4193) ranges. All are
/// non-globally-routable destinations a relay must not be pointed at.
pub fn is_private_or_local_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_documentation()
                || octets[0] == 10
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 168)
                || octets[0] == 100 && (64..=127).contains(&octets[1]) // CGNAT 100.64/10
                || octets[0] == 0
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address (::ffff:a.b.c.d) is checked as the IPv4 it
            // carries, so the mapped form cannot bypass the V4 range checks.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_private_or_local_ip(IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
        }
    }
}

fn canonical_model_name(value: &str) -> Result<String, String> {
    let model = value.trim_matches(char::is_whitespace);
    let bytes = model.as_bytes();
    if !(1..=256).contains(&bytes.len()) {
        return Err("model name must contain 1 through 256 UTF-8 bytes".to_string());
    }
    if bytes.iter().any(|byte| matches!(*byte, 0x00..=0x1f | 0x7f)) {
        return Err("model name must not contain C0 or DEL control characters".to_string());
    }
    Ok(model.to_string())
}

fn map_provider_public_name_write_error(
    error: &str,
    provider_public_name: &str,
    channel_public_name: &str,
) -> String {
    let lower = error.to_ascii_lowercase();
    if !(lower.contains("unique") || lower.contains("duplicate")) {
        return error.to_string();
    }
    if lower.contains("channel_public_name") || lower.contains("uq_monoize_channel_public_name") {
        return format!("public_name_conflict:channel:{channel_public_name}");
    }
    if lower.contains("public_name") || lower.contains("uq_monoize_provider_public_name") {
        return format!("public_name_conflict:provider:{provider_public_name}");
    }
    error.to_string()
}

fn normalize_pricing_profile(profile: Option<&str>) -> Result<Option<String>, String> {
    profile
        .map(str::trim)
        .map(|value| {
            if value.is_empty() {
                Err("pricing_profile must be null or non-empty".to_string())
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()
}

fn pricing_profile_mode_name(mode: PricingProfileMode) -> &'static str {
    match mode {
        PricingProfileMode::Inherit => "inherit",
        PricingProfileMode::Override => "override",
        PricingProfileMode::Unpriced => "unpriced",
    }
}

fn validate_models(models: &HashMap<String, MonoizeModelEntry>) -> Result<(), String> {
    for (model, entry) in models {
        canonical_model_name(model)?;
        let profile_override =
            normalize_pricing_profile(entry.pricing_profile_override.as_deref())?;
        match entry.pricing_profile_mode {
            PricingProfileMode::Override if profile_override.is_none() => {
                return Err(format!(
                    "model {model} override mode requires pricing_profile_override"
                ));
            }
            PricingProfileMode::Inherit | PricingProfileMode::Unpriced
                if profile_override.is_some() =>
            {
                return Err(format!(
                    "model {model} pricing_profile_override requires override mode"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_channel(
    channel: &CreateMonoizeChannelInput,
    require_api_key: bool,
) -> Result<(), String> {
    let c = channel;
    if c.name.trim().is_empty() {
        return Err("channel name must not be empty".to_string());
    }
    if c.base_url.trim().is_empty() {
        return Err("channel base_url must not be empty".to_string());
    }
    validate_channel_base_url(&c.base_url)?;
    if require_api_key {
        let key = c.api_key.as_deref().unwrap_or("");
        if key.trim().is_empty() {
            return Err("channel api_key must not be empty".to_string());
        }
    }
    if let Some(headers) = &c.extra_headers {
        validate_channel_extra_headers(&c.name, headers)?;
    }
    if let Some(v) = c.passive_failure_count_threshold_override {
        if !(1..=i32::MAX as u32).contains(&v) {
            return Err(
                "channel passive_failure_count_threshold_override must be between 1 and 2147483647"
                    .to_string(),
            );
        }
    }
    if let Some(v) = c.passive_cooldown_seconds_override {
        if !(1..=i32::MAX as u64).contains(&v) {
            return Err(
                "channel passive_cooldown_seconds_override must be between 1 and 2147483647"
                    .to_string(),
            );
        }
    }
    if let Some(v) = c.passive_window_seconds_override {
        if !(1..=i32::MAX as u64).contains(&v) {
            return Err(
                "channel passive_window_seconds_override must be between 1 and 2147483647"
                    .to_string(),
            );
        }
    }
    if let Some(v) = c.passive_rate_limit_cooldown_seconds_override {
        if !(1..=i32::MAX as u64).contains(&v) {
            return Err(
                    "channel passive_rate_limit_cooldown_seconds_override must be between 1 and 2147483647".to_string(),
                );
        }
    }
    if let Some(v) = c.active_probe_interval_seconds_override {
        if !(1..=i32::MAX as u64).contains(&v) {
            return Err(
                "channel active_probe_interval_seconds_override must be between 1 and 2147483647"
                    .to_string(),
            );
        }
    }
    if let Some(v) = c.active_probe_success_threshold_override {
        if !(1..=i32::MAX as u32).contains(&v) {
            return Err(
                "channel active_probe_success_threshold_override must be between 1 and 2147483647"
                    .to_string(),
            );
        }
    }
    if let Some(v) = c.affinity_idle_ttl_seconds_override {
        if !(1..=i32::MAX as u64).contains(&v) {
            return Err(
                "channel affinity_idle_ttl_seconds_override must be between 1 and 2147483647"
                    .to_string(),
            );
        }
    }
    if let Some(v) = c.affinity_failback_delay_seconds_override {
        if v > i32::MAX as u64 {
            return Err(
                "channel affinity_failback_delay_seconds_override must be between 0 and 2147483647"
                    .to_string(),
            );
        }
    }
    validate_models(&c.models)?;
    let mut model_seen = HashSet::new();
    for model in c.models.keys() {
        let model = canonical_model_name(model)?;
        if !model_seen.insert(model.clone()) {
            return Err(format!(
                "channel '{}' has duplicate model '{}'",
                c.name, model
            ));
        }
    }
    Ok(())
}

fn validate_provider_input(
    name: &str,
    channel: &CreateMonoizeChannelInput,
    api_type_overrides: &[ApiTypeOverride],
) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("provider name must not be empty".to_string());
    }
    validate_channel(channel, true)?;
    validate_api_type_overrides(api_type_overrides)?;
    Ok(())
}

fn validate_api_type_overrides(overrides: &[ApiTypeOverride]) -> Result<(), String> {
    for (idx, entry) in overrides.iter().enumerate() {
        if entry.pattern.trim().is_empty() {
            return Err(format!(
                "api_type_overrides[{idx}].pattern must not be empty"
            ));
        }
    }
    Ok(())
}

pub fn apply_provider_api_key(
    request: reqwest::RequestBuilder,
    provider_type: MonoizeProviderType,
    api_key: &str,
) -> reqwest::RequestBuilder {
    match provider_type {
        MonoizeProviderType::Gemini => request.header("x-goog-api-key", api_key),
        MonoizeProviderType::Messages => request.header("x-api-key", api_key).bearer_auth(api_key),
        _ => request.bearer_auth(api_key),
    }
}

pub async fn probe_channel_list_models(
    client: &reqwest::Client,
    channel: &MonoizeChannel,
    timeout_ms: u64,
) -> bool {
    let base = channel.base_url.trim_end_matches('/');
    let url = format!("{base}/v1/models");

    let mut request = client.get(url).timeout(Duration::from_millis(timeout_ms));
    request = apply_provider_api_key(request, channel.provider_type, &channel.api_key);
    if channel.provider_type == MonoizeProviderType::Messages {
        request = request.header("anthropic-version", "2023-06-01");
    }
    let result = request.send().await;

    match result {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Resolves the effective API type for a given model by evaluating api_type_overrides
/// in order. First matching glob pattern wins; falls back to the default provider_type.
pub fn resolve_effective_api_type(
    overrides: &[ApiTypeOverride],
    default_type: MonoizeProviderType,
    model: &str,
) -> MonoizeProviderType {
    for entry in overrides {
        if glob_match(&entry.pattern, model) {
            return entry.api_type;
        }
    }
    default_type
}

fn glob_match(pattern: &str, value: &str) -> bool {
    crate::glob::case_sensitive_glob_match(pattern, value)
}

pub struct ChannelProbeOutcome {
    pub ok: bool,
    pub usage: Option<Value>,
    pub http_status: Option<u16>,
    pub error_code: Option<String>,
    pub error_type: Option<String>,
    pub error: Option<String>,
}

const PROBE_ERROR_BODY_MAX_CHARS: usize = 512;

fn truncate_probe_body(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= PROBE_ERROR_BODY_MAX_CHARS {
        return body.to_string();
    }
    let truncated: String = body.chars().take(PROBE_ERROR_BODY_MAX_CHARS).collect();
    format!("{truncated}…")
}

pub fn format_probe_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let code = status.as_u16();
    let reason = status.canonical_reason().unwrap_or("");
    let body = truncate_probe_body(body);
    if body.is_empty() {
        if reason.is_empty() {
            format!("upstream returned {code}")
        } else {
            format!("upstream returned {code} {reason}")
        }
    } else if reason.is_empty() {
        format!("upstream returned {code}: {body}")
    } else {
        format!("upstream returned {code} {reason}: {body}")
    }
}

fn probe_error_metadata(body: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return (None, None);
    };
    let error = value.get("error").unwrap_or(&value);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    (code, error_type)
}

fn probe_event_error_object(value: &Value) -> Option<&Value> {
    for candidate in [
        value.get("error"),
        value
            .get("response")
            .and_then(|response| response.get("error")),
    ] {
        match candidate {
            Some(Value::Null) | None => {}
            Some(error) => return Some(error),
        }
    }
    None
}

fn probe_stream_error(
    value: &Value,
    sse_event: &str,
) -> Option<(Option<String>, Option<String>, String)> {
    let event_type = value.get("type").and_then(Value::as_str);
    let error = probe_event_error_object(value);
    if error.is_none()
        && !matches!(
            event_type,
            Some("error" | "response.failed" | "response.cancelled")
        )
        && !matches!(
            sse_event,
            "error" | "response.failed" | "response.cancelled"
        )
    {
        return None;
    }
    let detail = error.unwrap_or(value);
    let code = detail
        .get("code")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let error_type = detail
        .get("type")
        .and_then(Value::as_str)
        .or(event_type)
        .or_else(|| (!sse_event.is_empty()).then_some(sse_event))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let message = detail
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| truncate_probe_body(&detail.to_string()));
    Some((
        code,
        error_type,
        format!("upstream stream error: {message}"),
    ))
}

async fn read_probe_stream(
    response: reqwest::Response,
    effective_type: MonoizeProviderType,
) -> ChannelProbeOutcome {
    let status = response.status().as_u16();
    let mut usage = None;
    let mut chat_terminal_chunk_seen = false;
    let mut stream = response.bytes_stream().eventsource();

    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                return ChannelProbeOutcome {
                    ok: false,
                    usage,
                    http_status: Some(status),
                    error_code: Some("upstream_stream_decode_failed".to_string()),
                    error_type: Some("stream_error".to_string()),
                    error: Some(format!("upstream stream decode failed: {error}")),
                };
            }
        };
        let data = event.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            if effective_type != MonoizeProviderType::ChatCompletion || !chat_terminal_chunk_seen {
                return ChannelProbeOutcome {
                    ok: false,
                    usage,
                    http_status: Some(status),
                    error_code: Some("upstream_stream_missing_terminal".to_string()),
                    error_type: Some("stream_error".to_string()),
                    error: Some(format!(
                        "upstream {} stream sent [DONE] before its terminal event",
                        effective_type.as_str()
                    )),
                };
            }
            return ChannelProbeOutcome {
                ok: true,
                usage,
                http_status: Some(status),
                error_code: None,
                error_type: None,
                error: None,
            };
        }

        let value = match serde_json::from_str::<Value>(data) {
            Ok(value) => value,
            Err(error) => {
                return ChannelProbeOutcome {
                    ok: false,
                    usage,
                    http_status: Some(status),
                    error_code: Some("upstream_stream_decode_failed".to_string()),
                    error_type: Some("stream_error".to_string()),
                    error: Some(format!(
                        "upstream stream event contains invalid JSON: {error}: {}",
                        truncate_probe_body(data)
                    )),
                };
            }
        };
        if let Some(stream_usage) = extract_probe_usage(&value) {
            usage = Some(stream_usage);
        }
        if let Some((error_code, error_type, error)) = probe_stream_error(&value, &event.event) {
            return ChannelProbeOutcome {
                ok: false,
                usage,
                http_status: Some(status),
                error_code,
                error_type,
                error: Some(error),
            };
        }

        let event_type = value.get("type").and_then(Value::as_str);
        match effective_type {
            MonoizeProviderType::Responses => {
                if matches!(
                    event.event.as_str(),
                    "response.completed" | "response.incomplete"
                ) || matches!(
                    event_type,
                    Some("response.completed" | "response.incomplete")
                ) {
                    return ChannelProbeOutcome {
                        ok: true,
                        usage,
                        http_status: Some(status),
                        error_code: None,
                        error_type: None,
                        error: None,
                    };
                }
            }
            MonoizeProviderType::ChatCompletion => {
                chat_terminal_chunk_seen |= value
                    .get("choices")
                    .and_then(Value::as_array)
                    .is_some_and(|choices| {
                        choices.iter().any(|choice| {
                            choice
                                .get("finish_reason")
                                .is_some_and(|reason| !reason.is_null())
                        })
                    });
            }
            MonoizeProviderType::Messages => {
                if event.event == "message_stop" || event_type == Some("message_stop") {
                    return ChannelProbeOutcome {
                        ok: true,
                        usage,
                        http_status: Some(status),
                        error_code: None,
                        error_type: None,
                        error: None,
                    };
                }
            }
            MonoizeProviderType::Gemini => {
                let terminal = value
                    .get("candidates")
                    .and_then(Value::as_array)
                    .is_some_and(|candidates| {
                        candidates.iter().any(|candidate| {
                            candidate
                                .get("finishReason")
                                .is_some_and(|reason| !reason.is_null())
                        })
                    });
                if terminal {
                    return ChannelProbeOutcome {
                        ok: true,
                        usage,
                        http_status: Some(status),
                        error_code: None,
                        error_type: None,
                        error: None,
                    };
                }
            }
            MonoizeProviderType::OpenaiImage
            | MonoizeProviderType::Replicate
            | MonoizeProviderType::OpenaiVideo
            | MonoizeProviderType::FalVideo => {}
        }
    }

    ChannelProbeOutcome {
        ok: false,
        usage,
        http_status: Some(status),
        error_code: Some("upstream_stream_missing_terminal".to_string()),
        error_type: Some("stream_error".to_string()),
        error: Some(format!(
            "upstream {} stream ended without a terminal event",
            effective_type.as_str()
        )),
    }
}

pub async fn probe_channel_completion(
    client: &reqwest::Client,
    channel: &MonoizeChannel,
    timeout_ms: u64,
    model: &str,
    provider_type: MonoizeProviderType,
    api_type_overrides: &[ApiTypeOverride],
    stream: bool,
) -> ChannelProbeOutcome {
    let effective_type = resolve_effective_api_type(api_type_overrides, provider_type, model);
    let base = channel.base_url.trim_end_matches('/');
    let (url, body, extra_headers, use_google_api_key_header) =
        build_probe_request(base, model, effective_type, stream);

    let mut request = client.post(&url).timeout(Duration::from_millis(timeout_ms));
    request = if use_google_api_key_header {
        request.header("x-goog-api-key", &channel.api_key)
    } else {
        apply_provider_api_key(request, effective_type, &channel.api_key)
    };
    for &(header_name, header_value) in extra_headers {
        request = request.header(header_name, header_value);
    }
    if let Some(channel_headers) = &channel.extra_headers {
        for (header_name, header_value) in channel_headers {
            request = request.header(header_name, header_value);
        }
    }
    // CM-AFF-3: OpenCode Go may reject probes that omit x-opencode-session.
    if is_direct_opencode_zen_url(&channel.base_url) {
        for name in SESSION_CACHE_HEADER_NAMES {
            if !extra_headers_contain(channel.extra_headers.as_ref(), name) {
                request = request.header(*name, OPENCODE_PROBE_SESSION_VALUE);
            }
        }
    }
    let result = request.json(&body).send().await;

    match result {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let (error_code, error_type) = probe_error_metadata(&body);
                return ChannelProbeOutcome {
                    ok: false,
                    usage: None,
                    http_status: Some(status.as_u16()),
                    error_code,
                    error_type,
                    error: Some(format_probe_http_error(status, &body)),
                };
            }
            if stream {
                return read_probe_stream(resp, effective_type).await;
            }
            let usage = match resp.json::<Value>().await {
                Ok(value) => extract_probe_usage(&value),
                Err(_) => None,
            };
            ChannelProbeOutcome {
                ok: true,
                usage,
                http_status: Some(status.as_u16()),
                error_code: None,
                error_type: None,
                error: None,
            }
        }
        Err(error) => ChannelProbeOutcome {
            ok: false,
            usage: None,
            http_status: None,
            error_code: Some("upstream_connection_failed".to_string()),
            error_type: Some("transport_error".to_string()),
            error: Some(format!("connection failed: {error}")),
        },
    }
}

fn build_probe_request(
    base: &str,
    model: &str,
    effective_type: MonoizeProviderType,
    stream: bool,
) -> (String, Value, &'static [(&'static str, &'static str)], bool) {
    match effective_type {
        MonoizeProviderType::Responses => {
            let url = format!("{base}/v1/responses");
            let body = serde_json::json!({
                "model": model,
                "max_output_tokens": 16,
                "stream": stream,
                "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
            });
            (url, body, &[][..], false)
        }
        MonoizeProviderType::ChatCompletion => {
            let url = format!("{base}/v1/chat/completions");
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 16,
                "stream": stream,
                "messages": [{"role": "user", "content": "hi"}]
            });
            (url, body, &[][..], false)
        }
        MonoizeProviderType::Messages => {
            let url = format!("{base}/v1/messages");
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 16,
                "stream": stream,
                "messages": [{"role": "user", "content": "hi"}]
            });
            (url, body, &[("anthropic-version", "2023-06-01")][..], false)
        }
        MonoizeProviderType::Gemini => {
            let method = if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            };
            let url = format!("{base}/v1beta/models/{model}:{method}");
            let body = serde_json::json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "generationConfig": {"maxOutputTokens": 16}
            });
            (url, body, &[][..], true)
        }
        MonoizeProviderType::OpenaiImage => {
            let url = format!("{base}/v1/images/generations");
            let body = serde_json::json!({
                "model": model,
                "prompt": "test",
                "size": "1024x1024",
                "n": 1,
            });
            (url, body, &[][..], false)
        }
        MonoizeProviderType::OpenaiVideo | MonoizeProviderType::FalVideo => {
            // ST-E4: video channels are never chat-probed; fail like a transport error.
            (
                String::new(),
                serde_json::json!({}),
                &[("content-type", "application/json")][..],
                false,
            )
        }
        MonoizeProviderType::Replicate => {
            // Replicate providers are excluded from active probing; this is a
            // fallback that should never be reached.
            let url = format!("{base}/v1/predictions");
            let body = serde_json::json!({
                "version": model,
                "input": {}
            });
            (url, body, &[][..], false)
        }
    }
}

fn extract_probe_usage(body: &Value) -> Option<Value> {
    if let Some(usage) = body.get("usage") {
        let prompt_tokens = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("input_tokens").and_then(Value::as_u64));
        let completion_tokens = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("output_tokens").and_then(Value::as_u64));

        if let (Some(prompt_tokens), Some(completion_tokens)) = (prompt_tokens, completion_tokens) {
            return Some(
                json!({"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens}),
            );
        }
    }

    let usage = body.get("usageMetadata")?;
    let prompt_tokens = usage
        .get("promptTokenCount")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("input_tokens").and_then(Value::as_u64));
    let completion_tokens = usage
        .get("candidatesTokenCount")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("output_tokens").and_then(Value::as_u64));

    match (prompt_tokens, completion_tokens) {
        (Some(prompt_tokens), Some(completion_tokens)) => {
            Some(json!({"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens}))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;
    use crate::migration::Migrator;
    use sea_orm::ConnectionTrait;
    use sea_orm_migration::MigratorTrait;

    #[test]
    fn entry_limit_parser_requires_a_positive_integer() {
        assert_eq!(parse_positive_entry_limit(Some("17"), 9), 17);
        assert_eq!(parse_positive_entry_limit(Some(" 3 "), 9), 3);
        assert_eq!(parse_positive_entry_limit(Some("0"), 9), 9);
        assert_eq!(parse_positive_entry_limit(Some("-1"), 9), 9);
        assert_eq!(parse_positive_entry_limit(Some("invalid"), 9), 9);
        assert_eq!(parse_positive_entry_limit(None, 9), 9);
        assert_eq!(parse_provider_reorder_limit(Some("17")), 17);
        assert_eq!(parse_provider_reorder_limit(Some("200")), 199);
        assert_eq!(parse_provider_reorder_limit(Some("0")), 199);
        assert_eq!(parse_provider_reorder_limit(Some("invalid")), 199);
        assert_eq!(
            parse_channel_affinity_cleanup_interval(Some("17")),
            Duration::from_secs(17)
        );
        for raw in ["", "0", "-1", "invalid"] {
            assert_eq!(
                parse_channel_affinity_cleanup_interval(Some(raw)),
                Duration::from_secs(DEFAULT_CHANNEL_AFFINITY_CLEANUP_INTERVAL_SECONDS)
            );
        }
        assert_eq!(
            parse_channel_affinity_cleanup_interval(None),
            Duration::from_secs(DEFAULT_CHANNEL_AFFINITY_CLEANUP_INTERVAL_SECONDS)
        );
    }

    #[test]
    fn passive_failure_threshold_is_positive_and_capped() {
        assert_eq!(effective_passive_failure_threshold_with_limit(0, 1024), 1);
        assert_eq!(effective_passive_failure_threshold_with_limit(3, 1024), 3);
        assert_eq!(
            effective_passive_failure_threshold_with_limit(2048, 1024),
            1024
        );
        assert_eq!(effective_passive_failure_threshold_with_limit(3, 0), 1);
    }

    #[test]
    fn persisted_routing_booleans_accept_only_zero_and_one() {
        assert!(!decode_database_bool("provider", "p1", "enabled", 0).unwrap());
        assert!(decode_database_bool("channel", "c1", "enabled", 1).unwrap());
        assert!(decode_database_bool("provider", "p1", "enabled", -1).is_err());
        assert!(decode_database_bool("channel", "c1", "enabled", 2).is_err());
    }

    #[test]
    fn health_capacity_fails_closed_without_scanning_or_eviction() {
        let mut health = HashMap::from([
            (
                "unhealthy".to_string(),
                ChannelHealthState {
                    healthy: false,
                    ..ChannelHealthState::new()
                },
            ),
            ("healthy".to_string(), ChannelHealthState::new()),
        ]);
        assert!(!prepare_channel_health_insert_with_limit(
            &mut health,
            "new",
            2
        ));
        assert!(health.contains_key("unhealthy"));
        assert!(health.contains_key("healthy"));
        assert!(missing_channel_health_is_saturated_with_limit(
            &health, "new", 2
        ));
        assert_eq!(health.len(), 2);
    }

    #[test]
    fn saturation_warning_rearms_only_after_the_map_drains() {
        reset_channel_health_saturation_warning();
        note_channel_health_saturation(2);
        assert!(CHANNEL_HEALTH_SATURATION_WARNED.load(Ordering::Relaxed));
        reset_channel_health_saturation_warning();
        assert!(!CHANNEL_HEALTH_SATURATION_WARNED.load(Ordering::Relaxed));
        note_channel_health_saturation(2);
        assert!(CHANNEL_HEALTH_SATURATION_WARNED.load(Ordering::Relaxed));
        reset_channel_health_saturation_warning();
    }

    #[tokio::test]
    async fn transform_id_migration_crosses_keyset_batch_boundary_and_marks_completion() {
        let db = DbPool::connect("sqlite::memory:")
            .await
            .expect("db connects");
        {
            let write = db.write().await;
            Migrator::up(&*write, None).await.expect("migrates");
        }
        let legacy_transforms = json!([{
            "transform": "openai_prompt_cache",
            "phase": "request"
        }])
        .to_string();
        let default_group_id: String = db
            .read()
            .query_one(db.stmt("SELECT id FROM monoize_groups WHERE is_default = 1", vec![]))
            .await
            .expect("default Group query succeeds")
            .expect("default Group exists")
            .try_get("", "id")
            .expect("default Group ID decodes");
        let now = Utc::now().to_rfc3339();
        let row_count = TRANSFORM_MIGRATION_BATCH_SIZE + 3;
        let mut values: Vec<SeaValue> = Vec::with_capacity(row_count * 9);
        let mut rows = Vec::with_capacity(row_count);
        for index in 0..row_count {
            let start = values.len() + 1;
            let provider_name = format!("provider {index}");
            let channel_name = format!("channel {index}");
            values.extend([
                format!("provider-{index:04}").into(),
                default_group_id.clone().into(),
                provider_name.clone().into(),
                SeaValue::Bytes(Some(Box::new(provider_name.into_bytes()))),
                legacy_transforms.clone().into(),
                now.clone().into(),
                format!("channel-{index:04}").into(),
                channel_name.clone().into(),
                SeaValue::Bytes(Some(Box::new(channel_name.into_bytes()))),
            ]);
            rows.push(format!(
                "(${start}, ${}, ${}, ${}, ${}, 0, 1, ${}, ${}, ${}, ${}, ${}, ${}, ${}, \
                 'responses', 'https://example.com', 'secret', 1)",
                start + 1,
                start + 2,
                start + 2,
                start + 3,
                start + 4,
                start + 5,
                start + 5,
                start + 6,
                start + 7,
                start + 7,
                start + 8,
            ));
        }
        db.write()
            .await
            .execute(db.stmt(
                &format!(
                    "INSERT INTO monoize_providers
                     (id, group_id, name, public_name, public_name_key, priority, enabled, transforms,
                      created_at, updated_at, channel_id, channel_name, channel_public_name,
                      channel_public_name_key, channel_provider_type, channel_base_url,
                      channel_api_key, channel_enabled) VALUES {}",
                    rows.join(", ")
                ),
                values,
            ))
            .await
            .expect("legacy providers insert");

        MonoizeRoutingStore::new(db.clone())
            .await
            .expect("store migrates transforms");

        let transformed = db
            .read()
            .query_all(db.stmt(
                "SELECT transforms FROM monoize_providers ORDER BY id ASC",
                vec![],
            ))
            .await
            .expect("transforms load");
        assert_eq!(transformed.len(), row_count);
        for row in transformed {
            let raw: String = row.try_get("", "transforms").expect("transforms decode");
            let rules: Vec<TransformRuleConfig> =
                serde_json::from_str(&raw).expect("transforms parse");
            assert_eq!(rules[0].transform, "cache_openai_prompt");
        }
        let marker = db
            .read()
            .query_one(db.stmt(
                "SELECT value FROM system_settings WHERE key = $1",
                vec![TRANSFORM_MIGRATION_MARKER.into()],
            ))
            .await
            .expect("marker loads")
            .expect("marker exists")
            .try_get::<String>("", "value")
            .expect("marker decodes");
        assert_eq!(marker, "complete");
    }

    #[tokio::test]
    async fn routing_reads_fail_closed_on_non_boolean_integer() {
        let db = DbPool::connect("sqlite::memory:")
            .await
            .expect("db connects");
        {
            let write = db.write().await;
            Migrator::up(&*write, None).await.expect("migrates");
        }
        let store = MonoizeRoutingStore::new(db.clone())
            .await
            .expect("store creates");
        let provider = store
            .create_provider(
                serde_json::from_value(json!({
                    "name": "decode contract",
                    "confirm_public_exposure": true,
                    "channel": {
                        "name": "channel",
                        "provider_type": "responses",
                        "base_url": "https://example.com",
                        "api_key": "secret",
                        "models": { "model-a": { "redirect": null, "multiplier_override": "1" } }
                    }
                }))
                .expect("provider input parses"),
            )
            .await
            .expect("provider creates");

        db.write()
            .await
            .execute(db.stmt(
                "UPDATE monoize_providers SET enabled = 2 WHERE id = $1",
                vec![provider.id.clone().into()],
            ))
            .await
            .expect("provider boolean becomes malformed");
        assert!(store.get_provider(&provider.id).await.is_err());
    }

    #[tokio::test]
    async fn available_model_names_are_sorted_and_exclude_ineligible_providers() {
        let db = DbPool::connect("sqlite::memory:")
            .await
            .expect("db connects");
        {
            let write = db.write().await;
            Migrator::up(&*write, None).await.expect("migrates");
        }
        let store = MonoizeRoutingStore::new(db.clone())
            .await
            .expect("store creates");
        let default_group_id: String = db
            .read()
            .query_one(db.stmt("SELECT id FROM monoize_groups WHERE is_default = 1", vec![]))
            .await
            .expect("default Group query succeeds")
            .expect("default Group exists")
            .try_get("", "id")
            .expect("default Group ID decodes");
        store
            .reorder_providers(ReorderProvidersInput {
                group_id: default_group_id,
                provider_ids: Vec::new(),
            })
            .await
            .expect("empty provider reorder succeeds");
        let input: CreateMonoizeProviderInput = serde_json::from_value(json!({
            "name": "visible models",
            "confirm_public_exposure": true,
            "strip_cross_protocol_nested_extra": false,
            "channel": {
                "name": "active",
                "provider_type": "responses",
                "base_url": "https://example.com",
                "api_key": "secret",
                "models": {
                    "model-z": { "redirect": null, "multiplier_override": "1" },
                    "model-a": { "redirect": null, "multiplier_override": "1" }
                }
            }
        }))
        .expect("provider input parses");
        let created = store
            .create_provider(input)
            .await
            .expect("provider creates");

        assert_eq!(
            store
                .list_available_model_names()
                .await
                .expect("names list"),
            vec!["model-a".to_string(), "model-z".to_string()]
        );
        assert_eq!(
            store
                .available_model_names(&["model-z".to_string(),])
                .await
                .expect("candidate availability loads"),
            HashSet::from(["model-z".to_string()])
        );
        let listed = store.list_providers().await.expect("providers list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].strip_cross_protocol_nested_extra, Some(false));
        assert_eq!(listed[0].channel.models.len(), 2);
        let fetched = store
            .get_provider(&created.id)
            .await
            .expect("provider loads")
            .expect("provider exists");
        assert_eq!(fetched.channel.models.len(), 2);
        assert_eq!(fetched.strip_cross_protocol_nested_extra, Some(false));
        assert_eq!(
            store
                .list_providers_for_model("model-a")
                .await
                .expect("model providers list")[0]
                .strip_cross_protocol_nested_extra,
            Some(false)
        );
        let active_probe_candidates = store
            .list_active_probe_candidates()
            .await
            .expect("active probe candidates list");
        assert_eq!(active_probe_candidates.len(), 1);
        assert_eq!(
            active_probe_candidates[0].strip_cross_protocol_nested_extra,
            Some(false)
        );
        assert_eq!(active_probe_candidates[0].channel.name, "active");

        let second_input: CreateMonoizeProviderInput = serde_json::from_value(json!({
            "name": "second",
            "confirm_public_exposure": true,
            "channel": {
                "name": "second channel",
                "provider_type": "responses",
                "base_url": "https://example.com",
                "api_key": "secret",
                "models": { "model-second": { "redirect": null, "multiplier_override": "1" } }
            }
        }))
        .expect("second provider input parses");
        let second = store
            .create_provider(second_input)
            .await
            .expect("second provider creates");
        assert_eq!(created.priority, 0);
        assert_eq!(second.priority, 1);
        store
            .reorder_providers(ReorderProvidersInput {
                group_id: created.group_id.clone(),
                provider_ids: vec![second.id.clone(), created.id.clone()],
            })
            .await
            .expect("providers reorder");
        assert_eq!(
            store
                .list_providers()
                .await
                .expect("reordered providers list")
                .into_iter()
                .map(|provider| provider.id)
                .collect::<Vec<_>>(),
            vec![second.id.clone(), created.id.clone()]
        );

        let disabled_provider: CreateMonoizeProviderInput = serde_json::from_value(json!({
            "name": "disabled provider",
            "confirm_public_exposure": true,
            "enabled": false,
            "channel": {
                "name": "active channel",
                "provider_type": "responses",
                "base_url": "https://example.com",
                "api_key": "secret",
                "models": {
                    "model-disabled-provider": { "redirect": null, "multiplier_override": "1" }
                }
            }
        }))
        .expect("disabled provider input parses");
        store
            .create_provider(disabled_provider)
            .await
            .expect("disabled provider creates");
        assert!(
            store
                .list_providers_for_model("model-disabled-provider")
                .await
                .expect("disabled provider lookup")
                .is_empty()
        );
        assert!(
            store
                .list_active_probe_candidates()
                .await
                .expect("active probe candidates reload")
                .iter()
                .all(|provider| provider.enabled && provider.channel.enabled)
        );

        db.write()
            .await
            .execute(db.stmt(
                "UPDATE monoize_providers SET extra_fields_whitelist = $1 WHERE id = $2",
                vec!["not-json".into(), created.id.clone().into()],
            ))
            .await
            .expect("corrupt whitelist writes");
        assert!(
            store
                .get_provider(&created.id)
                .await
                .expect_err("invalid whitelist must fail provider decoding")
                .contains("invalid extra_fields_whitelist JSON")
        );
    }

    #[test]
    fn probe_request_plan_routes_each_api_type() {
        let (resp_url, resp_body, resp_headers, resp_google_auth) = build_probe_request(
            "https://up.example",
            "gpt-5-mini",
            MonoizeProviderType::Responses,
            false,
        );
        assert_eq!(resp_url, "https://up.example/v1/responses");
        assert!(resp_headers.is_empty());
        assert!(!resp_google_auth);
        assert_eq!(resp_body["max_output_tokens"].as_u64(), Some(16));
        assert_eq!(resp_body["stream"].as_bool(), Some(false));
        assert!(resp_body.get("input").is_some());

        let (chat_url, chat_body, chat_headers, chat_google_auth) = build_probe_request(
            "https://up.example",
            "gpt-5-mini",
            MonoizeProviderType::ChatCompletion,
            false,
        );
        assert_eq!(chat_url, "https://up.example/v1/chat/completions");
        assert!(chat_headers.is_empty());
        assert!(!chat_google_auth);
        assert_eq!(chat_body["max_tokens"].as_u64(), Some(16));
        assert_eq!(chat_body["stream"].as_bool(), Some(false));
        assert!(chat_body.get("messages").is_some());

        let (msg_url, msg_body, msg_headers, msg_google_auth) = build_probe_request(
            "https://up.example",
            "claude-3-7-sonnet",
            MonoizeProviderType::Messages,
            false,
        );
        assert_eq!(msg_url, "https://up.example/v1/messages");
        assert_eq!(msg_headers, &[("anthropic-version", "2023-06-01")]);
        assert!(!msg_google_auth);
        assert_eq!(msg_body["max_tokens"].as_u64(), Some(16));
        assert_eq!(msg_body["stream"].as_bool(), Some(false));
        assert!(msg_body.get("messages").is_some());

        let (gem_url, gem_body, gem_headers, gem_google_auth) = build_probe_request(
            "https://up.example",
            "gemini-2.5-flash",
            MonoizeProviderType::Gemini,
            false,
        );
        assert_eq!(
            gem_url,
            "https://up.example/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert!(gem_headers.is_empty());
        assert!(gem_google_auth);
        assert_eq!(
            gem_body["generationConfig"]["maxOutputTokens"].as_u64(),
            Some(16)
        );
        assert!(gem_body.get("contents").is_some());

        let (stream_url, stream_body, _, _) = build_probe_request(
            "https://up.example",
            "gpt-5-mini",
            MonoizeProviderType::Responses,
            true,
        );
        assert_eq!(stream_url, "https://up.example/v1/responses");
        assert_eq!(stream_body["stream"].as_bool(), Some(true));

        let (gem_stream_url, _, _, _) = build_probe_request(
            "https://up.example",
            "gemini-2.5-flash",
            MonoizeProviderType::Gemini,
            true,
        );
        assert_eq!(
            gem_stream_url,
            "https://up.example/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );

        let (img_url, img_body, img_headers, img_google_auth) = build_probe_request(
            "https://up.example",
            "gpt-image-1",
            MonoizeProviderType::OpenaiImage,
            false,
        );
        assert_eq!(img_url, "https://up.example/v1/images/generations");
        assert!(img_headers.is_empty());
        assert!(!img_google_auth);
        assert_eq!(img_body["model"].as_str(), Some("gpt-image-1"));
        assert_eq!(img_body["prompt"].as_str(), Some("test"));
        assert_eq!(img_body["size"].as_str(), Some("1024x1024"));
        assert_eq!(img_body["n"].as_u64(), Some(1));
    }

    #[test]
    fn format_probe_http_error_includes_status_reason_and_body() {
        assert_eq!(
            format_probe_http_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, ""),
            "upstream returned 500 Internal Server Error"
        );
        assert_eq!(
            format_probe_http_error(
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                "upstream requests error."
            ),
            "upstream returned 503 Service Unavailable: upstream requests error."
        );
    }

    #[test]
    fn extract_probe_usage_supports_gemini_usage_metadata() {
        let usage = extract_probe_usage(&json!({
            "usageMetadata": {
                "promptTokenCount": 12,
                "candidatesTokenCount": 8
            }
        }));
        assert_eq!(
            usage,
            Some(json!({"prompt_tokens": 12, "completion_tokens": 8}))
        );
    }

    #[test]
    fn validate_api_type_overrides_rejects_empty_pattern() {
        let err = validate_api_type_overrides(&[ApiTypeOverride {
            pattern: "   ".to_string(),
            api_type: MonoizeProviderType::ChatCompletion,
        }])
        .expect_err("expected invalid empty override pattern");
        assert!(err.contains("api_type_overrides[0].pattern must not be empty"));
    }

    #[test]
    fn extra_headers_validation_accepts_valid_and_rejects_invalid() {
        let ok = BTreeMap::from([("x-session-affinity".to_string(), "ses_001".to_string())]);
        assert!(validate_channel_extra_headers("ch", &ok).is_ok());

        for (name, value) in [("Authorization", "x"), ("CONTENT-TYPE", "application/json")] {
            let reserved = BTreeMap::from([(name.to_string(), value.to_string())]);
            assert!(
                validate_channel_extra_headers("ch", &reserved).is_err(),
                "reserved header {name} must be rejected"
            );
        }

        let dup = BTreeMap::from([
            ("X-Test".to_string(), "a".to_string()),
            ("x-test".to_string(), "b".to_string()),
        ]);
        assert!(validate_channel_extra_headers("ch", &dup).is_err());

        let crlf = BTreeMap::from([("X-Ok".to_string(), "a\r\nb".to_string())]);
        assert!(validate_channel_extra_headers("ch", &crlf).is_err());

        let invalid_token = BTreeMap::from([("X Bad Header".to_string(), "v".to_string())]);
        assert!(validate_channel_extra_headers("ch", &invalid_token).is_err());

        let empty_key = BTreeMap::from([("   ".to_string(), "v".to_string())]);
        assert!(validate_channel_extra_headers("ch", &empty_key).is_err());

        let too_many: BTreeMap<String, String> = (0..EXTRA_HEADERS_MAX_ENTRIES + 1)
            .map(|index| (format!("X-H{index}"), "v".to_string()))
            .collect();
        assert!(validate_channel_extra_headers("ch", &too_many).is_err());
    }

    #[test]
    fn channel_base_url_validation_rejects_local_and_non_http_targets() {
        for base in [
            "https://127.0.0.1/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]/v1",
            "https://10.0.0.5/v1",
            "https://172.16.3.4/v1",
            "https://172.31.3.4/v1",
            "https://192.168.1.10/v1",
            "https://169.254.169.254/latest/meta-data",
            "https://100.100.100.100/v1",
            "https://[fc00::1]/v1",
            "https://[fd12:3456::1]/v1",
            "ftp://example.com",
            "file:///etc/passwd",
            "https://0.0.0.0/v1",
            "https://255.255.255.255/v1",
        ] {
            assert!(
                validate_channel_base_url(base).is_err(),
                "base_url {base} must be rejected"
            );
        }
    }

    #[test]
    fn channel_base_url_validation_accepts_public_literal_ips() {
        for base in [
            "https://8.8.8.8/v1",
            "https://1.1.1.1/v1",
            "https://151.101.1.140/v1",
            "https://[2606:4700::1111]/v1",
        ] {
            assert!(
                validate_channel_base_url(base).is_ok(),
                "base_url {base} must be accepted"
            );
        }
    }

    #[test]
    fn extra_headers_normalization_trims_keys_and_sorts_json() {
        let raw = BTreeMap::from([
            ("  Z-Last  ".to_string(), "2".to_string()),
            ("A-First".to_string(), "1".to_string()),
        ]);
        assert_eq!(
            normalized_extra_headers_json(Some(&raw)).unwrap(),
            r#"{"A-First":"1","Z-Last":"2"}"#
        );
        assert!(normalized_extra_headers_json(None).is_none());
        assert!(normalized_extra_headers_json(Some(&BTreeMap::new())).is_none());
    }

    #[test]
    fn extra_headers_decode_roundtrips_and_rejects_garbage() {
        assert!(decode_extra_headers(None).unwrap().is_none());
        assert!(
            decode_extra_headers(Some("  ".to_string()))
                .unwrap()
                .is_none()
        );
        let decoded = decode_extra_headers(Some(r#"{"X-A":"1"}"#.to_string()));
        assert!(decoded.is_ok());
        assert!(decode_extra_headers(Some("not-json".to_string())).is_err());

        let canonical = normalized_extra_headers_json(Some(&BTreeMap::from([(
            "X-Session-Affinity".to_string(),
            "ses_9".to_string(),
        )])))
        .unwrap();
        let round = decode_extra_headers(Some(canonical)).unwrap().unwrap();
        assert_eq!(
            round.get("X-Session-Affinity").map(String::as_str),
            Some("ses_9")
        );
    }
}

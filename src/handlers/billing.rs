use super::*;
use crate::billing_rate_store::DbBillingRateRecord;
#[cfg(test)]
use crate::model_registry_store::ModelPricing;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use sha2::Digest as _;
use std::str::FromStr as _;

#[derive(Debug, Clone)]
pub(super) struct BillingRateResolution {
    pub(super) pricing_profile: String,
    pub(super) pricing_model: String,
    pub(super) rates: Vec<DbBillingRateRecord>,
    /// FX snapshot captured when the rates were loaded, used to express a CNY-basis rate in
    /// the nano-USD unit every balance is denominated in. `None` when no snapshot exists.
    pub(super) cny_per_usd: Option<Decimal>,
    /// MB-R14: the once-per-request peak-window determination.
    pub(super) is_peak: bool,
}

/// MB-C1a: converts one line-item charge into nano-USD.
///
/// The conversion is applied to the charge rather than to the unit price. A unit price is a
/// nano-value per token and can be as small as a single digit, so dividing it by the exchange
/// rate before multiplying by the quantity would quantise a cheap rate into uselessness — a
/// 10 nano-CNY/token rate becomes 1 nano-USD, a 29 percent error. Multiplying first keeps the
/// dividend large, so the rounding error never exceeds half a nano-USD on the whole line.
pub(crate) fn nano_charge_to_usd(
    charge: i128,
    is_cny_basis: bool,
    cny_per_usd: Option<Decimal>,
    rate_id: &str,
) -> Result<i128, String> {
    if !is_cny_basis {
        return Ok(charge);
    }
    let fx = cny_per_usd
        .filter(|value| value.is_sign_positive() && !value.is_zero())
        .ok_or_else(|| format!("no exchange-rate snapshot for CNY-basis rate {rate_id}"))?;
    let charge = Decimal::from_i128_with_scale(charge, 0)
        .checked_div(fx)
        .ok_or_else(|| format!("CNY charge conversion overflow for rate {rate_id}"))?;
    charge
        .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointAwayFromZero)
        .to_i128()
        .ok_or_else(|| format!("CNY charge does not fit i128 for rate {rate_id}"))
}

pub(super) fn charge_in_usd(
    charge: i128,
    rate: &DbBillingRateRecord,
    cny_per_usd: Option<Decimal>,
) -> Result<i128, String> {
    nano_charge_to_usd(charge, rate.is_cny_basis(), cny_per_usd, &rate.id)
}

/// MB-C1b/MB-R14: the per-request pricing context carried beside the rate rows.
/// The FX snapshot and the peak-window determination are each made exactly once
/// per request so every line item of one request bills on the same basis.
#[derive(Debug, Clone, Copy)]
pub(super) struct BillingPricingContext {
    pub(super) cny_per_usd: Option<Decimal>,
    pub(super) is_peak: bool,
}

/// MB-R14: Beijing time (UTC+08:00, no daylight saving) Monday-Friday with
/// `09:00 <= time < 12:00` or `14:00 <= time < 18:00` is peak; everything
/// else is off-peak.
pub(super) fn is_beijing_peak_window(now_utc: chrono::DateTime<chrono::Utc>) -> bool {
    use chrono::{Datelike, FixedOffset, Timelike};
    let beijing = now_utc.with_timezone(&FixedOffset::east_opt(8 * 3600).expect("UTC+8 is valid"));
    let weekday = beijing.weekday();
    let weekday_peak = matches!(
        weekday,
        chrono::Weekday::Mon
            | chrono::Weekday::Tue
            | chrono::Weekday::Wed
            | chrono::Weekday::Thu
            | chrono::Weekday::Fri
    );
    let minute_of_day = beijing.hour() * 60 + beijing.minute();
    weekday_peak && ((540..720).contains(&minute_of_day) || (840..1080).contains(&minute_of_day))
}

/// MB-R15: the unit price the row bills at under the resolution's window.
fn effective_rate_price_nano(rate: &DbBillingRateRecord, is_peak: bool) -> Result<i128, String> {
    if is_peak && let Some(peak) = rate.peak_unit_price_nano()? {
        return Ok(peak);
    }
    rate.unit_price_nano()
}

#[derive(Debug, Clone, Default)]
pub(super) struct BillingRateResolutionSnapshot {
    reasoning_suffix_map: HashMap<String, String>,
    resolutions: HashMap<(String, String, String), Option<BillingRateResolution>>,
}

impl BillingRateResolutionSnapshot {
    pub(super) fn resolve(
        &self,
        upstream_model: &str,
        logical_model: &str,
        provider_type: ProviderType,
        pricing_profile: &str,
    ) -> Option<BillingRateResolution> {
        let provider_type = reasoning_envelope_provider_type(provider_type).to_string();
        let normalized_upstream =
            normalize_pricing_model_key(upstream_model, &self.reasoning_suffix_map);
        if let Some(resolution) = self
            .resolutions
            .get(&(
                normalized_upstream.clone(),
                provider_type.clone(),
                pricing_profile.to_string(),
            ))
            .cloned()
            .flatten()
            && billing_rate_matrix_allows_request(&resolution, &[]).is_ok_and(|complete| complete)
        {
            return Some(resolution);
        }
        let normalized_logical =
            normalize_pricing_model_key(logical_model, &self.reasoning_suffix_map);
        if normalized_logical == normalized_upstream {
            return None;
        }
        self.resolutions
            .get(&(
                normalized_logical,
                provider_type,
                pricing_profile.to_string(),
            ))
            .cloned()
            .flatten()
    }
}

#[derive(Debug, Clone)]
pub(super) struct MatrixChargeComponents {
    pub(super) token_line_items: Vec<Value>,
    pub(super) meter_line_items: Vec<Value>,
    pub(super) context_tier: Option<String>,
    pub(super) service_tier: Option<String>,
    pub(super) base_charge: i128,
    pub(super) final_charge: i128,
}

#[derive(Debug, Clone)]
#[cfg(test)]
#[allow(dead_code)]
pub(super) struct ChargeComponents {
    prompt_tokens: i128,
    completion_tokens: i128,
    cached_tokens: i128,
    cache_creation_tokens: i128,
    billed_cache_creation_tokens: i128,
    cache_creation_charge: i128,
    reasoning_tokens: i128,
    billed_uncached_prompt_tokens: i128,
    billed_cached_prompt_tokens: i128,
    billed_non_reasoning_completion_tokens: i128,
    billed_reasoning_completion_tokens: i128,
    uncached_prompt_charge: i128,
    cached_prompt_charge: i128,
    non_reasoning_completion_charge: i128,
    reasoning_completion_charge: i128,
    prompt_charge: i128,
    completion_charge: i128,
    base_charge: i128,
    final_charge: i128,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ChargeComputation {
    pub(super) charge_nano_usd: Option<i128>,
    pub(super) billing_breakdown: Option<Value>,
}

#[cfg(test)]
#[allow(dead_code)]
pub(super) fn non_negative_i128_to_u64(value: i128) -> u64 {
    if value <= 0 {
        0
    } else {
        u64::try_from(value).unwrap_or(u64::MAX)
    }
}

pub(super) fn parse_u64_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
}

pub(super) fn plan_maximum_charge_nano(
    resolution: &BillingRateResolution,
    max_input_tokens: u64,
    max_output_tokens: Option<u64>,
    server_tool_usage_classes: &[String],
    provider_multiplier: Multiplier,
) -> Option<i128> {
    if max_input_tokens == 0 || max_output_tokens == Some(0) {
        return None;
    }
    // The maximum is taken over per-rate charges already expressed in nano-USD, not over raw
    // unit prices. A CNY-basis and a USD-basis rate are not comparable before conversion, so
    // picking the larger unit price first would reserve against the wrong currency.
    let maximum_token_charge = |output: bool, tokens: u64| {
        resolution
            .rates
            .iter()
            .filter(|rate| {
                rate.enabled
                    && rate.rate_kind == "token"
                    && if output {
                        rate.usage_class == "output" || rate.usage_class == "reasoning_output"
                    } else {
                        rate.usage_class.starts_with("input_")
                            || rate.usage_class.starts_with("cache_")
                    }
            })
            .map(|rate| {
                let unit_price = effective_rate_price_nano(rate, resolution.is_peak).ok()?;
                let charge = i128::from(tokens).checked_mul(unit_price)?;
                charge_in_usd(charge, rate, resolution.cny_per_usd).ok()
            })
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max()
    };
    let mut maximum = maximum_token_charge(false, max_input_tokens)?;
    if let Some(max_output_tokens) = max_output_tokens {
        maximum = maximum.checked_add(maximum_token_charge(true, max_output_tokens)?)?;
    }
    for usage_class in server_tool_usage_classes {
        let meter_maximum = resolution
            .rates
            .iter()
            .filter(|rate| {
                rate.enabled && rate.rate_kind == "meter" && rate.usage_class == *usage_class
            })
            .map(|rate| {
                let units = rate
                    .match_json
                    .get("maximum_units")
                    .and_then(parse_u64_value)?;
                let charge = i128::from(units).checked_mul(rate.unit_price_nano().ok()?)?;
                charge_in_usd(charge, rate, resolution.cny_per_usd).ok()
            })
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max()?;
        maximum = maximum.checked_add(meter_maximum)?;
    }
    provider_multiplier.checked_scale_i128(maximum)
}

pub(super) async fn plan_maximum_for_attempts(
    state: &AppState,
    request: Option<&urp::UrpRequest>,
    attempts: &[MonoizeAttempt],
) -> Option<(i128, String)> {
    let mut maximum = 0_i128;
    let mut revision_parts = Vec::new();
    let mut found_billable = false;
    for attempt in attempts
        .iter()
        .filter(|attempt| attempt.billable_pricing_available)
    {
        found_billable = true;
        let resolution = attempt.billing_rate_resolution.as_ref()?;
        let model = state
            .model_registry_store
            .get_model_by_logical_and_provider(&attempt.logical_model, &attempt.provider_id)
            .await
            .ok()??;
        let metadata = state
            .model_registry_store
            .get_model_metadata(&model.id)
            .await
            .ok()??;
        let max_input = metadata
            .max_input_tokens
            .or(metadata.max_tokens)
            .and_then(|value| u64::try_from(value).ok())?;
        let max_output = request.and_then(|request| {
            request.max_output_tokens.or_else(|| {
                metadata
                    .max_output_tokens
                    .and_then(|value| u64::try_from(value).ok())
            })
        });
        let attempt_maximum = plan_maximum_charge_nano(
            resolution,
            max_input,
            max_output,
            &attempt.server_tool_usage_classes,
            attempt.model_multiplier,
        )?;
        maximum = maximum.max(attempt_maximum);
        revision_parts.push(format!(
            "{}:{}:{}:{}",
            attempt.provider_id,
            attempt.routing_config_revision,
            resolution.pricing_profile,
            resolution.pricing_model
        ));
        revision_parts.extend(
            resolution
                .rates
                .iter()
                .map(|rate| format!("{}:{}", rate.id, rate.updated_at.timestamp_micros())),
        );
    }
    if !found_billable || maximum <= 0 {
        return None;
    }
    revision_parts.sort();
    let digest = sha2::Sha256::digest(revision_parts.join("\n").as_bytes());
    let mut revision = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(revision, "{byte:02x}");
    }
    Some((maximum, revision))
}

pub(super) fn map_get_u64(map: &Map<String, Value>, key: &str) -> Option<u64> {
    map.get(key).and_then(parse_u64_value)
}

#[cfg(test)]
pub(super) fn calculate_charge_components(
    usage: &urp::Usage,
    pricing: &ModelPricing,
    provider_multiplier: Multiplier,
) -> Option<ChargeComponents> {
    let aggregate = usage.accounting();
    let usage = aggregate.as_ref();
    let prompt_tokens = i128::from(usage.input_tokens);
    let completion_tokens = i128::from(usage.output_tokens);
    let cached_tokens = i128::from(usage.cached_tokens().unwrap_or(0));
    let cache_creation_tokens = i128::from(
        usage
            .input_details
            .as_ref()
            .map(|d| d.cache_creation_tokens)
            .unwrap_or(0),
    );
    let reasoning_tokens = i128::from(usage.reasoning_tokens().unwrap_or(0));

    let uncached_prompt_tokens = (prompt_tokens - cached_tokens - cache_creation_tokens).max(0);
    let non_reasoning_completion_tokens = (completion_tokens - reasoning_tokens).max(0);

    let (
        billed_uncached_prompt_tokens,
        billed_cached_prompt_tokens,
        uncached_prompt_charge,
        cached_prompt_charge,
    ) = if let Some(cached_rate) = pricing.cache_read_input_cost_per_token_nano {
        let uncached_charge =
            uncached_prompt_tokens.checked_mul(pricing.input_cost_per_token_nano)?;
        let cached_charge = cached_tokens.max(0).checked_mul(cached_rate)?;
        (
            uncached_prompt_tokens,
            cached_tokens.max(0),
            uncached_charge,
            cached_charge,
        )
    } else {
        // No cache-read pricing, but cache_creation tokens still MUST be excluded from
        // the base input bucket to avoid double-billing (spec § 5 C3a).
        (
            uncached_prompt_tokens,
            0,
            uncached_prompt_tokens.checked_mul(pricing.input_cost_per_token_nano)?,
            0,
        )
    };
    let prompt_charge = uncached_prompt_charge.checked_add(cached_prompt_charge)?;

    let (billed_cache_creation_tokens, cache_creation_charge) =
        if let Some(cache_creation_rate) = pricing.cache_creation_input_cost_per_token_nano {
            let tokens = cache_creation_tokens.max(0);
            let charge = tokens.checked_mul(cache_creation_rate)?;
            (tokens, charge)
        } else {
            (0, 0)
        };

    let (
        billed_non_reasoning_completion_tokens,
        billed_reasoning_completion_tokens,
        non_reasoning_completion_charge,
        reasoning_completion_charge,
    ) = if let Some(reasoning_rate) = pricing.output_cost_per_reasoning_token_nano {
        let non_reasoning_charge =
            non_reasoning_completion_tokens.checked_mul(pricing.output_cost_per_token_nano)?;
        let reasoning_charge = reasoning_tokens.max(0).checked_mul(reasoning_rate)?;
        (
            non_reasoning_completion_tokens,
            reasoning_tokens.max(0),
            non_reasoning_charge,
            reasoning_charge,
        )
    } else {
        (
            completion_tokens.max(0),
            0,
            completion_tokens.checked_mul(pricing.output_cost_per_token_nano)?,
            0,
        )
    };
    let completion_charge =
        non_reasoning_completion_charge.checked_add(reasoning_completion_charge)?;

    let base_charge = prompt_charge
        .checked_add(completion_charge)?
        .checked_add(cache_creation_charge)?;
    let final_charge = scale_charge_with_multiplier(base_charge, provider_multiplier)?;

    Some(ChargeComponents {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_creation_tokens,
        billed_cache_creation_tokens,
        cache_creation_charge,
        reasoning_tokens,
        billed_uncached_prompt_tokens,
        billed_cached_prompt_tokens,
        billed_non_reasoning_completion_tokens,
        billed_reasoning_completion_tokens,
        uncached_prompt_charge,
        cached_prompt_charge,
        non_reasoning_completion_charge,
        reasoning_completion_charge,
        prompt_charge,
        completion_charge,
        base_charge,
        final_charge,
    })
}

#[cfg(test)]
pub(super) fn calculate_charge_nano(
    usage: &urp::Usage,
    pricing: &ModelPricing,
    provider_multiplier: Multiplier,
) -> Option<i128> {
    calculate_charge_components(usage, pricing, provider_multiplier).map(|parts| parts.final_charge)
}

pub(super) fn build_usage_breakdown(usage: &urp::Usage) -> Value {
    let aggregate = usage.accounting();
    let usage = aggregate.as_ref();
    let input_details = usage.input_details.as_ref();
    let output_details = usage.output_details.as_ref();

    let input_cached = input_details
        .map(|d| d.cache_read_tokens)
        .filter(|&v| v > 0)
        .or_else(|| {
            usage
                .extra_body
                .get("cache_read_input_tokens")
                .and_then(parse_u64_value)
        })
        .or_else(|| {
            usage
                .extra_body
                .get("input_tokens_details")
                .and_then(|v| v.as_object())
                .and_then(|d| map_get_u64(d, "cached_tokens"))
        })
        .or_else(|| {
            usage
                .extra_body
                .get("prompt_tokens_details")
                .and_then(|v| v.as_object())
                .and_then(|d| map_get_u64(d, "cached_tokens"))
        });
    let input_cache_creation = input_details
        .map(|d| d.cache_creation_tokens)
        .filter(|&v| v > 0)
        .or_else(|| {
            usage
                .extra_body
                .get("cache_creation_input_tokens")
                .and_then(parse_u64_value)
        });
    let input_cache_creation_5m = input_details
        .map(|d| d.cache_creation_5m_tokens)
        .filter(|&v| v > 0);
    let input_cache_creation_1h = input_details
        .map(|d| d.cache_creation_1h_tokens)
        .filter(|&v| v > 0);
    let input_text = input_details
        .and_then(|d| d.modality_breakdown.as_ref())
        .and_then(|m| m.text_tokens);
    let input_cached_text = input_details
        .and_then(|d| d.cache_read_modality_breakdown.as_ref())
        .and_then(|m| m.text_tokens);
    let input_audio = input_details
        .and_then(|d| d.modality_breakdown.as_ref())
        .and_then(|m| m.audio_tokens);
    let input_cached_audio = input_details
        .and_then(|d| d.cache_read_modality_breakdown.as_ref())
        .and_then(|m| m.audio_tokens);
    let input_image = input_details
        .and_then(|d| d.modality_breakdown.as_ref())
        .and_then(|m| m.image_tokens);
    let input_cached_image = input_details
        .and_then(|d| d.cache_read_modality_breakdown.as_ref())
        .and_then(|m| m.image_tokens);
    let output_reasoning = output_details
        .map(|d| d.reasoning_tokens)
        .filter(|&v| v > 0)
        .or_else(|| {
            usage
                .extra_body
                .get("output_tokens_details")
                .and_then(|v| v.as_object())
                .and_then(|d| map_get_u64(d, "reasoning_tokens"))
        })
        .or_else(|| {
            usage
                .extra_body
                .get("completion_tokens_details")
                .and_then(|v| v.as_object())
                .and_then(|d| map_get_u64(d, "reasoning_tokens"))
        });
    let output_text = output_details
        .and_then(|d| d.modality_breakdown.as_ref())
        .and_then(|m| m.text_tokens);
    let output_audio = output_details
        .and_then(|d| d.modality_breakdown.as_ref())
        .and_then(|m| m.audio_tokens);
    let output_image = output_details
        .and_then(|d| d.modality_breakdown.as_ref())
        .and_then(|m| m.image_tokens);

    json!({
        "version": 1,
        "input": {
            "total_tokens": usage.input_tokens,
            "uncached_tokens": usage.input_tokens
                .saturating_sub(input_cached.unwrap_or(0))
                .saturating_sub(input_cache_creation.unwrap_or(0)),
            "text_tokens": input_text,
            "cached_text_tokens": input_cached_text,
            "cached_tokens": input_cached,
            "cache_creation_tokens": input_cache_creation,
            "cache_creation_5m_tokens": input_cache_creation_5m,
            "cache_creation_1h_tokens": input_cache_creation_1h,
            "audio_tokens": input_audio,
            "cached_audio_tokens": input_cached_audio,
            "image_tokens": input_image,
            "cached_image_tokens": input_cached_image
        },
        "output": {
            "total_tokens": usage.output_tokens,
            "non_reasoning_tokens": usage.output_tokens.saturating_sub(output_reasoning.unwrap_or(0)),
            "text_tokens": output_text,
            "reasoning_tokens": output_reasoning,
            "audio_tokens": output_audio,
            "image_tokens": output_image
        }
        // RL15c: the upstream usage echo must not be persisted. Attribution maps inside
        // `extra_body` grow with conversation length and once averaged 24 KB per row.
    })
}

#[cfg(test)]
#[allow(dead_code)]
pub(super) fn build_billing_breakdown(
    logical_model: &str,
    attempt: &MonoizeAttempt,
    pricing: &ModelPricing,
    components: &ChargeComponents,
) -> Value {
    json!({
        "version": 1,
        "currency": "nano_usd",
        "logical_model": logical_model,
        "upstream_model": attempt.upstream_model,
        "provider_id": attempt.provider_id,
        "provider_multiplier": attempt.model_multiplier,
        "input": {
            "total_tokens": non_negative_i128_to_u64(components.prompt_tokens),
            "cached_tokens": non_negative_i128_to_u64(components.cached_tokens),
            "billed_uncached_tokens": non_negative_i128_to_u64(components.billed_uncached_prompt_tokens),
            "billed_cached_tokens": non_negative_i128_to_u64(components.billed_cached_prompt_tokens),
            "unit_price_nano": pricing.input_cost_per_token_nano.to_string(),
            "cached_unit_price_nano": pricing.cache_read_input_cost_per_token_nano.map(|v| v.to_string()),
            "uncached_charge_nano": components.uncached_prompt_charge.to_string(),
            "cached_charge_nano": components.cached_prompt_charge.to_string(),
            "cache_creation_tokens": non_negative_i128_to_u64(components.cache_creation_tokens),
            "billed_cache_creation_tokens": non_negative_i128_to_u64(components.billed_cache_creation_tokens),
            "cache_creation_unit_price_nano": pricing.cache_creation_input_cost_per_token_nano.map(|v| v.to_string()),
            "cache_creation_charge_nano": components.cache_creation_charge.to_string(),
            "total_charge_nano": components.prompt_charge.to_string(),
        },
        "output": {
            "total_tokens": non_negative_i128_to_u64(components.completion_tokens),
            "reasoning_tokens": non_negative_i128_to_u64(components.reasoning_tokens),
            "billed_non_reasoning_tokens": non_negative_i128_to_u64(components.billed_non_reasoning_completion_tokens),
            "billed_reasoning_tokens": non_negative_i128_to_u64(components.billed_reasoning_completion_tokens),
            "unit_price_nano": pricing.output_cost_per_token_nano.to_string(),
            "reasoning_unit_price_nano": pricing.output_cost_per_reasoning_token_nano.map(|v| v.to_string()),
            "non_reasoning_charge_nano": components.non_reasoning_completion_charge.to_string(),
            "reasoning_charge_nano": components.reasoning_completion_charge.to_string(),
            "total_charge_nano": components.completion_charge.to_string(),
        },
        "base_charge_nano": components.base_charge.to_string(),
        "final_charge_nano": components.final_charge.to_string(),
    })
}

pub(super) async fn resolve_billing_rate_matrix(
    state: &AppState,
    upstream_model: &str,
    logical_model: &str,
    provider_type: ProviderType,
    pricing_profile: &str,
) -> AppResult<Option<BillingRateResolution>> {
    let snapshot = build_billing_rate_resolution_snapshot(
        state,
        &[(
            upstream_model.to_string(),
            provider_type,
            pricing_profile.to_string(),
        )],
        logical_model,
    )
    .await?;
    Ok(snapshot.resolve(
        upstream_model,
        logical_model,
        provider_type,
        pricing_profile,
    ))
}

pub(super) async fn build_billing_rate_resolution_snapshot(
    state: &AppState,
    attempts: &[(String, ProviderType, String)],
    logical_model: &str,
) -> AppResult<BillingRateResolutionSnapshot> {
    let reasoning_suffix_map = {
        let runtime = state.monoize_runtime.read().await;
        runtime.reasoning_suffix_map.clone()
    };

    let normalized_logical = normalize_pricing_model_key(logical_model, &reasoning_suffix_map);
    let mut pairs = std::collections::HashSet::new();
    for (upstream_model, provider_type, pricing_profile) in attempts {
        let provider_type = reasoning_envelope_provider_type(*provider_type).to_string();
        pairs.insert((
            normalize_pricing_model_key(upstream_model, &reasoning_suffix_map),
            provider_type.clone(),
            pricing_profile.clone(),
        ));
        pairs.insert((
            normalized_logical.clone(),
            provider_type,
            pricing_profile.clone(),
        ));
    }
    let mut candidate_profiles = attempts
        .iter()
        .map(|(_, _, profile)| profile.clone())
        .collect::<Vec<_>>();
    candidate_profiles.sort();
    candidate_profiles.dedup();
    let mut provider_types = pairs
        .iter()
        .map(|(_, provider_type, _)| provider_type.clone())
        .collect::<Vec<_>>();
    provider_types.sort();
    provider_types.dedup();
    let candidate_rates = state
        .billing_rate_store
        .list_candidate_rates_for_profiles_and_provider_types(&candidate_profiles, &provider_types)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;
    // Captured once per request so every line item of one request bills at the same rate.
    let cny_per_usd = state
        .exchange_rate_service
        .current()
        .await
        .ok()
        .and_then(|snapshot| Decimal::from_str(&snapshot.cny_per_usd).ok())
        .filter(|rate| rate.is_sign_positive() && !rate.is_zero());
    // MB-R14: determined once per request, at the same moment as the FX snapshot.
    let pricing = BillingPricingContext {
        cny_per_usd,
        is_peak: is_beijing_peak_window(chrono::Utc::now()),
    };
    let resolutions = pairs
        .into_iter()
        .map(|(model, provider_type, pricing_profile)| {
            let resolution = resolve_billing_rate_matrix_from_snapshot(
                &candidate_rates,
                &model,
                &provider_type,
                &pricing_profile,
                &pricing,
            );
            ((model, provider_type, pricing_profile), resolution)
        })
        .collect();
    Ok(BillingRateResolutionSnapshot {
        reasoning_suffix_map,
        resolutions,
    })
}

fn resolve_billing_rate_matrix_from_snapshot(
    candidate_rates: &[DbBillingRateRecord],
    model: &str,
    provider_type: &str,
    pricing_profile: &str,
    pricing: &BillingPricingContext,
) -> Option<BillingRateResolution> {
    let profile_rates = candidate_rates
        .iter()
        .filter(|rate| {
            rate.pricing_profile == pricing_profile
                && rate
                    .provider_type
                    .as_deref()
                    .is_none_or(|value| value == provider_type)
                && rate
                    .model_pattern
                    .as_deref()
                    .is_none_or(|pattern| crate::billing_rate_store::glob_matches(pattern, model))
        })
        .cloned()
        .collect::<Vec<_>>();
    (!profile_rates.is_empty()).then(|| BillingRateResolution {
        pricing_profile: pricing_profile.to_string(),
        pricing_model: model.to_string(),
        rates: profile_rates,
        cny_per_usd: pricing.cny_per_usd,
        is_peak: pricing.is_peak,
    })
}

pub(super) fn billing_rate_matrix_allows_request(
    resolution: &BillingRateResolution,
    server_tool_usage_classes: &[String],
) -> Result<bool, String> {
    for rate in &resolution.rates {
        let unit_price = rate.unit_price_nano()?;
        if unit_price < 0 || unit_price.to_string() != rate.unit_price_nano {
            return Err(format!(
                "non-canonical or negative unit_price_nano for billing rate {}",
                rate.id
            ));
        }
        // MB-R15: a malformed peak price makes the matrix incomplete exactly like a
        // malformed unit_price_nano does.
        if let Some(peak) = rate.peak_unit_price_nano()?
            && peak < 0
        {
            return Err(format!(
                "negative peak_unit_price_nano for billing rate {}",
                rate.id
            ));
        }
    }
    // MB-R9a: a CNY-basis rate is unsettleable without an FX snapshot. Reporting the matrix
    // incomplete excludes the mapping before dispatch, so the request fails with
    // `model_pricing_required` instead of failing after upstream bytes were delivered.
    if resolution.cny_per_usd.is_none() && resolution.rates.iter().any(|rate| rate.is_cny_basis()) {
        return Ok(false);
    }
    let context_tiers: std::collections::BTreeSet<String> = resolution
        .rates
        .iter()
        .filter_map(|r| r.context_tier.as_deref())
        .filter(|tier| *tier != "default")
        .map(str::to_string)
        .collect();
    let has_dimensionless_fallback = |usage_class: &str, context_tier: Option<&str>| {
        resolution.rates.iter().any(|rate| {
            rate.rate_kind == "token"
                && rate.usage_class == usage_class
                && rate.modality.is_none()
                && rate.cache_ttl.is_none()
                && rate
                    .service_tier
                    .as_deref()
                    .is_none_or(|tier| tier == "default")
                && match context_tier {
                    Some(tier) => rate.context_tier.as_deref() == Some(tier),
                    None => rate
                        .context_tier
                        .as_deref()
                        .is_none_or(|tier| tier == "default"),
                }
        })
    };
    if context_tiers.is_empty()
        && (!has_dimensionless_fallback("input_uncached", None)
            || !has_dimensionless_fallback("output", None))
    {
        return Ok(false);
    }
    if !context_tiers.is_empty() {
        let has_threshold = resolution
            .rates
            .iter()
            .filter_map(|r| r.match_json.get("context_threshold_tokens"))
            .any(|value| parse_u64_value(value).is_some());
        if !has_threshold {
            return Err("context-tier rate requires context_threshold_tokens".to_string());
        }
        for tier in &context_tiers {
            for usage_class in ["input_uncached", "output"] {
                let has_tier_rate = has_dimensionless_fallback(usage_class, Some(tier.as_str()));
                if !has_tier_rate {
                    return Err(format!(
                        "missing token rate for usage_class={usage_class}, context_tier={tier}"
                    ));
                }
            }
        }
    }
    // MB-M5: a missing meter rate for a server-native tool usage class settles at zero
    // rather than rejecting. The usage classes therefore do not participate in matrix
    // completeness, and this function ignores `server_tool_usage_classes` entirely.
    let _ = server_tool_usage_classes;
    Ok(true)
}

pub(crate) fn billing_rates_form_complete_matrix(rates: &[DbBillingRateRecord]) -> bool {
    let resolution = BillingRateResolution {
        pricing_profile: String::new(),
        pricing_model: String::new(),
        rates: rates.to_vec(),
        // This helper answers a structural question — whether the usage-class and tier matrix
        // is complete — not whether the matrix can be settled right now. A placeholder rate
        // keeps the MB-R9a gate from reporting a correctly configured CNY matrix as
        // incomplete just because the FX snapshot is not in scope here.
        cny_per_usd: Some(Decimal::ONE),
        is_peak: false,
    };
    billing_rate_matrix_allows_request(&resolution, &[]).is_ok_and(|complete| complete)
}

fn determine_context_tier(
    usage: &urp::Usage,
    rates: &[DbBillingRateRecord],
) -> Result<Option<String>, String> {
    if let Some(tier) = usage
        .extra_body
        .get("context_tier")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(tier.to_string()));
    }

    let has_context_tiers = rates
        .iter()
        .any(|r| r.context_tier.as_deref().is_some_and(|v| v != "default"));
    if !has_context_tiers {
        return Ok(None);
    }

    let threshold = rates
        .iter()
        .filter_map(|r| r.match_json.get("context_threshold_tokens"))
        .find_map(parse_u64_value)
        .ok_or_else(|| "context-tier rate requires context_threshold_tokens".to_string())?;
    if usage.input_tokens > threshold {
        Ok(Some("long".to_string()))
    } else {
        Ok(Some("short".to_string()))
    }
}

fn rate_matches_dimension(
    rate: &DbBillingRateRecord,
    modality: Option<&str>,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    cache_ttl: Option<&str>,
) -> bool {
    if let Some(rate_modality) = rate.modality.as_deref()
        && Some(rate_modality) != modality
    {
        return false;
    }
    if rate.modality.is_none() && modality.is_some() {
        return false;
    }
    if let Some(rate_context_tier) = rate.context_tier.as_deref()
        && Some(rate_context_tier) != context_tier
        && rate_context_tier != "default"
    {
        return false;
    }
    match service_tier {
        Some(service_tier) if service_tier != "default" => {
            if rate.service_tier.as_deref() != Some(service_tier) {
                return false;
            }
        }
        None | Some("default") => {
            if rate
                .service_tier
                .as_deref()
                .is_some_and(|tier| tier != "default")
            {
                return false;
            }
        }
        Some(_) => unreachable!(),
    }
    if let Some(rate_cache_ttl) = rate.cache_ttl.as_deref()
        && Some(rate_cache_ttl) != cache_ttl
    {
        return false;
    }
    true
}

fn find_rate<'a>(
    rates: &'a [DbBillingRateRecord],
    rate_kind: &str,
    usage_class: &str,
    modality: Option<&str>,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    cache_ttl: Option<&str>,
) -> Option<&'a DbBillingRateRecord> {
    rates.iter().find(|rate| {
        rate.rate_kind == rate_kind
            && rate.usage_class == usage_class
            && rate_matches_dimension(rate, modality, context_tier, service_tier, cache_ttl)
    })
}

fn find_rate_for_usage_classes<'a>(
    rates: &'a [DbBillingRateRecord],
    rate_kind: &str,
    usage_classes: &[&str],
    modality: Option<&str>,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    cache_ttl: Option<&str>,
) -> Option<&'a DbBillingRateRecord> {
    usage_classes.iter().find_map(|usage_class| {
        find_rate(
            rates,
            rate_kind,
            usage_class,
            modality,
            context_tier,
            service_tier,
            cache_ttl,
        )
    })
}

fn has_matching_modality_rates(
    rates: &[DbBillingRateRecord],
    usage_classes: &[&str],
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    cache_ttl: Option<&str>,
) -> bool {
    rates.iter().any(|rate| {
        rate.rate_kind == "token"
            && usage_classes.contains(&rate.usage_class.as_str())
            && rate.modality.is_some()
            && rate_matches_dimension(
                rate,
                rate.modality.as_deref(),
                context_tier,
                service_tier,
                cache_ttl,
            )
    })
}

fn modality_quantity_sum(breakdown: &urp::ModalityBreakdown) -> u64 {
    breakdown
        .text_tokens
        .unwrap_or(0)
        .saturating_add(breakdown.image_tokens.unwrap_or(0))
        .saturating_add(breakdown.audio_tokens.unwrap_or(0))
        .saturating_add(breakdown.video_tokens.unwrap_or(0))
        .saturating_add(breakdown.document_tokens.unwrap_or(0))
}

fn validate_modality_sum(
    usage_class: &str,
    breakdown: &urp::ModalityBreakdown,
    expected: u64,
) -> Result<(), String> {
    let actual = modality_quantity_sum(breakdown);
    if actual != expected {
        return Err(format!(
            "modality-specific rate for {usage_class} requires modality quantities to sum to billed quantity"
        ));
    }
    Ok(())
}

fn subtract_optional_modality(
    total: Option<u64>,
    subtract: Option<u64>,
    usage_class: &str,
) -> Result<Option<u64>, String> {
    match (total, subtract) {
        (Some(total), Some(subtract)) if subtract <= total => Ok(Some(total - subtract)),
        (Some(total), None) => Ok(Some(total)),
        (None, Some(0)) => Ok(None),
        (None, None) => Ok(None),
        _ => Err(format!(
            "modality-specific rate for {usage_class} requires compatible cache-read modality quantities"
        )),
    }
}

fn subtract_modality_breakdown(
    total: &urp::ModalityBreakdown,
    subtract: &urp::ModalityBreakdown,
    usage_class: &str,
) -> Result<urp::ModalityBreakdown, String> {
    Ok(urp::ModalityBreakdown {
        text_tokens: subtract_optional_modality(
            total.text_tokens,
            subtract.text_tokens,
            usage_class,
        )?,
        image_tokens: subtract_optional_modality(
            total.image_tokens,
            subtract.image_tokens,
            usage_class,
        )?,
        audio_tokens: subtract_optional_modality(
            total.audio_tokens,
            subtract.audio_tokens,
            usage_class,
        )?,
        video_tokens: subtract_optional_modality(
            total.video_tokens,
            subtract.video_tokens,
            usage_class,
        )?,
        document_tokens: subtract_optional_modality(
            total.document_tokens,
            subtract.document_tokens,
            usage_class,
        )?,
    })
}

fn input_uncached_modality_breakdown(
    details: Option<&urp::InputDetails>,
    uncached_tokens: u64,
) -> Result<Option<urp::ModalityBreakdown>, String> {
    let Some(details) = details else {
        return Ok(None);
    };
    let Some(total_breakdown) = details.modality_breakdown.as_ref() else {
        return Ok(None);
    };
    if details.cache_creation_tokens > 0 {
        return Err(
            "modality-specific input rate requires cache-creation modality quantities".to_string(),
        );
    }
    if details.cache_read_tokens == 0 {
        validate_modality_sum("input_uncached", total_breakdown, uncached_tokens)?;
        return Ok(Some(total_breakdown.clone()));
    }
    let cached_breakdown = details
        .cache_read_modality_breakdown
        .as_ref()
        .ok_or_else(|| {
            "modality-specific input rate requires cache-read modality quantities".to_string()
        })?;
    validate_modality_sum("cache_read", cached_breakdown, details.cache_read_tokens)?;
    let uncached_breakdown =
        subtract_modality_breakdown(total_breakdown, cached_breakdown, "input_uncached")?;
    validate_modality_sum("input_uncached", &uncached_breakdown, uncached_tokens)?;
    Ok(Some(uncached_breakdown))
}

fn add_token_line(
    line_items: &mut Vec<Value>,
    rates: &[DbBillingRateRecord],
    usage_class: &str,
    quantity: u64,
    modality: Option<&str>,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    cache_ttl: Option<&str>,
    pricing: &BillingPricingContext,
) -> Result<i128, String> {
    add_token_line_for_usage_classes(
        line_items,
        rates,
        &[usage_class],
        quantity,
        modality,
        context_tier,
        service_tier,
        cache_ttl,
        &pricing,
    )
}

fn add_token_line_for_usage_classes(
    line_items: &mut Vec<Value>,
    rates: &[DbBillingRateRecord],
    usage_classes: &[&str],
    quantity: u64,
    modality: Option<&str>,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    cache_ttl: Option<&str>,
    pricing: &BillingPricingContext,
) -> Result<i128, String> {
    if quantity == 0 {
        return Ok(0);
    }
    let rate = find_rate_for_usage_classes(
        rates,
        "token",
        usage_classes,
        modality,
        context_tier,
        service_tier,
        cache_ttl,
    )
    .ok_or_else(|| {
        format!(
            "missing token rate for usage_class={}, modality={:?}, context_tier={:?}, service_tier={:?}, cache_ttl={:?}",
            usage_classes.join("|"), modality, context_tier, service_tier, cache_ttl
        )
    })?;
    let unit_price = effective_rate_price_nano(rate, pricing.is_peak)?;
    let pricing_window = if pricing.is_peak && rate.peak_unit_price_nano().ok().flatten().is_some()
    {
        "peak"
    } else {
        "off_peak"
    };
    let charge = i128::from(quantity)
        .checked_mul(unit_price)
        .ok_or_else(|| "token charge overflow".to_string())?;
    let charge = charge_in_usd(charge, rate, pricing.cny_per_usd)?;
    line_items.push(json!({
        "rate_id": rate.id,
        "usage_class": rate.usage_class,
        "unit": rate.unit,
        "unit_price_nano": unit_price.to_string(),
        "unit_price_currency": rate.unit_price_currency,
        "pricing_window": pricing_window,
        "quantity": quantity,
        "charge_nano": charge.to_string(),
        "modality": modality,
        "context_tier": context_tier,
        "service_tier": service_tier,
        "cache_ttl": cache_ttl,
    }));
    Ok(charge)
}

fn add_modality_token_lines(
    line_items: &mut Vec<Value>,
    rates: &[DbBillingRateRecord],
    usage_classes: &[&str],
    breakdown: Option<&urp::ModalityBreakdown>,
    fallback_quantity: u64,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    pricing: &BillingPricingContext,
) -> Result<i128, String> {
    if !has_matching_modality_rates(rates, usage_classes, context_tier, service_tier, None) {
        return add_token_line_for_usage_classes(
            line_items,
            rates,
            usage_classes,
            fallback_quantity,
            None,
            context_tier,
            service_tier,
            None,
            &pricing,
        );
    }
    // Zero tokens need no modality breakdown — charge is 0 regardless of rates.
    if fallback_quantity == 0 && breakdown.is_none() {
        return Ok(0);
    }
    let Some(breakdown) = breakdown else {
        return add_token_line_for_usage_classes(
            line_items,
            rates,
            usage_classes,
            fallback_quantity,
            None,
            context_tier,
            service_tier,
            None,
            &pricing,
        );
    };
    validate_modality_sum(usage_classes[0], breakdown, fallback_quantity)?;
    let mut total = 0i128;
    for (modality, quantity) in [
        ("text", breakdown.text_tokens),
        ("image", breakdown.image_tokens),
        ("audio", breakdown.audio_tokens),
        ("video", breakdown.video_tokens),
        ("document", breakdown.document_tokens),
    ] {
        total = total
            .checked_add(add_token_line_for_usage_classes(
                line_items,
                rates,
                usage_classes,
                quantity.unwrap_or(0),
                Some(modality),
                context_tier,
                service_tier,
                None,
                &pricing,
            )?)
            .ok_or_else(|| "token charge overflow".to_string())?;
    }
    Ok(total)
}

fn add_cache_read_lines(
    line_items: &mut Vec<Value>,
    rates: &[DbBillingRateRecord],
    breakdown: Option<&urp::ModalityBreakdown>,
    quantity: u64,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    pricing: &BillingPricingContext,
) -> Result<i128, String> {
    let usage_classes = ["cache_read", "input_cached"];
    if breakdown.is_some()
        && has_matching_modality_rates(rates, &usage_classes, context_tier, service_tier, None)
    {
        return add_modality_token_lines(
            line_items,
            rates,
            &usage_classes,
            breakdown,
            quantity,
            context_tier,
            service_tier,
            &pricing,
        );
    }
    if find_rate_for_usage_classes(
        rates,
        "token",
        &usage_classes,
        None,
        context_tier,
        service_tier,
        None,
    )
    .is_some()
    {
        return add_token_line_for_usage_classes(
            line_items,
            rates,
            &usage_classes,
            quantity,
            None,
            context_tier,
            service_tier,
            None,
            &pricing,
        );
    }
    add_token_line(
        line_items,
        rates,
        "input_uncached",
        quantity,
        None,
        context_tier,
        service_tier,
        None,
        &pricing,
    )
}

fn add_cache_write_line(
    line_items: &mut Vec<Value>,
    rates: &[DbBillingRateRecord],
    usage_class: &str,
    quantity: u64,
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    cache_ttl: &str,
    pricing: &BillingPricingContext,
) -> Result<i128, String> {
    if find_rate(
        rates,
        "token",
        usage_class,
        None,
        context_tier,
        service_tier,
        Some(cache_ttl),
    )
    .is_some()
    {
        return add_token_line(
            line_items,
            rates,
            usage_class,
            quantity,
            None,
            context_tier,
            service_tier,
            Some(cache_ttl),
            &pricing,
        );
    }
    add_token_line(
        line_items,
        rates,
        "input_uncached",
        quantity,
        None,
        context_tier,
        service_tier,
        None,
        pricing,
    )
}

fn authoritative_meter_quantity(usage: &urp::Usage, usage_class: &str, unit: &str) -> Option<u64> {
    let direct_keys = [
        usage_class.to_string(),
        format!("{usage_class}_requests"),
        format!("{usage_class}_calls"),
        format!("{usage_class}_billed_minutes"),
        format!("{usage_class}_minutes"),
    ];
    for key in &direct_keys {
        if let Some(value) = usage.extra_body.get(key).and_then(parse_u64_value) {
            return Some(value);
        }
    }
    if let Some(obj) = usage
        .extra_body
        .get("server_tool_use")
        .and_then(Value::as_object)
    {
        let key = match usage_class {
            "web_search" => "web_search_requests",
            "code_execution_duration" if unit == "billed_minute" => "code_execution_billed_minutes",
            "code_execution" => "code_execution_requests",
            _ => usage_class,
        };
        if let Some(value) = obj.get(key).and_then(parse_u64_value) {
            return Some(value);
        }
    }
    if let Some(obj) = usage
        .extra_body
        .get("server_side_tool_usage")
        .and_then(Value::as_object)
    {
        for key in [
            usage_class,
            &format!("{usage_class}_calls"),
            &format!("{usage_class}_requests"),
        ] {
            if let Some(value) = obj.get(key).and_then(parse_u64_value) {
                return Some(value);
            }
        }
    }
    None
}

fn decoded_provider_item_count(output: Option<&[urp::Node]>, usage_class: &str) -> u64 {
    let Some(output) = output else {
        return 0;
    };
    output
        .iter()
        .filter(|node| match node {
            urp::Node::ProviderItem { item_type, .. } => match usage_class {
                "web_search" => item_type.contains("web_search"),
                "file_search_tool_call" => item_type.contains("file_search"),
                "x_search" => item_type.contains("x_search"),
                "code_execution" | "code_execution_duration" | "code_interpreter_duration" => {
                    item_type.contains("code")
                }
                _ => false,
            },
            _ => false,
        })
        .count() as u64
}

fn add_meter_lines(
    line_items: &mut Vec<Value>,
    rates: &[DbBillingRateRecord],
    usage: &urp::Usage,
    output: Option<&[urp::Node]>,
    requested_usage_classes: &[String],
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    pricing: &BillingPricingContext,
) -> Result<i128, String> {
    let mut total = 0i128;
    let mut selected_usage_classes = HashSet::new();
    for rate in rates.iter().filter(|rate| {
        rate.rate_kind == "meter"
            && rate_matches_dimension(rate, None, context_tier, service_tier, None)
    }) {
        if !selected_usage_classes.insert(rate.usage_class.as_str()) {
            continue;
        }
        let authoritative = authoritative_meter_quantity(usage, &rate.usage_class, &rate.unit);
        if requested_usage_classes
            .iter()
            .any(|usage_class| usage_class == &rate.usage_class)
            && rate
                .match_json
                .get("requires_authoritative_usage")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            && authoritative.is_none()
        {
            return Err(format!(
                "authoritative usage required for meter usage_class={}",
                rate.usage_class
            ));
        }
        let mut quantity = authoritative.unwrap_or_else(|| {
            if rate.unit == "call" {
                decoded_provider_item_count(output, &rate.usage_class)
            } else {
                0
            }
        });
        if quantity == 0 {
            continue;
        }
        if rate
            .match_json
            .get("requires_authoritative_usage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && authoritative.is_none()
        {
            return Err(format!(
                "authoritative usage required for meter usage_class={}",
                rate.usage_class
            ));
        }
        if let Some(minimum) = rate
            .match_json
            .get("minimum_units")
            .and_then(parse_u64_value)
        {
            quantity = quantity.max(minimum);
        }
        let unit_price = effective_rate_price_nano(rate, pricing.is_peak)?;
        let pricing_window =
            if pricing.is_peak && rate.peak_unit_price_nano().ok().flatten().is_some() {
                "peak"
            } else {
                "off_peak"
            };
        let charge = i128::from(quantity)
            .checked_mul(unit_price)
            .ok_or_else(|| "meter charge overflow".to_string())?;
        let charge = charge_in_usd(charge, rate, pricing.cny_per_usd)?;
        line_items.push(json!({
            "rate_id": rate.id,
            "usage_class": rate.usage_class,
            "unit": rate.unit,
            "unit_price_nano": unit_price.to_string(),
            "unit_price_currency": rate.unit_price_currency,
            "pricing_window": pricing_window,
            "quantity": quantity,
            "charge_nano": charge.to_string(),
            "authoritative": authoritative.is_some(),
            "context_tier": context_tier,
            "service_tier": service_tier,
        }));
        total = total
            .checked_add(charge)
            .ok_or_else(|| "meter charge overflow".to_string())?;
    }
    // MB-M5 / MB-M5a: a requested server-tool usage class with no eligible rate settles at
    // zero. Record the observed quantity and mark the line `unpriced` so the request log
    // still carries the unbilled volume; MB-M5b keeps this distinct from a rate priced at 0.
    for usage_class in requested_usage_classes {
        if selected_usage_classes.contains(usage_class.as_str()) {
            continue;
        }
        let quantity = authoritative_meter_quantity(usage, usage_class, "call")
            .unwrap_or_else(|| decoded_provider_item_count(output, usage_class));
        if quantity == 0 {
            continue;
        }
        tracing::warn!(
            usage_class = %usage_class,
            quantity,
            "no meter rate configured for server-native tool usage class; settling at zero"
        );
        line_items.push(json!({
            "rate_id": Value::Null,
            "usage_class": usage_class,
            "unit": "call",
            "unit_price_nano": "0",
            "unit_price_currency": "USD",
            "pricing_window": if pricing.is_peak { "peak" } else { "off_peak" },
            "quantity": quantity,
            "charge_nano": "0",
            "authoritative": authoritative_meter_quantity(usage, usage_class, "call").is_some(),
            "context_tier": context_tier,
            "service_tier": service_tier,
            "unpriced": true,
        }));
    }
    Ok(total)
}

fn ensure_settled_service_tier_rates(
    rates: &[DbBillingRateRecord],
    context_tier: Option<&str>,
    service_tier: Option<&str>,
    requested_usage_classes: &[String],
) -> Result<(), String> {
    let Some(service_tier) = service_tier.filter(|tier| *tier != "default") else {
        return Ok(());
    };

    for usage_class in ["input_uncached", "output"] {
        if find_rate(
            rates,
            "token",
            usage_class,
            None,
            context_tier,
            Some(service_tier),
            None,
        )
        .is_none()
        {
            return Err(format!(
                "missing token rate for usage_class={usage_class}, context_tier={context_tier:?}, service_tier={service_tier:?}"
            ));
        }
    }

    // MB-M5: an absent meter rate for a requested server-tool usage class settles at zero,
    // so it is not a missing-rate condition even on a non-default service tier.
    let _ = requested_usage_classes;
    Ok(())
}

pub(super) fn calculate_rate_matrix_charge_components(
    usage: &urp::Usage,
    output: Option<&[urp::Node]>,
    response_service_tier: Option<&str>,
    resolution: &BillingRateResolution,
    provider_multiplier: Multiplier,
    requested_usage_classes: &[String],
) -> Result<MatrixChargeComponents, String> {
    let aggregate = usage.accounting();
    let usage = aggregate.as_ref();
    let input_details = usage.input_details.as_ref();
    let output_details = usage.output_details.as_ref();
    // One FX snapshot and one peak-window determination for the whole request (MB-C1b,
    // MB-R14), so every line item of one charge bills on the same basis no matter how long
    // the request ran.
    let pricing = BillingPricingContext {
        cny_per_usd: resolution.cny_per_usd,
        is_peak: resolution.is_peak,
    };
    let context_tier = determine_context_tier(usage, &resolution.rates)?;
    let service_tier = response_service_tier
        .map(str::trim)
        .filter(|tier| !tier.is_empty())
        .map(str::to_string);
    let context_tier_ref = context_tier.as_deref();
    let service_tier_ref = service_tier.as_deref();
    ensure_settled_service_tier_rates(
        &resolution.rates,
        context_tier_ref,
        service_tier_ref,
        requested_usage_classes,
    )?;

    let cached_tokens = input_details.map(|d| d.cache_read_tokens).unwrap_or(0);
    let cache_creation_tokens = input_details.map(|d| d.cache_creation_tokens).unwrap_or(0);
    let cache_creation_5m = input_details
        .map(|d| d.cache_creation_5m_tokens)
        .unwrap_or(0);
    let cache_creation_1h = input_details
        .map(|d| d.cache_creation_1h_tokens)
        .unwrap_or(0);
    let uncached_tokens = usage
        .input_tokens
        .saturating_sub(cached_tokens)
        .saturating_sub(cache_creation_tokens);
    let reasoning_tokens = output_details.map(|d| d.reasoning_tokens).unwrap_or(0);
    let has_reasoning_rate = reasoning_tokens == 0
        || find_rate(
            &resolution.rates,
            "token",
            "reasoning_output",
            None,
            context_tier_ref,
            service_tier_ref,
            None,
        )
        .is_some();
    let non_reasoning_output_tokens = if has_reasoning_rate {
        usage.output_tokens.saturating_sub(reasoning_tokens)
    } else {
        usage.output_tokens
    };
    let billable_reasoning_tokens = if has_reasoning_rate {
        reasoning_tokens
    } else {
        0
    };
    let can_derive_uncached_modality = input_details.is_some_and(|details| {
        details.modality_breakdown.is_some()
            && details.cache_creation_tokens == 0
            && (details.cache_read_tokens == 0 || details.cache_read_modality_breakdown.is_some())
    });
    let uncached_input_modality_breakdown = if can_derive_uncached_modality
        && has_matching_modality_rates(
            &resolution.rates,
            &["input_uncached"],
            context_tier_ref,
            service_tier_ref,
            None,
        ) {
        input_uncached_modality_breakdown(input_details, uncached_tokens)?
    } else {
        None
    };

    let mut token_line_items = Vec::new();
    let mut token_total = 0i128;
    token_total = token_total
        .checked_add(add_modality_token_lines(
            &mut token_line_items,
            &resolution.rates,
            &["input_uncached"],
            uncached_input_modality_breakdown.as_ref(),
            uncached_tokens,
            context_tier_ref,
            service_tier_ref,
            &pricing,
        )?)
        .ok_or_else(|| "token charge overflow".to_string())?;
    token_total = token_total
        .checked_add(add_cache_read_lines(
            &mut token_line_items,
            &resolution.rates,
            input_details.and_then(|d| d.cache_read_modality_breakdown.as_ref()),
            cached_tokens,
            context_tier_ref,
            service_tier_ref,
            &pricing,
        )?)
        .ok_or_else(|| "token charge overflow".to_string())?;

    let has_cache_5m_rate = find_rate(
        &resolution.rates,
        "token",
        "cache_write_5m",
        None,
        context_tier_ref,
        service_tier_ref,
        Some("5m"),
    )
    .is_some()
        || find_rate(
            &resolution.rates,
            "token",
            "cache_write_5m",
            None,
            context_tier_ref,
            service_tier_ref,
            None,
        )
        .is_some();
    let has_cache_1h_rate = find_rate(
        &resolution.rates,
        "token",
        "cache_write_1h",
        None,
        context_tier_ref,
        service_tier_ref,
        Some("1h"),
    )
    .is_some()
        || find_rate(
            &resolution.rates,
            "token",
            "cache_write_1h",
            None,
            context_tier_ref,
            service_tier_ref,
            None,
        )
        .is_some();
    if cache_creation_tokens > 0
        && cache_creation_5m == 0
        && cache_creation_1h == 0
        && has_cache_5m_rate
        && has_cache_1h_rate
    {
        return Err(
            "cache creation usage requires 5m/1h split for the selected rate matrix".to_string(),
        );
    }
    if cache_creation_5m == 0 && cache_creation_1h == 0 {
        token_total = token_total
            .checked_add(add_token_line(
                &mut token_line_items,
                &resolution.rates,
                "input_uncached",
                cache_creation_tokens,
                None,
                context_tier_ref,
                service_tier_ref,
                None,
                &pricing,
            )?)
            .ok_or_else(|| "token charge overflow".to_string())?;
    } else {
        token_total = token_total
            .checked_add(add_cache_write_line(
                &mut token_line_items,
                &resolution.rates,
                "cache_write_5m",
                cache_creation_5m,
                context_tier_ref,
                service_tier_ref,
                "5m",
                &pricing,
            )?)
            .ok_or_else(|| "token charge overflow".to_string())?;
        token_total = token_total
            .checked_add(add_cache_write_line(
                &mut token_line_items,
                &resolution.rates,
                "cache_write_1h",
                cache_creation_1h,
                context_tier_ref,
                service_tier_ref,
                "1h",
                &pricing,
            )?)
            .ok_or_else(|| "token charge overflow".to_string())?;
    }
    token_total = token_total
        .checked_add(add_modality_token_lines(
            &mut token_line_items,
            &resolution.rates,
            &["output"],
            output_details.and_then(|d| d.modality_breakdown.as_ref()),
            non_reasoning_output_tokens,
            context_tier_ref,
            service_tier_ref,
            &pricing,
        )?)
        .ok_or_else(|| "token charge overflow".to_string())?;
    token_total = token_total
        .checked_add(add_token_line(
            &mut token_line_items,
            &resolution.rates,
            "reasoning_output",
            billable_reasoning_tokens,
            None,
            context_tier_ref,
            service_tier_ref,
            None,
            &pricing,
        )?)
        .ok_or_else(|| "token charge overflow".to_string())?;

    let mut meter_line_items = Vec::new();
    let meter_total = add_meter_lines(
        &mut meter_line_items,
        &resolution.rates,
        usage,
        output,
        requested_usage_classes,
        context_tier_ref,
        service_tier_ref,
        &pricing,
    )?;
    let base_charge = token_total
        .checked_add(meter_total)
        .ok_or_else(|| "charge overflow".to_string())?;
    let final_charge = scale_charge_with_multiplier(base_charge, provider_multiplier)
        .ok_or_else(|| "charge overflow".to_string())?;

    Ok(MatrixChargeComponents {
        token_line_items,
        meter_line_items,
        context_tier,
        service_tier,
        base_charge,
        final_charge,
    })
}

fn build_matrix_billing_breakdown(
    logical_model: &str,
    attempt: &MonoizeAttempt,
    resolution: &BillingRateResolution,
    components: &MatrixChargeComponents,
) -> Value {
    json!({
        "version": 2,
        "currency": "nano_usd",
        "logical_model": logical_model,
        "upstream_model": attempt.upstream_model,
        "pricing_model": resolution.pricing_model,
        "pricing_profile": resolution.pricing_profile,
        "provider_id": attempt.provider_id,
        "provider_multiplier": attempt.model_multiplier,
        "tier": {
            "context_tier": components.context_tier,
            "service_tier": components.service_tier,
        },
        "token_line_items": components.token_line_items,
        "meter_line_items": components.meter_line_items,
        // MB-C4: the snapshot that normalized every CNY-basis line item of this charge. It is
        // recorded even when no line item used it, so a later audit can tell "no CNY rate was
        // applied" apart from "the rate in force is unknown".
        "cny_per_usd": resolution.cny_per_usd.map(|rate| rate.to_string()),
        "base_charge_nano": components.base_charge.to_string(),
        "final_charge_nano": components.final_charge.to_string(),
    })
}

pub(super) fn scale_charge_with_multiplier(
    base_nano: i128,
    provider_multiplier: Multiplier,
) -> Option<i128> {
    provider_multiplier.checked_scale_i128(base_nano)
}

pub(super) async fn maybe_charge_usage(
    state: &AppState,
    auth: &crate::auth::AuthResult,
    attempt: &MonoizeAttempt,
    logical_model: &str,
    usage: &urp::Usage,
    skip_charge: bool,
    response_service_tier: Option<&str>,
    request_id: Option<&str>,
) -> AppResult<ChargeComputation> {
    if skip_charge {
        release_plan_reservation(state, request_id).await?;
        return Ok(ChargeComputation::default());
    }
    maybe_charge_usage_with_output(
        state,
        auth,
        attempt,
        logical_model,
        usage,
        None,
        response_service_tier,
        request_id,
    )
    .await
}

async fn maybe_charge_usage_with_output(
    state: &AppState,
    auth: &crate::auth::AuthResult,
    attempt: &MonoizeAttempt,
    logical_model: &str,
    usage: &urp::Usage,
    output: Option<&[urp::Node]>,
    response_service_tier: Option<&str>,
    request_id: Option<&str>,
) -> AppResult<ChargeComputation> {
    let aggregate = usage.accounting();
    let usage = aggregate.as_ref();
    let resolution = match attempt.billing_rate_resolution.clone() {
        Some(resolution) => Some(resolution),
        None => match attempt.pricing_profile.as_deref() {
            Some(profile) => {
                resolve_billing_rate_matrix(
                    state,
                    &attempt.upstream_model,
                    logical_model,
                    attempt.provider_type,
                    profile,
                )
                .await?
            }
            None => None,
        },
    };
    let resolution = match resolution {
        Some(v) => v,
        None => {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "model_pricing_required",
                format!(
                    "pricing metadata required for model: {}",
                    attempt.upstream_model
                ),
            ));
        }
    };
    let Some(user_id) = auth.user_id.as_deref() else {
        return Ok(ChargeComputation::default());
    };

    let components = match calculate_rate_matrix_charge_components(
        usage,
        output,
        response_service_tier,
        &resolution,
        attempt.model_multiplier,
        &attempt.server_tool_usage_classes,
    ) {
        Ok(v) => v,
        Err(err) => {
            if err.contains("missing token rate")
                || err.contains("missing meter rate")
                || err.contains("requires")
                || err.contains("authoritative usage required")
            {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    "model_pricing_required",
                    err,
                ));
            }
            tracing::error!(
                "billing error: charge calculation failed for model={}: {}",
                attempt.upstream_model,
                err
            );
            return Err(AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "billing_overflow",
                err,
            ));
        }
    };
    let billing_breakdown =
        build_matrix_billing_breakdown(logical_model, attempt, &resolution, &components);
    let charge_nano = components.final_charge;
    if state.node.is_replica()
        && let (Some(metering), Some(request_id)) = (state.metering.as_ref(), request_id)
        && metering.admission_client().has_active(request_id)
    {
        let client = metering.admission_client();
        if charge_nano <= 0 {
            client
                .release(request_id, chrono::Utc::now())
                .await
                .map_err(super::map_admission_client_error)?;
        } else {
            client
                .settle(request_id, charge_nano, chrono::Utc::now())
                .await
                .map_err(super::map_admission_client_error)?;
        }
        return Ok(ChargeComputation {
            charge_nano_usd: (charge_nano > 0).then_some(charge_nano),
            billing_breakdown: Some(billing_breakdown),
        });
    }
    if let Some(request_id) = request_id
        && crate::store_billing::quota::QuotaStore::new(state.db_pool.clone())
            .settle_request_if_reserved(request_id, charge_nano.max(0), chrono::Utc::now())
            .await
            .map_err(map_plan_quota_error)?
            .is_some()
    {
        return Ok(ChargeComputation {
            charge_nano_usd: (charge_nano > 0).then_some(charge_nano),
            billing_breakdown: Some(billing_breakdown),
        });
    }
    if charge_nano <= 0 {
        return Ok(ChargeComputation {
            charge_nano_usd: None,
            billing_breakdown: Some(billing_breakdown),
        });
    }

    // ORGL-19: settlement-time personal-key spend-limit check, replacing the old
    // single daily window. Like the org check, the upstream tokens are consumed,
    // so the row is recorded and the wallet debited, but the response delivery
    // path rejects when a configured window is now exhausted.
    if auth.org_key.is_none()
        && charge_nano > 0
        && let Some(api_key_id) = auth.api_key_id.as_deref()
    {
        match crate::users::org_limits::load_key_windows(&state.user_store, api_key_id).await {
            Ok(Some(windows)) => {
                if let Err(breach) = crate::users::org_limits::evaluate_key_windows(&windows) {
                    return Err(AppError::new(
                        StatusCode::PAYMENT_REQUIRED,
                        "api_key_spend_limit_reached",
                        format!(
                            "api key spend limit reached: {} {}",
                            breach.level, breach.window
                        ),
                    ));
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("spend limit settlement check failed: {err}");
                // Fail open: the wallet balance check still applies.
            }
        }
    }

    // ORGL-5: settlement-time org spend-limit check. The charge is computed and the
    // upstream tokens are already consumed, so a breach here still records the row and
    // debits the wallet (ORGL-7) but rejects the response delivery path.
    if let Some(org_key) = auth.org_key.as_ref() {
        let member_id = org_key
            .created_by
            .clone()
            .unwrap_or_else(|| org_key.org_id.clone());
        match crate::users::org_limits::load_limit_levels(
            &state.user_store,
            &org_key.org_id,
            &member_id,
            &org_key.api_key_id,
        )
        .await
        {
            Ok(levels) => {
                if let Err(breach) = crate::users::org_limits::evaluate_limits(&levels) {
                    return Err(AppError::new(
                        StatusCode::PAYMENT_REQUIRED,
                        "org_spend_limit_reached",
                        format!(
                            "org {} spend limit reached: {} {}",
                            org_key.org_id, breach.level, breach.window
                        ),
                    ));
                }
            }
            Err(err) => {
                tracing::error!("org limit check failed for org {}: {}", org_key.org_id, err);
                // Fail open on query errors: the wallet balance check still applies.
            }
        }
    }

    let meta = json!({
        "logical_model": logical_model,
        "upstream_model": attempt.upstream_model,
        "provider_id": attempt.provider_id,
        "provider_multiplier": attempt.model_multiplier,
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "cached_tokens": usage.cached_tokens(),
        "cache_creation_tokens": usage.input_details.as_ref().map(|d| d.cache_creation_tokens),
        "cache_creation_5m_tokens": usage.input_details.as_ref().map(|d| d.cache_creation_5m_tokens),
        "cache_creation_1h_tokens": usage.input_details.as_ref().map(|d| d.cache_creation_1h_tokens),
        "reasoning_tokens": usage.reasoning_tokens(),
        "charge_nano_usd": charge_nano.to_string(),
        "api_key_id": auth.api_key_id,
        "request_id": request_id,
    });

    if state.node.is_replica() {
        // M3: on replicas the synchronous charge path is replaced by a durable
        // balance-delta enqueue; the primary applies it idempotently later.
        if auth.sub_account_enabled {
            crate::replica::metering::ReplicaMetering::enqueue_balance_delta_for_request(
                state,
                "api_key_charge",
                user_id,
                auth.api_key_id.as_deref(),
                charge_nano,
                &meta,
            )
            .await
            .map_err(|err| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "metering_enqueue_failed",
                    err,
                )
            })?;
        } else {
            crate::replica::metering::ReplicaMetering::enqueue_balance_delta_for_request(
                state,
                "request_charge",
                user_id,
                None,
                charge_nano,
                &meta,
            )
            .await
            .map_err(|err| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "metering_enqueue_failed",
                    err,
                )
            })?;
        }
        return Ok(ChargeComputation {
            charge_nano_usd: Some(charge_nano),
            billing_breakdown: Some(billing_breakdown),
        });
    }

    if auth.sub_account_enabled {
        let api_key_id = auth.api_key_id.as_deref().unwrap_or("");
        match state
            .user_store
            .charge_sub_account_balance_nano(api_key_id, user_id, charge_nano, &meta)
            .await
        {
            Ok(()) => {
                return Ok(ChargeComputation {
                    charge_nano_usd: Some(charge_nano),
                    billing_breakdown: Some(billing_breakdown),
                });
            }
            Err(err) => match err.kind {
                BillingErrorKind::InsufficientBalance => {
                    return Err(AppError::new(
                        StatusCode::PAYMENT_REQUIRED,
                        "insufficient_balance",
                        "insufficient balance",
                    ));
                }
                BillingErrorKind::NotFound => {
                    return Err(AppError::new(
                        StatusCode::UNAUTHORIZED,
                        "unauthorized",
                        "api key not found",
                    ));
                }
                _ => {
                    return Err(AppError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        err.message,
                    ));
                }
            },
        }
    }

    match state
        .user_store
        .charge_user_balance_nano(user_id, charge_nano, &meta)
        .await
    {
        Ok(()) => Ok(ChargeComputation {
            charge_nano_usd: Some(charge_nano),
            billing_breakdown: Some(billing_breakdown),
        }),
        Err(err) => match err.kind {
            BillingErrorKind::InsufficientBalance => Err(AppError::new(
                StatusCode::PAYMENT_REQUIRED,
                "insufficient_balance",
                "insufficient balance",
            )),
            BillingErrorKind::NotFound => Err(AppError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "user not found",
            )),
            _ => Err(AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                err.message,
            )),
        },
    }
}

pub(super) async fn maybe_charge_stream_usage(
    state: &AppState,
    auth: &crate::auth::AuthResult,
    attempt: &MonoizeAttempt,
    logical_model: &str,
    usage: &urp::Usage,
    skip_charge: bool,
    output: &[urp::Node],
    response_service_tier: Option<&str>,
    request_id: Option<&str>,
) -> AppResult<ChargeComputation> {
    if skip_charge {
        release_plan_reservation(state, request_id).await?;
        return Ok(ChargeComputation::default());
    }
    maybe_charge_usage_with_output(
        state,
        auth,
        attempt,
        logical_model,
        usage,
        Some(output),
        response_service_tier,
        request_id,
    )
    .await
}

pub(super) async fn maybe_charge_response(
    state: &AppState,
    auth: &crate::auth::AuthResult,
    attempt: &MonoizeAttempt,
    logical_model: &str,
    response: &urp::UrpResponse,
    skip_charge: bool,
    request_id: Option<&str>,
) -> AppResult<ChargeComputation> {
    if skip_charge {
        release_plan_reservation(state, request_id).await?;
        return Ok(ChargeComputation::default());
    }
    let Some(usage) = response.usage.as_ref() else {
        return Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_usage_required",
            "upstream response did not include billable usage",
        ));
    };
    let response_service_tier = response
        .extra_body
        .get("service_tier")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|tier| !tier.is_empty());
    maybe_charge_usage_with_output(
        state,
        auth,
        attempt,
        logical_model,
        usage,
        Some(response.output.as_slice()),
        response_service_tier,
        request_id,
    )
    .await
}

pub(super) async fn release_plan_reservation(
    state: &AppState,
    request_id: Option<&str>,
) -> AppResult<()> {
    let Some(request_id) = request_id else {
        return Ok(());
    };
    if state.node.is_replica() {
        if let Some(metering) = state.metering.as_ref() {
            metering
                .admission_client()
                .release(request_id, chrono::Utc::now())
                .await
                .map_err(super::map_admission_client_error)?;
        }
        return Ok(());
    }
    crate::store_billing::quota::QuotaStore::new(state.db_pool.clone())
        .release_request_if_reserved(request_id, chrono::Utc::now())
        .await
        .map_err(map_plan_quota_error)?;
    Ok(())
}

pub(super) fn substitute_zero_usage_if_allowed(
    usage: &mut Option<urp::Usage>,
    attempt: &MonoizeAttempt,
) -> bool {
    if usage.is_none() && attempt.allow_missing_usage {
        *usage = Some(urp::Usage::default());
        return true;
    }
    false
}

#[cfg(test)]
mod peak_pricing_tests {
    use super::*;
    use crate::billing_rate_store::RATE_CURRENCY_USD;
    use chrono::TimeZone;

    fn peak_rate() -> DbBillingRateRecord {
        DbBillingRateRecord {
            id: "peak-test-rate".to_string(),
            source: "manual".to_string(),
            pricing_profile: "default".to_string(),
            model_pattern: None,
            provider_type: None,
            rate_kind: "token".to_string(),
            usage_class: "output".to_string(),
            unit: "token".to_string(),
            unit_price_nano: "1000".to_string(),
            unit_price_currency: RATE_CURRENCY_USD.to_string(),
            peak_unit_price_nano: Some("2000".to_string()),
            context_tier: None,
            service_tier: None,
            modality: None,
            cache_ttl: None,
            match_json: serde_json::json!({}),
            priority: 0,
            enabled: true,
            raw_json: serde_json::json!({}),
            updated_at: chrono::Utc::now(),
        }
    }

    fn beijing_time(
        weekday_chrono_day: u32,
        hour: u32,
        minute: u32,
    ) -> chrono::DateTime<chrono::Utc> {
        // 2026-09-14 is a Monday; the weekday advances one day per `weekday_chrono_day`.
        chrono::Utc
            .with_ymd_and_hms(2026, 9, 14 + weekday_chrono_day, hour, minute, 0)
            .single()
            .expect("valid test timestamp")
    }

    /// MB-R14: window edges and the UTC+8 conversion.
    #[test]
    fn beijing_peak_window_boundaries() {
        // Monday 09:00 Beijing (01:00 UTC) through 11:59 is peak.
        assert!(is_beijing_peak_window(beijing_time(0, 1, 0)));
        assert!(is_beijing_peak_window(beijing_time(0, 3, 59)));
        // 12:00-13:59 lunch break is off-peak.
        assert!(!is_beijing_peak_window(beijing_time(0, 4, 0)));
        assert!(!is_beijing_peak_window(beijing_time(0, 5, 59)));
        // 14:00-17:59 afternoon is peak; 18:00 sharp is off-peak.
        assert!(is_beijing_peak_window(beijing_time(0, 6, 0)));
        assert!(is_beijing_peak_window(beijing_time(0, 9, 59)));
        assert!(!is_beijing_peak_window(beijing_time(0, 10, 0)));
        // Weekend is always off-peak, even inside the weekday hours.
        assert!(!is_beijing_peak_window(beijing_time(5, 1, 0)));
        assert!(!is_beijing_peak_window(beijing_time(6, 6, 0)));
        // Friday afternoon is the last peak of the week.
        assert!(is_beijing_peak_window(beijing_time(4, 6, 0)));
    }

    /// MB-R15/MB-C1d: the peak price applies only when the window is peak AND
    /// the row carries one; the line item records which window billed.
    #[test]
    fn token_line_uses_effective_price_and_records_the_window() {
        let rate = peak_rate();
        let mut line_items = Vec::new();
        let charge = add_token_line_for_usage_classes(
            &mut line_items,
            std::slice::from_ref(&rate),
            &["output"],
            10,
            None,
            None,
            None,
            None,
            &BillingPricingContext {
                cny_per_usd: None,
                is_peak: true,
            },
        )
        .expect("peak charge");
        assert_eq!(charge, 10 * 2000);
        assert_eq!(line_items[0]["unit_price_nano"], serde_json::json!("2000"));
        assert_eq!(line_items[0]["pricing_window"], serde_json::json!("peak"));

        let mut line_items = Vec::new();
        add_token_line_for_usage_classes(
            &mut line_items,
            std::slice::from_ref(&rate),
            &["output"],
            10,
            None,
            None,
            None,
            None,
            &BillingPricingContext {
                cny_per_usd: None,
                is_peak: false,
            },
        )
        .expect("off-peak charge");
        assert_eq!(line_items[0]["unit_price_nano"], serde_json::json!("1000"));
        assert_eq!(
            line_items[0]["pricing_window"],
            serde_json::json!("off_peak")
        );
    }

    /// MB-D3g: a row without a peak price bills at `unit_price_nano` even in the
    /// peak window.
    #[test]
    fn peak_window_without_peak_price_falls_back_to_unit_price() {
        let mut rate = peak_rate();
        rate.peak_unit_price_nano = None;
        let mut line_items = Vec::new();
        add_token_line_for_usage_classes(
            &mut line_items,
            std::slice::from_ref(&rate),
            &["output"],
            10,
            None,
            None,
            None,
            None,
            &BillingPricingContext {
                cny_per_usd: None,
                is_peak: true,
            },
        )
        .expect("fallback charge");
        assert_eq!(line_items[0]["unit_price_nano"], serde_json::json!("1000"));
        assert_eq!(
            line_items[0]["pricing_window"],
            serde_json::json!("off_peak")
        );
    }

    /// MB-R15: the hold maximum uses the effective price of the window.
    #[test]
    fn plan_maximum_uses_the_peak_price() {
        let mut input_rate = peak_rate();
        input_rate.usage_class = "input_uncached".to_string();
        let resolution = BillingRateResolution {
            pricing_profile: "default".to_string(),
            pricing_model: "test-model".to_string(),
            rates: vec![input_rate, peak_rate()],
            cny_per_usd: None,
            is_peak: true,
        };
        let maximum = plan_maximum_charge_nano(&resolution, 100, Some(100), &[], Multiplier::ONE)
            .expect("maximum");
        // Both hold legs price 100 tokens at the 2000 peak nano price.
        assert_eq!(maximum, 100 * 2000 + 100 * 2000);

        // The same matrix off-peak holds against the 1000 base price.
        let off_peak_resolution = BillingRateResolution {
            is_peak: false,
            ..resolution
        };
        let maximum =
            plan_maximum_charge_nano(&off_peak_resolution, 100, Some(100), &[], Multiplier::ONE)
                .expect("maximum");
        assert_eq!(maximum, 100 * 1000 + 100 * 1000);
    }
}

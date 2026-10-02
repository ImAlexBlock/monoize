use super::{StreamRuntimeMetrics, StreamTerminalError};
use crate::urp;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn mark_stream_ttfb_if_needed(
    started_at: Option<std::time::Instant>,
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
) {
    let Some(started_at) = started_at else {
        return;
    };
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut guard = runtime_metrics.lock().await;
    if guard.ttfb_ms.is_none() {
        guard.ttfb_ms = Some(started_at.elapsed().as_millis() as u64);
    }
}

pub(crate) async fn record_stream_usage_if_present(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    usage: Option<urp::Usage>,
) {
    let Some(usage) = usage else {
        return;
    };
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut guard = runtime_metrics.lock().await;
    let new_total = usage.total_tokens();
    let replace = match guard.usage.as_ref() {
        Some(existing) => {
            let existing_total = existing.total_tokens();
            new_total >= existing_total
        }
        None => true,
    };
    if replace {
        guard.usage = Some(usage);
    }
}

pub(crate) async fn record_stream_response_id(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    response_id: &str,
) {
    let response_id = response_id.trim();
    if response_id.is_empty() {
        return;
    }
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    runtime_metrics.lock().await.response_id = Some(response_id.to_string());
}

fn json_string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn usage_speed_or_tier(usage: &Value) -> Option<&str> {
    json_string_field(usage, "speed").or_else(|| json_string_field(usage, "service_tier"))
}

pub(crate) fn response_service_tier(value: &Value) -> Option<&str> {
    json_string_field(value, "service_tier")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| json_string_field(response, "service_tier"))
        })
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| json_string_field(message, "service_tier"))
        })
        .or_else(|| value.get("usage").and_then(usage_speed_or_tier))
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
                .and_then(usage_speed_or_tier)
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("usage"))
                .and_then(usage_speed_or_tier)
        })
}

pub(crate) fn usage_service_tier(usage: &urp::Usage) -> Option<&str> {
    usage
        .extra_body
        .get("speed")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            usage
                .extra_body
                .get("service_tier")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
}

pub(crate) async fn record_stream_response_service_tier(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    response: &Value,
) {
    let Some(service_tier) = response_service_tier(response) else {
        return;
    };
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    runtime_metrics.lock().await.response_service_tier = Some(service_tier.to_string());
}

pub(crate) async fn record_observed_upstream_response_model(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    model: &str,
    terminal: bool,
) {
    let model = model.trim();
    if model.is_empty() {
        return;
    }
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut metrics = runtime_metrics.lock().await;
    if metrics.response_model_terminal && !terminal {
        return;
    }
    if terminal || metrics.response_model.is_none() {
        metrics.response_model = Some(model.to_string());
        metrics.response_model_terminal = terminal;
    }
}

pub(crate) async fn record_cumulative_stream_usage_snapshot(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    usage: Option<urp::Usage>,
) {
    let Some(usage) = usage else {
        return;
    };
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    runtime_metrics.lock().await.usage = Some(usage);
}

pub(crate) async fn latest_stream_usage_snapshot(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
) -> Option<urp::Usage> {
    let runtime_metrics = runtime_metrics.as_ref()?;
    let guard = runtime_metrics.lock().await;
    guard.usage.clone()
}

pub(crate) async fn record_stream_done_sentinel(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
) {
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut guard = runtime_metrics.lock().await;
    guard.terminal.saw_done_sentinel = true;
}

pub(crate) async fn increment_estimated_output_tokens(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    chars: u64,
) {
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut guard = runtime_metrics.lock().await;
    guard.estimated_output_tokens += (chars + 3) / 4;
}

pub(crate) async fn record_visible_output_delta(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    content: &str,
) {
    if content.is_empty() {
        return;
    }
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut guard = runtime_metrics.lock().await;
    guard.visible_output_bytes = guard
        .visible_output_bytes
        .saturating_add(content.len() as u64);
}

pub(crate) async fn record_visible_stream_event_delta(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    event: &urp::UrpStreamEvent,
) {
    let content = match event {
        urp::UrpStreamEvent::NodeDelta {
            delta:
                urp::NodeDelta::Text {
                    signature: _,
                    citations: _,
                    logprobs: _,
                    content,
                },
            ..
        }
        | urp::UrpStreamEvent::NodeDelta {
            delta: urp::NodeDelta::Refusal { content, .. },
            ..
        } => content.as_str(),
        _ => return,
    };
    record_visible_output_delta(runtime_metrics, content).await;
}

pub(crate) async fn record_stream_terminal_event(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    event: &str,
    finish_reason: Option<&str>,
) {
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut guard = runtime_metrics.lock().await;
    guard.terminal.terminal_event = Some(event.to_string());
    if let Some(reason) = finish_reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        guard.terminal.terminal_finish_reason = Some(reason.to_string());
    }
}

pub(crate) async fn record_stream_terminal_error(
    runtime_metrics: &Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    event: &str,
    error: StreamTerminalError,
) {
    let Some(runtime_metrics) = runtime_metrics.as_ref() else {
        return;
    };
    let mut guard = runtime_metrics.lock().await;
    guard.terminal.terminal_event = Some(event.to_string());
    guard.terminal.terminal_error = Some(error);
}

pub(crate) fn usage_to_chat_usage_json(usage: &urp::Usage) -> Value {
    let usage = usage.accounting();
    let mut obj = json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens(),
        "completion_tokens_details": {
            "reasoning_tokens": usage.reasoning_tokens().unwrap_or(0),
            "accepted_prediction_tokens": usage.output_details.as_ref().map(|d| d.accepted_prediction_tokens).unwrap_or(0),
            "rejected_prediction_tokens": usage.output_details.as_ref().map(|d| d.rejected_prediction_tokens).unwrap_or(0)
        },
        "prompt_tokens_details": {
            "cached_tokens": usage.cached_tokens().unwrap_or(0),
            "cache_write_tokens": usage.input_details.as_ref().map(|d| d.cache_creation_tokens).unwrap_or(0),
            "cache_creation_tokens": usage.input_details.as_ref().map(|d| d.cache_creation_tokens).unwrap_or(0),
            "tool_prompt_tokens": usage.input_details.as_ref().map(|d| d.tool_prompt_tokens).unwrap_or(0)
        }
    });
    if let Some(map) = obj.as_object_mut() {
        for detail_key in ["prompt_tokens_details", "completion_tokens_details"] {
            let Some(extra_detail) = usage.extra_body.get(detail_key).and_then(Value::as_object)
            else {
                continue;
            };
            let Some(generated_detail) = map.get_mut(detail_key).and_then(Value::as_object_mut)
            else {
                continue;
            };
            for (key, value) in extra_detail {
                if !key.starts_with("_monoize_") {
                    generated_detail
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
            }
        }

        for (k, v) in &usage.extra_body {
            if !k.starts_with("_monoize_")
                && !matches!(
                    k.as_str(),
                    "prompt_tokens_details"
                        | "completion_tokens_details"
                        | "total_tokens"
                        | "prompt_tokens"
                        | "completion_tokens"
                        | "input_tokens"
                        | "output_tokens"
                )
            {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    urp::usage::write_modality(
        &mut obj["prompt_tokens_details"],
        &usage
            .input_details
            .as_ref()
            .and_then(|d| d.modality_breakdown.clone()),
    );
    urp::usage::write_modality(
        &mut obj["completion_tokens_details"],
        &usage
            .output_details
            .as_ref()
            .and_then(|d| d.modality_breakdown.clone()),
    );
    obj
}

fn split_usage_extra(usage: &Map<String, Value>, known_keys: &[&str]) -> HashMap<String, Value> {
    usage
        .iter()
        .filter_map(|(k, v)| {
            if known_keys.contains(&k.as_str()) || urp::decode::is_internal_extra_key(k) {
                None
            } else {
                let mut value = v.clone();
                if let Some(object) = value.as_object_mut() {
                    object.retain(|key, _| !urp::decode::is_internal_extra_key(key));
                }
                Some((k.clone(), value))
            }
        })
        .collect()
}

fn parse_modality_breakdown_from_detail_object(
    detail: Option<&Map<String, Value>>,
) -> Option<urp::ModalityBreakdown> {
    let detail = detail?;
    let modality = detail
        .get("modality_breakdown")
        .and_then(|v| v.as_object())
        .unwrap_or(detail);
    let breakdown = urp::ModalityBreakdown {
        text_tokens: modality.get("text_tokens").and_then(|v| v.as_u64()),
        image_tokens: modality.get("image_tokens").and_then(|v| v.as_u64()),
        audio_tokens: modality.get("audio_tokens").and_then(|v| v.as_u64()),
        video_tokens: modality.get("video_tokens").and_then(|v| v.as_u64()),
        document_tokens: modality.get("document_tokens").and_then(|v| v.as_u64()),
    };
    if breakdown.text_tokens.is_some()
        || breakdown.image_tokens.is_some()
        || breakdown.audio_tokens.is_some()
        || breakdown.video_tokens.is_some()
        || breakdown.document_tokens.is_some()
    {
        Some(breakdown)
    } else {
        None
    }
}

fn parse_cache_read_modality_breakdown_from_detail_object(
    detail: Option<&Map<String, Value>>,
) -> Option<urp::ModalityBreakdown> {
    let detail = detail?;
    for key in [
        "cache_read_tokens_details",
        "cached_tokens_details",
        "cached_input_tokens_details",
    ] {
        if let Some(breakdown) =
            parse_modality_breakdown_from_detail_object(detail.get(key).and_then(|v| v.as_object()))
        {
            return Some(breakdown);
        }
    }

    let breakdown = urp::ModalityBreakdown {
        text_tokens: detail
            .get("cache_read_text_tokens")
            .or_else(|| detail.get("cached_text_tokens"))
            .or_else(|| detail.get("cached_input_text_tokens"))
            .and_then(|v| v.as_u64()),
        image_tokens: detail
            .get("cache_read_image_tokens")
            .or_else(|| detail.get("cached_image_tokens"))
            .or_else(|| detail.get("cached_input_image_tokens"))
            .and_then(|v| v.as_u64()),
        audio_tokens: detail
            .get("cache_read_audio_tokens")
            .or_else(|| detail.get("cached_audio_tokens"))
            .or_else(|| detail.get("cached_input_audio_tokens"))
            .and_then(|v| v.as_u64()),
        video_tokens: detail
            .get("cache_read_video_tokens")
            .or_else(|| detail.get("cached_video_tokens"))
            .or_else(|| detail.get("cached_input_video_tokens"))
            .and_then(|v| v.as_u64()),
        document_tokens: detail
            .get("cache_read_document_tokens")
            .or_else(|| detail.get("cached_document_tokens"))
            .or_else(|| detail.get("cached_input_document_tokens"))
            .and_then(|v| v.as_u64()),
    };
    if breakdown.text_tokens.is_some()
        || breakdown.image_tokens.is_some()
        || breakdown.audio_tokens.is_some()
        || breakdown.video_tokens.is_some()
        || breakdown.document_tokens.is_some()
    {
        Some(breakdown)
    } else {
        None
    }
}

fn make_input_details(
    standard_tokens: u64,
    cache_read_tokens: u64,
    cache_read_modality_breakdown: Option<urp::ModalityBreakdown>,
    cache_creation_tokens: u64,
    tool_prompt_tokens: u64,
    modality_breakdown: Option<urp::ModalityBreakdown>,
) -> Option<urp::InputDetails> {
    if standard_tokens > 0
        || cache_read_tokens > 0
        || cache_read_modality_breakdown.is_some()
        || cache_creation_tokens > 0
        || tool_prompt_tokens > 0
        || modality_breakdown.is_some()
    {
        Some(urp::InputDetails {
            tool_prompt_modality_breakdown: None,
            standard_tokens,
            cache_read_tokens,
            cache_read_modality_breakdown,
            cache_creation_tokens,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            tool_prompt_tokens,
            modality_breakdown,
        })
    } else {
        None
    }
}

fn make_output_details(
    standard_tokens: u64,
    reasoning_tokens: u64,
    accepted_prediction_tokens: u64,
    rejected_prediction_tokens: u64,
    modality_breakdown: Option<urp::ModalityBreakdown>,
) -> Option<urp::OutputDetails> {
    if standard_tokens > 0
        || reasoning_tokens > 0
        || accepted_prediction_tokens > 0
        || rejected_prediction_tokens > 0
        || modality_breakdown.is_some()
    {
        Some(urp::OutputDetails {
            standard_tokens,
            reasoning_tokens,
            accepted_prediction_tokens,
            rejected_prediction_tokens,
            modality_breakdown,
        })
    } else {
        None
    }
}

pub(crate) fn parse_usage_from_chat_object(obj: &Value) -> Option<urp::Usage> {
    let usage = obj.get("usage")?.as_object()?;
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_u64())?;
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_u64())?;
    let prompt_details = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .and_then(|v| v.as_object());
    let completion_details = usage
        .get("completion_tokens_details")
        .or_else(|| usage.get("output_tokens_details"))
        .and_then(|v| v.as_object());
    // C3-i-a: the cached subset of the inclusive prompt total may also arrive
    // in top-level usage fields of chat-shaped upstreams; the first positive
    // field in precedence order wins.
    let cached_tokens = [
        usage
            .get("prompt_tokens_details")
            .and_then(|v| v.get("cached_tokens")),
        usage
            .get("input_tokens_details")
            .and_then(|v| v.get("cached_tokens")),
        usage.get("prompt_cache_hit_tokens"),
        usage.get("input_cache_read"),
        usage.get("cache_read_input_tokens"),
    ]
    .into_iter()
    .filter_map(|value| value.and_then(crate::urp::decode::value_to_u64))
    .find(|&value| value > 0)
    .unwrap_or(0);
    let cache_creation_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|v| v.get("cache_write_tokens"))
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|v| v.get("cache_creation_tokens"))
        })
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|v| v.get("cache_write_tokens"))
        })
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|v| v.get("cache_creation_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let tool_prompt_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|v| v.get("tool_prompt_tokens"))
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|v| v.get("tool_prompt_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|v| v.get("reasoning_tokens"))
        .or_else(|| {
            usage
                .get("output_tokens_details")
                .and_then(|v| v.get("reasoning_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let accepted_prediction_tokens = usage
        .get("completion_tokens_details")
        .and_then(|v| v.get("accepted_prediction_tokens"))
        .or_else(|| {
            usage
                .get("output_tokens_details")
                .and_then(|v| v.get("accepted_prediction_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let rejected_prediction_tokens = usage
        .get("completion_tokens_details")
        .and_then(|v| v.get("rejected_prediction_tokens"))
        .or_else(|| {
            usage
                .get("output_tokens_details")
                .and_then(|v| v.get("rejected_prediction_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut extra_body = split_usage_extra(
        usage,
        &[
            "prompt_tokens",
            "completion_tokens",
            "input_tokens",
            "output_tokens",
        ],
    );
    for key in [
        "prompt_tokens_details",
        "input_tokens_details",
        "completion_tokens_details",
        "output_tokens_details",
    ] {
        if let Some(Value::Object(details)) = extra_body.get_mut(key) {
            for field in [
                "text_tokens",
                "image_tokens",
                "audio_tokens",
                "video_tokens",
                "document_tokens",
            ] {
                details.remove(field);
            }
        }
    }
    Some(urp::Usage {
        iterations: None,
        input_tokens,
        output_tokens,
        input_details: make_input_details(
            0,
            cached_tokens,
            parse_cache_read_modality_breakdown_from_detail_object(prompt_details),
            cache_creation_tokens,
            tool_prompt_tokens,
            parse_modality_breakdown_from_detail_object(prompt_details),
        ),
        output_details: make_output_details(
            0,
            reasoning_tokens,
            accepted_prediction_tokens,
            rejected_prediction_tokens,
            parse_modality_breakdown_from_detail_object(completion_details),
        ),
        extra_body,
    })
}

pub(crate) fn parse_usage_from_responses_object(obj: &Value) -> Option<urp::Usage> {
    let usage = obj
        .get("usage")
        .or_else(|| obj.get("response").and_then(|v| v.get("usage")))?;
    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(|v| v.as_u64())?;
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(|v| v.as_u64())?;
    let input_details_obj = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"))
        .and_then(|v| v.as_object());
    let output_details_obj = usage
        .get("output_tokens_details")
        .or_else(|| usage.get("completion_tokens_details"))
        .and_then(|v| v.as_object());
    let cached_tokens = usage
        .get("input_tokens_details")
        .and_then(|v| v.get("cached_tokens"))
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|v| v.get("cached_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("output_tokens_details")
        .and_then(|v| v.get("reasoning_tokens"))
        .or_else(|| {
            usage
                .get("completion_tokens_details")
                .and_then(|v| v.get("reasoning_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_creation_tokens = usage
        .get("input_tokens_details")
        .and_then(|v| v.get("cache_creation_tokens"))
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|v| v.get("cache_write_tokens"))
        })
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|v| v.get("cache_creation_tokens"))
        })
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|v| v.get("cache_write_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let tool_prompt_tokens = usage
        .get("input_tokens_details")
        .and_then(|v| v.get("tool_prompt_tokens"))
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|v| v.get("tool_prompt_tokens"))
        })
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let accepted_prediction_tokens = usage
        .get("output_tokens_details")
        .and_then(|v| v.get("accepted_prediction_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let rejected_prediction_tokens = usage
        .get("output_tokens_details")
        .and_then(|v| v.get("rejected_prediction_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let extra_body = split_usage_extra(
        usage.as_object()?,
        &[
            "input_tokens",
            "output_tokens",
            "prompt_tokens",
            "completion_tokens",
        ],
    );
    Some(urp::Usage {
        iterations: None,
        input_tokens,
        output_tokens,
        input_details: make_input_details(
            0,
            cached_tokens,
            parse_cache_read_modality_breakdown_from_detail_object(input_details_obj),
            cache_creation_tokens,
            tool_prompt_tokens,
            parse_modality_breakdown_from_detail_object(input_details_obj),
        ),
        output_details: make_output_details(
            0,
            reasoning_tokens,
            accepted_prediction_tokens,
            rejected_prediction_tokens,
            parse_modality_breakdown_from_detail_object(output_details_obj),
        ),
        extra_body,
    })
}

pub(crate) fn parse_usage_from_gemini_object(obj: &Value) -> Option<urp::Usage> {
    let usage = obj.get("usageMetadata")?.as_object()?;
    for keys in [["promptTokenCount", "prompt_token_count"], ["candidatesTokenCount", "candidates_token_count"]] {
        usage.get(keys[0]).or_else(|| usage.get(keys[1])).and_then(urp::decode::value_to_u64)?;
    }
    urp::decode::gemini::parse_usage(usage).ok()
}

pub(super) fn parse_usage_from_embeddings_object(obj: &Value) -> Option<urp::Usage> {
    let usage = obj.get("usage")?.as_object()?;
    let input_tokens = usage.get("prompt_tokens")?.as_u64()?;
    let total_tokens = usage.get("total_tokens")?.as_u64()?;
    let mut extra_body = HashMap::new();
    extra_body.insert("total_tokens".to_string(), Value::from(total_tokens));
    Some(urp::Usage {
        iterations: None,
        input_tokens,
        output_tokens: 0,
        input_details: None,
        output_details: None,
        extra_body,
    })
}
#[cfg(test)]
mod upstream_response_model_stream_tests {
    use super::*;
    use crate::config::ProviderType;
    use crate::handlers::UrpRequest;
    use serde_json::json;
    use tokio::sync::mpsc;

    async fn observe_models(
        provider_type: ProviderType,
        events: Vec<Value>,
    ) -> (Option<String>, bool) {
        let body = events
            .into_iter()
            .map(|event| {
                let event_name = event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message");
                format!("event: {event_name}\ndata: {event}\n\n")
            })
            .collect::<String>();
        let response =
            reqwest::Response::from(axum::http::Response::new(reqwest::Body::from(body)));
        let metrics = Arc::new(Mutex::new(StreamRuntimeMetrics::default()));
        let request = UrpRequest {
            audio_output_format: Default::default(),
            messages_custom_tool_names: Default::default(),
            model: "sent-model".to_string(),
            max_multiplier: None,
            server_tool_usage_classes: Vec::new(),
            affinity_explicit: None,
            affinity_prefix_hash: String::new(),
            estimated_input_tokens: 0,
            has_tools: false,
        };
        let (tx, _rx) = mpsc::channel(64);
        crate::urp::stream_decode::stream_upstream_to_urp_events(
            &request,
            None,
            provider_type,
            response,
            tx,
            None,
            Some(Arc::clone(&metrics)),
            1_000,
        )
        .await
        .expect("decode upstream stream");
        let metrics = metrics.lock().await;
        (
            metrics.response_model.clone(),
            metrics.response_model_terminal,
        )
    }

    #[tokio::test]
    async fn responses_terminal_model_replaces_initial_model() {
        let observed = observe_models(
            ProviderType::Responses,
            vec![
                json!({"type": "response.created", "response": {"id": "r1", "model": "initial-model", "output": []}}),
                json!({"type": "response.in_progress", "response": {"id": "r1", "model": "intermediate-model", "output": []}}),
                json!({"type": "response.completed", "response": {"id": "r1", "model": " terminal-model ", "status": "completed", "output": []}}),
            ],
        )
        .await;
        assert_eq!(observed, (Some("terminal-model".to_string()), true));
    }

    #[tokio::test]
    async fn responses_keeps_first_declaration_when_terminal_model_is_absent() {
        let observed = observe_models(
            ProviderType::Responses,
            vec![
                json!({"type": "response.created", "response": {"id": "r1", "model": "initial-model", "output": []}}),
                json!({"type": "response.in_progress", "response": {"id": "r1", "model": "intermediate-model", "output": []}}),
                json!({"type": "response.completed", "response": {"id": "r1", "status": "completed", "output": []}}),
            ],
        )
        .await;
        assert_eq!(observed, (Some("initial-model".to_string()), false));
    }

    #[tokio::test]
    async fn chat_terminal_model_replaces_initial_model() {
        let observed = observe_models(
            ProviderType::ChatCompletion,
            vec![
                json!({"model": "initial-model", "choices": [{"index": 0, "delta": {"content": "Hello"}, "finish_reason": null}]}),
                json!({"model": "terminal-model", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
                json!({"model": "usage-model", "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1}}),
            ],
        )
        .await;
        assert_eq!(observed, (Some("terminal-model".to_string()), true));
    }

    #[tokio::test]
    async fn gemini_terminal_model_version_replaces_initial_model() {
        let observed = observe_models(
            ProviderType::Gemini,
            vec![
                json!({"modelVersion": "initial-model", "candidates": [{"content": {"parts": [{"text": "Hello"}]}}]}),
                json!({"modelVersion": "terminal-model", "candidates": [{"finishReason": "STOP"}]}),
            ],
        )
        .await;
        assert_eq!(observed, (Some("terminal-model".to_string()), true));
    }

    #[tokio::test]
    async fn gemini_later_nonterminal_models_do_not_replace_first_declaration() {
        let observed = observe_models(ProviderType::Gemini, vec![
            json!({"modelVersion":"initial-model", "candidates":[{"content":{"parts":[{"text":"Hello"}]}}]}),
            json!({"modelVersion":"intermediate-model", "candidates":[{"content":{"parts":[{"text":" world"}]}}]}),
            json!({"candidates":[{"finishReason":"STOP"}]}),
            json!({"modelVersion":"usage-only-model", "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1}}),
        ]).await;
        assert_eq!(observed, (Some("initial-model".to_string()), false));
    }

    #[tokio::test]
    async fn image_terminal_model_replaces_partial_model() {
        assert_eq!(
            observe_models(
                ProviderType::OpenaiImage,
                vec![
                    json!({"type": "image_generation.partial_image", "model": "initial-model"}),
                    json!({"type": "image_generation.completed", "model": "actual-image-model", "b64_json": "aGVsbG8=", "output_format": "png"}),
                ],
            ).await,
            (Some("actual-image-model".to_string()), true)
        );
    }

    #[tokio::test]
    async fn absent_stream_model_is_not_inferred_from_sent_model() {
        for (provider, events) in [
            (
                ProviderType::Responses,
                vec![
                    json!({"type": "response.completed", "response": {"id": "r1", "status": "completed", "output": []}}),
                ],
            ),
            (
                ProviderType::ChatCompletion,
                vec![json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})],
            ),
            (
                ProviderType::Gemini,
                vec![json!({"candidates": [{"content": {"parts": []}, "finishReason": "STOP"}]})],
            ),
        ] {
            assert_eq!(observe_models(provider, events).await, (None, false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::urp::{NodeDelta, UrpStreamEvent};
    use std::collections::HashMap;
    #[tokio::test]
    async fn visible_output_bytes_count_only_visible_text_and_refusal_deltas() {
        let metrics = Arc::new(Mutex::new(StreamRuntimeMetrics::default()));
        let runtime_metrics = Some(metrics.clone());

        record_visible_stream_event_delta(
            &runtime_metrics,
            &UrpStreamEvent::NodeDelta {
                node_index: 0,
                delta: NodeDelta::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
                    content: "hello".to_string(),
                },
                usage: None,
                extra_body: HashMap::new(),
            },
        )
        .await;
        record_visible_stream_event_delta(
            &runtime_metrics,
            &UrpStreamEvent::NodeDelta {
                node_index: 1,
                delta: NodeDelta::Reasoning {
            metadata: Default::default(),
                    content: Some("hidden".to_string()),
                    encrypted: None,
                    summary: None,
                    source: None,
                },
                usage: None,
                extra_body: HashMap::new(),
            },
        )
        .await;
        record_visible_stream_event_delta(
            &runtime_metrics,
            &UrpStreamEvent::NodeDelta {
                node_index: 2,
                delta: NodeDelta::ToolCallArguments {
                    arguments: "{\"x\":1}".to_string(),
                },
                usage: None,
                extra_body: HashMap::new(),
            },
        )
        .await;
        record_visible_stream_event_delta(
            &runtime_metrics,
            &UrpStreamEvent::NodeDelta {
                node_index: 3,
                delta: NodeDelta::Refusal {
            logprobs: Default::default(),
                    content: "拒绝".to_string(),
                },
                usage: None,
                extra_body: HashMap::new(),
            },
        )
        .await;

        // "hello" (5 bytes) + "拒绝" (6 UTF-8 bytes); reasoning and tool
        // arguments are not visible output.
        assert_eq!(metrics.lock().await.visible_output_bytes, 11);
    }

    #[test]
    fn chat_stream_usage_maps_top_level_cache_read_aliases() {
        // DeepSeek shape: cached subset in a top-level usage field of a
        // chat-shaped stream chunk; `prompt_tokens` stays the inclusive total.
        let usage = parse_usage_from_chat_object(&json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "prompt_cache_hit_tokens": 60
            }
        }))
        .expect("usage should decode");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(
            usage
                .input_details
                .expect("input details from alias")
                .cache_read_tokens,
            60
        );

        // DashScope shape via the input_tokens naming.
        let usage = parse_usage_from_chat_object(&json!({
            "usage": {
                "input_tokens": 80,
                "output_tokens": 4,
                "input_cache_read": 30
            }
        }))
        .expect("usage should decode");
        assert_eq!(
            usage
                .input_details
                .expect("input details from alias")
                .cache_read_tokens,
            30
        );

        // The standard nested field keeps precedence over the top-level aliases.
        let usage = parse_usage_from_chat_object(&json!({
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 2,
                "prompt_tokens_details": { "cached_tokens": 7 },
                "prompt_cache_hit_tokens": 999
            }
        }))
        .expect("usage should decode");
        assert_eq!(
            usage
                .input_details
                .expect("input details")
                .cache_read_tokens,
            7
        );
    }

    #[test]
    fn gemini_stream_usage_builds_inclusive_totals() {
        let usage = parse_usage_from_gemini_object(&json!({
            "usageMetadata": {
                "promptTokenCount": 27,
                "toolUsePromptTokenCount": 10_309,
                "candidatesTokenCount": 45,
                "thoughtsTokenCount": 31
            }
        }))
        .expect("usage should decode");

        assert_eq!(usage.input_tokens, 10_336);
        assert_eq!(usage.output_tokens, 76);
        assert_eq!(
            usage
                .input_details
                .as_ref()
                .expect("input details")
                .tool_prompt_tokens,
            10_309
        );
        assert_eq!(usage.reasoning_tokens(), Some(31));
    }

    #[test]
    fn gemini_stream_usage_rejects_inclusive_total_overflow() {
        assert!(
            parse_usage_from_gemini_object(&json!({
                "usageMetadata": {
                    "promptTokenCount": u64::MAX,
                    "toolUsePromptTokenCount": 1,
                    "candidatesTokenCount": 0
                }
            }))
            .is_none()
        );
    }
}

#[cfg(test)]
mod typed_gemini_usage_tests {
    use super::*;

    #[test]
    fn gemini_stream_usage_keeps_modality_and_tool_prompt_detail() {
        let value = json!({"usageMetadata": {
            "promptTokenCount":10, "toolUsePromptTokenCount":3,
            "candidatesTokenCount":4, "thoughtsTokenCount":2,
            "promptTokensDetails":[{"modality":"TEXT","tokenCount":8},{"modality":"IMAGE","tokenCount":2}],
            "toolUsePromptTokensDetails":[{"modality":"TEXT","tokenCount":3}]
        }});
        let usage = parse_usage_from_gemini_object(&value).unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (13, 6));
        let details = usage.input_details.unwrap();
        assert_eq!(details.modality_breakdown.unwrap().image_tokens, Some(2));
        assert_eq!(details.tool_prompt_modality_breakdown.unwrap().text_tokens, Some(3));
    }
}

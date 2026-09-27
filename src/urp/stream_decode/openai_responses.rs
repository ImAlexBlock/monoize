use crate::error::{AppError, AppResult};
use crate::handlers::routing::now_ts;
use crate::handlers::usage::{
    mark_stream_ttfb_if_needed, parse_usage_from_responses_object,
    record_observed_upstream_response_model, record_stream_done_sentinel,
    record_stream_response_id, record_stream_response_service_tier, record_stream_terminal_error,
    record_stream_terminal_event, record_stream_usage_if_present,
    record_visible_stream_event_delta,
};
use crate::handlers::{StreamRuntimeMetrics, StreamTerminalError, UrpRequest as HandlerUrpRequest};
#[cfg(test)]
use crate::urp::internal_legacy_bridge::nodes_to_items;
use crate::urp::internal_legacy_bridge::{Item, Part, Role};
use crate::urp::stream_helpers::{
    extract_reasoning_parts, extract_responses_message_phase, extract_responses_message_text,
};
use crate::urp::{
    FinishReason, Node, NodeDelta, NodeHeader, OrdinaryRole, ProviderProtocol,
    RESPONSES_IMAGE_GENERATION_CALL_EXTRA_KEY, RESPONSES_STREAM_START_SOURCE_EXTRA_KEY,
    ToolCallType, UrpStreamEvent, node_is_empty_text, nodes_semantically_match,
};
use axum::http::StatusCode;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

const RESPONSES_SSE_MAX_DATA_BYTES: usize = 8 * 1024 * 1024;
const RESPONSES_SSE_MAX_JOINED_VALUES: usize = 64;

struct ParsedResponsesSseData {
    events: Vec<(String, Value)>,
    done: bool,
}

fn parse_responses_sse_data(data: &str) -> Result<ParsedResponsesSseData, String> {
    parse_responses_sse_data_with_event(data, "")
}

fn parse_responses_sse_data_with_event(
    data: &str,
    sse_event: &str,
) -> Result<ParsedResponsesSseData, String> {
    if data.len() > RESPONSES_SSE_MAX_DATA_BYTES {
        return Err(format!(
            "upstream Responses event data exceeds {RESPONSES_SSE_MAX_DATA_BYTES} bytes"
        ));
    }

    let trimmed = data.trim();
    let (json_data, done) = if trimmed == "[DONE]" {
        ("", true)
    } else if let Some(prefix) = trimmed.strip_suffix("[DONE]") {
        (prefix.trim_end(), true)
    } else {
        (trimmed, false)
    };
    if json_data.is_empty() {
        return if done {
            Ok(ParsedResponsesSseData {
                events: Vec::new(),
                done,
            })
        } else {
            Err("upstream Responses event data is empty".to_string())
        };
    }

    let mut events = Vec::new();
    for value in serde_json::Deserializer::from_str(json_data).into_iter::<Value>() {
        let value = value.map_err(|error| error.to_string())?;
        if events.len() == RESPONSES_SSE_MAX_JOINED_VALUES {
            return Err(format!(
                "upstream Responses event contains more than {RESPONSES_SSE_MAX_JOINED_VALUES} JSON values"
            ));
        }
        let payload_type = value
            .as_object()
            .and_then(|object| object.get("type"))
            .and_then(Value::as_str)
            .filter(|event_name| !event_name.is_empty());
        // PR3d: a non-empty SSE `event:` field other than "message" names the frame;
        // otherwise the payload's own non-empty string `type` does. Joined values
        // in one frame may each carry their own `type`, which wins per value.
        let event_name = if !sse_event.is_empty() && sse_event != "message" {
            sse_event.to_string()
        } else if let Some(name) = payload_type {
            name.to_string()
        } else {
            return Err(
                "upstream Responses event value must be an object with a non-empty string type"
                    .to_string(),
            );
        };
        events.push((event_name, value));
    }
    if events.is_empty() {
        return Err("upstream Responses event contains no JSON value".to_string());
    }

    Ok(ParsedResponsesSseData { events, done })
}

include!("openai_responses/image_helpers.inc.rs");
include!("openai_responses/stream_loop_part1.inc.rs");
include!("openai_responses/stream_loop_part2.inc.rs");
include!("openai_responses/event_map.inc.rs");
include!("openai_responses/state.inc.rs");
include!("openai_responses/output_events.inc.rs");
include!("openai_responses/completed.inc.rs");
include!("openai_responses/decode_helpers.inc.rs");
include!("openai_responses/tests.inc.rs");

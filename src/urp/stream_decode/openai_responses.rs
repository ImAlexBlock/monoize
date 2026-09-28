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
use crate::urp::internal_legacy_bridge::{Part, Role};
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
        // otherwise each joined value uses its own type or a bare error envelope.
        let event_name = if !sse_event.is_empty() && sse_event != "message" {
            sse_event.to_string()
        } else if let Some(name) = payload_type {
            name.to_string()
        } else if value.get("error").is_some_and(|error| !error.is_null())
            && value.get("response").is_none()
        {
            "error".to_string()
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

#[cfg(test)]
mod joined_sse_data_tests {
    use super::*;

    #[test]
    fn splits_multiple_complete_typed_objects_in_source_order() {
        let parsed = parse_responses_sse_data(
            "{\"type\":\"response.created\",\"sequence_number\":0}\n{\"type\":\"response.in_progress\",\"sequence_number\":1}",
        )
        .expect("joined event is valid");

        assert!(!parsed.done);
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.events[0].0, "response.created");
        assert_eq!(parsed.events[1].0, "response.in_progress");
    }

    #[test]
    fn accepts_done_after_complete_objects() {
        let parsed = parse_responses_sse_data(
            "{\"type\":\"response.completed\",\"response\":{}}\n[DONE]",
        )
        .expect("terminal joined event is valid");

        assert!(parsed.done);
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].0, "response.completed");
    }

    #[test]
    fn rejects_any_invalid_value_without_partial_output() {
        for data in [
            "{\"type\":\"response.created\"}\ntrailing",
            "{\"sequence_number\":0}",
            "[]",
            "{\"type\":\"\"}",
        ] {
            assert!(parse_responses_sse_data(data).is_err(), "accepted {data:?}");
        }
    }

    #[test]
    fn sse_event_field_wins_over_payload_type() {
        let parsed = parse_responses_sse_data_with_event(
            "{\"type\":\"response.completed\",\"response\":{}}",
            "response.incomplete",
        )
        .expect("named SSE event with payload type is valid");

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].0, "response.incomplete");
    }

    #[test]
    fn sse_message_event_falls_back_to_payload_type() {
        let parsed = parse_responses_sse_data_with_event(
            "{\"type\":\"response.completed\",\"response\":{}}",
            "message",
        )
        .expect("generic SSE event name falls back to the payload type");

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].0, "response.completed");
    }

    #[test]
    fn empty_sse_event_falls_back_to_payload_type() {
        let parsed = parse_responses_sse_data_with_event(
            "{\"type\":\"response.created\"}",
            "",
        )
        .expect("missing SSE event name falls back to the payload type");

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].0, "response.created");
    }

    #[test]
    fn sse_event_cannot_replace_a_missing_payload_type() {
        let parsed = parse_responses_sse_data_with_event("{\"sequence_number\":0}", "response.created")
            .expect("SSE event name satisfies the naming rule without a payload type");

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].0, "response.created");
    }

    #[test]
    fn enforces_joined_event_bounds() {
        let too_many = (0..65)
            .map(|index| format!("{{\"type\":\"response.vendor.{index}\"}}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(parse_responses_sse_data(&too_many).is_err());

        let oversized = format!(
            "{{\"type\":\"response.vendor\",\"padding\":\"{}\"}}",
            "x".repeat(8 * 1024 * 1024)
        );
        assert!(parse_responses_sse_data(&oversized).is_err());
    }
}

#[cfg(test)]
mod delta_extra_body_tests {
    use super::*;

    #[test]
    fn output_text_delta_extra_body_excludes_the_wire_event_type() {
        let events = map_responses_event_to_urp_events_with_state(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "content_index": 0,
                "item_id": "msg_mock",
                "delta": "answer",
                "vendor_hint": "keep"
            }),
            &HashMap::new(),
            &mut ResponsesStreamIndexState::default(),
        );
        let extra = events.iter().find_map(|event| match event {
            UrpStreamEvent::NodeDelta { extra_body, .. } => Some(extra_body),
            _ => None,
        }).expect("text delta");

        assert!(
            !extra.contains_key("type"),
            "wire event type must not enter item extra_body: {extra:?}"
        );
        assert_eq!(extra.get("vendor_hint"), Some(&json!("keep")));
    }
}

pub mod anthropic;
pub mod gemini;
pub mod openai_chat;
pub mod openai_image;
pub mod openai_responses;
pub mod replicate;

use crate::config::ProviderType;
use crate::error::{AppError, AppResult};
use crate::handlers::{StreamRuntimeMetrics, UrpRequest};
use crate::urp::UrpStreamEvent;
use axum::http::StatusCode;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

fn explicit_stream_error_status(value: &Value) -> Option<u16> {
    ["status", "status_code"].into_iter().find_map(|field| {
        value
            .get(field)
            .and_then(Value::as_u64)
            .filter(|status| (400..=599).contains(status))
            .map(|status| status as u16)
    })
}

fn inferred_stream_error_status(code: Option<&str>, error_type: Option<&str>) -> Option<u16> {
    let signals = [code, error_type].map(|signal| signal.map(|s| s.trim().to_ascii_lowercase()));
    if signals.iter().flatten().any(|signal| {
        matches!(
            signal.as_str(),
            "server_is_overloaded"
                | "service_unavailable_error"
                | "overloaded_error"
                | "service_unavailable"
                | "temporarily_unavailable"
        )
    }) {
        Some(StatusCode::SERVICE_UNAVAILABLE.as_u16())
    } else if signals.iter().flatten().any(|signal| {
        matches!(signal.as_str(), "server_error" | "internal_server_error")
    }) {
        Some(StatusCode::BAD_GATEWAY.as_u16())
    } else {
        None
    }
}

pub(crate) async fn stream_upstream_to_urp_events(
    urp: &UrpRequest,
    pending_request_envelope_extra: Option<HashMap<String, Value>>,
    provider_type: ProviderType,
    upstream_resp: reqwest::Response,
    tx: mpsc::Sender<UrpStreamEvent>,
    started_at: Option<std::time::Instant>,
    runtime_metrics: Option<Arc<Mutex<StreamRuntimeMetrics>>>,
    idle_timeout_ms: u64,
) -> AppResult<()> {
    match provider_type {
        // ST-E4: video channel types never enter the chat stream path.
        ProviderType::OpenaiVideo | ProviderType::FalVideo => Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "provider_type_not_supported",
            "video channel types support only the studio executor",
        )),
        ProviderType::Responses => {
            openai_responses::stream_responses_to_urp_events(
                urp,
                pending_request_envelope_extra,
                upstream_resp,
                tx,
                started_at,
                runtime_metrics,
                idle_timeout_ms,
            )
            .await
        }
        ProviderType::ChatCompletion => {
            openai_chat::stream_chat_to_urp_events(
                urp,
                upstream_resp,
                tx,
                started_at,
                runtime_metrics,
                idle_timeout_ms,
            )
            .await
        }
        ProviderType::Messages => {
            anthropic::stream_messages_to_urp_events(
                urp,
                upstream_resp,
                tx,
                started_at,
                runtime_metrics,
                idle_timeout_ms,
            )
            .await
        }
        ProviderType::Gemini => {
            gemini::stream_gemini_to_urp_events(
                urp,
                upstream_resp,
                tx,
                started_at,
                runtime_metrics,
                idle_timeout_ms,
            )
            .await
        }
        ProviderType::OpenaiImage => {
            openai_image::stream_image_to_urp_events(
                urp,
                upstream_resp,
                tx,
                started_at,
                runtime_metrics,
                idle_timeout_ms,
            )
            .await
        }
        ProviderType::Replicate => {
            replicate::stream_replicate_to_urp_events(
                urp,
                upstream_resp,
                tx,
                started_at,
                runtime_metrics,
                idle_timeout_ms,
            )
            .await
        }
        ProviderType::Group => Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "provider_type_not_supported",
            "group is virtual",
        )),
    }
}

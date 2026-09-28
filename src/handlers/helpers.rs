use super::*;
use crate::transforms::stream_split_sse_frames::DEFAULT_MAX_FRAME_LENGTH;
use crate::urp::ImageSource;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use xxhash_rust::xxh3::Xxh3;

#[allow(clippy::result_large_err)]
pub(super) fn decode_urp_request(
    protocol: DownstreamProtocol,
    known: Value,
    extra: Map<String, Value>,
) -> AppResult<urp::UrpRequest> {
    let merged = merge_known_and_extra(known, extra);
    let decoded = match protocol {
        DownstreamProtocol::Responses => urp::decode::openai_responses::decode_request(&merged),
        DownstreamProtocol::ChatCompletions => urp::decode::openai_chat::decode_request(&merged),
        DownstreamProtocol::AnthropicMessages => urp::decode::anthropic::decode_request(&merged),
    };
    decoded.map_err(|e| AppError::new(StatusCode::BAD_REQUEST, "invalid_request", e))
}

pub(super) fn merge_known_and_extra(known: Value, extra: Map<String, Value>) -> Value {
    let mut obj = known.as_object().cloned().unwrap_or_default();
    for (k, v) in extra {
        obj.insert(k, v);
    }
    Value::Object(obj)
}

pub(super) fn resolve_max_multiplier(
    req: &urp::UrpRequest,
    headers: &HeaderMap,
    auth: &crate::auth::AuthResult,
) -> Option<Multiplier> {
    let ceiling = auth.max_multiplier;
    let requested =
        read_max_multiplier_from_extra(req).or_else(|| parse_max_multiplier_header(headers));

    match (ceiling, requested) {
        (Some(c), Some(r)) => Some(r.min(c)),
        (Some(c), None) => Some(c),
        (None, Some(r)) => Some(r),
        (None, None) => None,
    }
}

pub(super) fn extract_client_ip(headers: &HeaderMap) -> Option<String> {
    crate::client_ip::canonical_client_ip_from_headers(headers).map(|address| address.to_string())
}

/// Reject the request if the API key has an IP whitelist and the client IP is not in it.
#[allow(clippy::result_large_err)]
pub(super) fn check_ip_whitelist(
    auth: &crate::auth::AuthResult,
    headers: &HeaderMap,
) -> AppResult<()> {
    if auth.ip_whitelist.is_empty() {
        return Ok(());
    }
    let client_ip = crate::client_ip::canonical_client_ip_from_headers(headers);
    let allowed = client_ip.is_some_and(|client_ip| {
        auth.ip_whitelist.iter().any(|entry| {
            entry
                .parse::<std::net::IpAddr>()
                .is_ok_and(|allowed| allowed == client_ip)
                || entry
                    .parse::<ipnet::IpNet>()
                    .is_ok_and(|network| network.contains(&client_ip))
        })
    });
    if !allowed {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "ip_not_allowed",
            "client IP is not in the API key whitelist",
        ));
    }
    Ok(())
}

pub(super) fn extract_request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// CM-AFF-1a: read the client-supplied session-affinity value. Underscore and
/// hyphenated aliases are both accepted because some reverse proxies drop
/// header names that contain `_`. OpenCode clients send `x-opencode-session`.
/// Values are sanitized per the shared affinity sanitizer; an empty result
/// means "absent".
pub(super) fn extract_client_session_id(headers: &HeaderMap) -> Option<String> {
    for name in [
        "session_id",
        "session-id",
        "x-session-id",
        "x-opencode-session",
        "x-session-affinity",
    ] {
        if let Some(raw) = headers.get(name).and_then(|value| value.to_str().ok()) {
            let sanitized = crate::handlers::routing::sanitize_session_affinity(raw);
            if !sanitized.is_empty() {
                return Some(sanitized);
            }
        }
    }
    None
}

pub(super) fn read_max_multiplier_from_extra(req: &urp::UrpRequest) -> Option<Multiplier> {
    req.extra_body
        .get("max_multiplier")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
}

pub(super) fn inject_monoize_context(auth: &crate::auth::AuthResult, req: &mut urp::UrpRequest) {
    req.context.username = auth.username.clone();
    req.context.api_key_id = auth.api_key_id.clone();
}

pub(super) fn strip_monoize_context(req: &mut urp::UrpRequest) {
    req.context.username = None;
    req.context.api_key_id = None;
}

pub(super) async fn apply_transform_rules_request(
    state: &AppState,
    req: &mut urp::UrpRequest,
    rules: &[TransformRuleConfig],
    match_model: &str,
    upstream_provider_type: Option<ProviderType>,
) -> AppResult<()> {
    if rules.is_empty() {
        return Ok(());
    }
    let mut states = transforms::build_states_for_rules(rules, state.transform_registry.as_ref())
        .map_err(|e| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "transform_init_failed",
            e.to_string(),
        )
    })?;
    let context = transforms::TransformRuntimeContext {
        image_transform_cache: state.image_transform_cache.clone(),
        http_client: state.http.clone(),
        upstream_provider_type,
    };
    transforms::apply_transforms(
        transforms::UrpData::Request(req),
        rules,
        &mut states,
        match_model,
        Phase::Request,
        &context,
        state.transform_registry.as_ref(),
    )
    .await
    .map_err(|e| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "transform_apply_failed",
            e.to_string(),
        )
    })
}

pub(super) async fn apply_transform_rules_response(
    state: &AppState,
    resp: &mut urp::UrpResponse,
    rules: &[TransformRuleConfig],
    model: &str,
    upstream_provider_type: Option<ProviderType>,
) -> AppResult<()> {
    if !rules.is_empty() {
        let mut states =
            transforms::build_states_for_rules(rules, state.transform_registry.as_ref()).map_err(
                |e| {
                    AppError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "transform_init_failed",
                        e.to_string(),
                    )
                },
            )?;
        let context = transforms::TransformRuntimeContext {
            image_transform_cache: state.image_transform_cache.clone(),
            http_client: state.http.clone(),
            upstream_provider_type,
        };
        transforms::apply_transforms(
            transforms::UrpData::Response(resp),
            rules,
            &mut states,
            model,
            Phase::Response,
            &context,
            state.transform_registry.as_ref(),
        )
        .await
        .map_err(|e| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "transform_apply_failed",
                e.to_string(),
            )
        })?;
    }
    urp::integerize_tool_call_nodes(&mut resp.output);
    Ok(())
}

pub(super) async fn transform_urp_stream(
    state: &AppState,
    mut rx: mpsc::Receiver<urp::UrpStreamEvent>,
    tx: mpsc::Sender<urp::UrpStreamEvent>,
    provider_rules: &[TransformRuleConfig],
    global_rules: &[TransformRuleConfig],
    auth_rules: &[TransformRuleConfig],
    model: &str,
    upstream_provider_type: Option<ProviderType>,
    reasoning_envelope: Option<(&str, &str)>,
) -> AppResult<()> {
    let mut provider_states =
        transforms::build_states_for_rules(provider_rules, state.transform_registry.as_ref())
            .map_err(|e| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "transform_init_failed",
                    e.to_string(),
                )
            })?;
    let mut global_states =
        transforms::build_states_for_rules(global_rules, state.transform_registry.as_ref())
            .map_err(|e| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "transform_init_failed",
                    e.to_string(),
                )
            })?;
    let mut auth_states =
        transforms::build_states_for_rules(auth_rules, state.transform_registry.as_ref()).map_err(
            |e| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "transform_init_failed",
                    e.to_string(),
                )
            },
        )?;
    let context = transforms::TransformRuntimeContext {
        image_transform_cache: state.image_transform_cache.clone(),
        http_client: state.http.clone(),
        upstream_provider_type,
    };

    let mut reasoning_envelope_state = urp::ReasoningEnvelopeStreamState::default();
    while let Some(event) = rx.recv().await {
        // Fragment surfaces are assembled before envelope construction. This
        // keeps response transforms from observing raw fragments or a string
        // made by concatenating several independently wrapped envelopes.
        let enveloped_events = match reasoning_envelope {
            Some((provider_type, upstream_model)) => {
                reasoning_envelope_state.wrap_event(event, provider_type, upstream_model)
            }
            None => vec![event],
        };

        for event in enveloped_events {
            let provider_events = transforms::apply_stream_transforms(
                event,
                provider_rules,
                &mut provider_states,
                model,
                Phase::Response,
                &context,
                state.transform_registry.as_ref(),
            )
            .await
            .map_err(|e| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "transform_apply_failed",
                    e.to_string(),
                )
            })?;

            for provider_event in provider_events {
                let global_events = transforms::apply_stream_transforms(
                    provider_event,
                    global_rules,
                    &mut global_states,
                    model,
                    Phase::Response,
                    &context,
                    state.transform_registry.as_ref(),
                )
                .await
                .map_err(|e| {
                    AppError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "transform_apply_failed",
                        e.to_string(),
                    )
                })?;

                for global_event in global_events {
                    let auth_events = transforms::apply_stream_transforms(
                        global_event,
                        auth_rules,
                        &mut auth_states,
                        model,
                        Phase::Response,
                        &context,
                        state.transform_registry.as_ref(),
                    )
                    .await
                    .map_err(|e| {
                        AppError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "transform_apply_failed",
                            e.to_string(),
                        )
                    })?;

                    for mut auth_event in auth_events {
                        urp::integerize_tool_call_stream_event(&mut auth_event);
                        tx.send(auth_event).await.map_err(|_| {
                            AppError::new(
                                StatusCode::BAD_GATEWAY,
                                "stream_transform_failed",
                                "failed to forward transformed stream event",
                            )
                        })?;
                    }
                }
            }
        }
    }

    Ok(())
}

#[allow(clippy::result_large_err)]
pub(crate) fn typed_request_to_legacy(
    req: &urp::UrpRequest,
    max_multiplier: Option<Multiplier>,
) -> AppResult<UrpRequest> {
    let mut legacy = build_routing_stub(req, max_multiplier);
    legacy.messages_custom_tool_names = messages_custom_bridge_names(req);
    Ok(legacy)
}

fn affinity_value_from_json(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .as_i64()
                .map(|v| v.to_string())
                .or_else(|| value.as_u64().map(|v| v.to_string()))
        })
}

/// CM-AFF-1b: raw conversation identifier from the decoded request body.
/// Returns the identifier string itself so a header uuid and a body uuid match.
pub(super) fn stable_session_affinity_raw(req: &urp::UrpRequest) -> Option<String> {
    const SESSION_KEYS: &[&str] = &[
        "session_id",
        "session",
        "conversation_id",
        "conversation",
        "thread_id",
        "thread",
    ];
    for key in SESSION_KEYS {
        if let Some(value) = req.extra_body.get(*key).and_then(affinity_value_from_json) {
            return Some(value);
        }
    }
    if let Some(metadata) = req.extra_body.get("metadata").and_then(Value::as_object) {
        for key in SESSION_KEYS {
            if let Some(value) = metadata.get(*key).and_then(affinity_value_from_json) {
                return Some(value);
            }
        }
    }
    if let Some(value) = req
        .extra_body
        .get("user_id")
        .and_then(affinity_value_from_json)
    {
        return Some(value);
    }
    if let Some(metadata) = req.extra_body.get("metadata").and_then(Value::as_object)
        && let Some(value) = metadata.get("user_id").and_then(affinity_value_from_json)
    {
        return Some(value);
    }
    req.user
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn stable_affinity_field(req: &urp::UrpRequest) -> Option<String> {
    if let Some(previous_response_id) = req
        .extra_body
        .get("previous_response_id")
        .and_then(affinity_value_from_json)
    {
        return Some(format!("previous_response_id:{previous_response_id}"));
    }
    if let Some(user) = req.user.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(format!("user:{user}"));
    }
    const KEYS: &[&str] = &[
        "session_id",
        "session",
        "conversation_id",
        "conversation",
        "thread_id",
        "thread",
        "user_id",
        "user",
    ];
    for key in KEYS {
        if *key == "request_id" {
            continue;
        }
        if let Some(value) = req.extra_body.get(*key).and_then(affinity_value_from_json) {
            return Some(format!("{key}:{value}"));
        }
    }
    if let Some(metadata) = req.extra_body.get("metadata").and_then(Value::as_object) {
        for key in KEYS {
            if *key == "request_id" {
                continue;
            }
            if let Some(value) = metadata.get(*key).and_then(affinity_value_from_json) {
                return Some(format!("metadata.{key}:{value}"));
            }
        }
    }
    None
}

pub(crate) fn short_xxh3_hex(input: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(input.as_bytes()))
}

const AFFINITY_PREFIX_NODE_LIMIT: usize = 8;
const AFFINITY_PREFIX_BYTE_LIMIT: usize = 16 * 1024;

struct BoundedHashWriter {
    hasher: Xxh3,
    remaining: usize,
    limit_reached: bool,
}

impl BoundedHashWriter {
    fn new(limit: usize) -> Self {
        Self {
            hasher: Xxh3::new(),
            remaining: limit,
            limit_reached: false,
        }
    }

    fn digest(&self) -> u64 {
        self.hasher.digest()
    }
}

impl Write for BoundedHashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let accepted = self.remaining.min(buf.len());
        if accepted > 0 {
            self.hasher.update(&buf[..accepted]);
            self.remaining -= accepted;
        }
        if accepted < buf.len() {
            self.limit_reached = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "affinity prefix byte limit reached",
            ));
        }
        Ok(accepted)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// AFF-5a mirror of `urp::Node` that borrows every field and replaces the
/// flattened `HashMap` extras with a sorted `BTreeMap`. `HashMap` iteration
/// order is randomized per instance, so serializing `Node` directly would
/// hash identical requests to different affinity keys.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CanonicalAffinityNode<'a> {
    Text {
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: &'a Option<Value>,
        #[serde(skip_serializing_if = "<[crate::urp::Citation]>::is_empty")]
        citations: &'a [crate::urp::Citation],
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        role: urp::OrdinaryRole,
        content: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: &'a Option<String>,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    Image {
        #[serde(skip_serializing_if = "empty_media_metadata")]
        metadata: &'a urp::MediaMetadata,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        role: urp::OrdinaryRole,
        source: &'a ImageSource,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    Audio {
        #[serde(skip_serializing_if = "empty_media_metadata")]
        metadata: &'a urp::MediaMetadata,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        role: urp::OrdinaryRole,
        source: &'a urp::AudioSource,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    File {
        #[serde(skip_serializing_if = "empty_media_metadata")]
        metadata: &'a urp::MediaMetadata,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        role: urp::OrdinaryRole,
        source: &'a urp::FileSource,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    Refusal {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        content: &'a str,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    Reasoning {
        #[serde(skip_serializing_if = "CanonicalAffinityReasoningMetadata::is_empty")]
        metadata: CanonicalAffinityReasoningMetadata<'a>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted: &'a Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: &'a Option<String>,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    ToolCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: &'a Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        tool_type: urp::ToolCallType,
        call_id: &'a str,
        name: &'a str,
        arguments: &'a str,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    ProviderItem {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        origin_protocol: urp::ProviderProtocol,
        role: urp::OrdinaryRole,
        item_type: &'a str,
        body: &'a Value,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    ToolResult {
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: &'a Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: &'a Option<String>,
        tool_type: urp::ToolCallType,
        call_id: &'a str,
        is_error: bool,
        content: Vec<CanonicalAffinityToolResultContent<'a>>,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    NextDownstreamEnvelopeExtra {
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
}

#[derive(Serialize)]
struct CanonicalAffinityReasoningMetadata<'a> {
    redacted: bool,
    downstream_only: bool,
    chat_content: bool,
    summary_as_thinking: bool,
    item_id: &'a Option<String>,
    summary_parts: Option<CanonicalAffinityReasoningParts<'a>>,
    content_parts: Option<CanonicalAffinityReasoningParts<'a>>,
}

struct CanonicalAffinityReasoningParts<'a>(&'a [urp::ReasoningTextPart]);

impl Serialize for CanonicalAffinityReasoningParts<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        #[derive(Serialize)]
        struct Entry<'a> {
            byte_length: usize,
            #[serde(flatten)]
            extra_body: BTreeMap<&'a String, &'a Value>,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for part in self.0 {
            sequence.serialize_element(&Entry {
                byte_length: part.byte_length,
                extra_body: sorted_extra(&part.extra_body),
            })?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CanonicalAffinityToolResultContent<'a> {
    Text {
        text: &'a str,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    Image {
        #[serde(skip_serializing_if = "empty_media_metadata")]
        metadata: &'a urp::MediaMetadata,
        source: &'a ImageSource,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    File {
        #[serde(skip_serializing_if = "empty_media_metadata")]
        metadata: &'a urp::MediaMetadata,
        source: &'a urp::FileSource,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
    ProviderItem {
        origin_protocol: urp::ProviderProtocol,
        item_type: &'a str,
        body: &'a Value,
        #[serde(flatten)]
        extra_body: BTreeMap<&'a String, &'a Value>,
    },
}

fn empty_media_metadata(metadata: &urp::MediaMetadata) -> bool {
    metadata == &urp::MediaMetadata::default()
}

impl CanonicalAffinityReasoningMetadata<'_> {
    fn is_empty(&self) -> bool {
        !self.redacted && !self.downstream_only && !self.chat_content && !self.summary_as_thinking
            && self.item_id.is_none() && self.summary_parts.is_none() && self.content_parts.is_none()
    }
}

fn sorted_extra(extra_body: &HashMap<String, Value>) -> BTreeMap<&String, &Value> {
    extra_body.iter().collect()
}

fn canonical_affinity_tool_result_content(
    content: &urp::ToolResultContent,
) -> CanonicalAffinityToolResultContent<'_> {
    match content {
        urp::ToolResultContent::Text { text, extra_body } => {
            CanonicalAffinityToolResultContent::Text {
                text,
                extra_body: sorted_extra(extra_body),
            }
        }
        urp::ToolResultContent::Image {
            source,
            metadata,
            extra_body,
        } => CanonicalAffinityToolResultContent::Image {
            metadata,
            source,
            extra_body: sorted_extra(extra_body),
        },
        urp::ToolResultContent::File {
            source,
            metadata,
            extra_body,
        } => CanonicalAffinityToolResultContent::File {
            metadata,
            source,
            extra_body: sorted_extra(extra_body),
        },
        urp::ToolResultContent::ProviderItem {
            origin_protocol,
            item_type,
            body,
            extra_body,
        } => CanonicalAffinityToolResultContent::ProviderItem {
            origin_protocol: *origin_protocol,
            item_type,
            body,
            extra_body: sorted_extra(extra_body),
        },
    }
}

fn canonical_affinity_node(node: &urp::Node) -> CanonicalAffinityNode<'_> {
    match node {
        urp::Node::Text {
            logprobs: _,
            signature,
            citations,
            id,
            role,
            content,
            phase,
            extra_body,
        } => CanonicalAffinityNode::Text {
            signature,
            citations,
            id,
            role: *role,
            content,
            phase,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::Image {
            metadata,
            id,
            role,
            source,
            extra_body,
            ..
        } => CanonicalAffinityNode::Image {
            metadata,
            id,
            role: *role,
            source,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::Audio {
            metadata,
            id,
            role,
            source,
            extra_body,
            ..
        } => CanonicalAffinityNode::Audio {
            metadata,
            id,
            role: *role,
            source,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::File {
            metadata,
            id,
            role,
            source,
            extra_body,
            ..
        } => CanonicalAffinityNode::File {
            metadata,
            id,
            role: *role,
            source,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::Refusal {
            logprobs: _,
            id,
            content,
            extra_body,
        } => CanonicalAffinityNode::Refusal {
            id,
            content,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::Reasoning {
            metadata,
            id,
            content,
            encrypted,
            summary,
            source,
            extra_body,
        } => CanonicalAffinityNode::Reasoning {
            metadata: CanonicalAffinityReasoningMetadata {
                redacted: metadata.redacted,
                downstream_only: metadata.downstream_only,
                chat_content: metadata.chat_content,
                summary_as_thinking: metadata.summary_as_thinking,
                item_id: &metadata.item_id,
                summary_parts: metadata
                    .summary_parts
                    .as_deref()
                    .map(CanonicalAffinityReasoningParts),
                content_parts: metadata
                    .content_parts
                    .as_deref()
                    .map(CanonicalAffinityReasoningParts),
            },
            id,
            content,
            encrypted,
            summary,
            source,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::ToolCall {
            namespace,
            signature,
            id,
            tool_type,
            call_id,
            name,
            arguments,
            extra_body,
            ..
        } => CanonicalAffinityNode::ToolCall {
            namespace,
            signature,
            id,
            tool_type: *tool_type,
            call_id,
            name,
            arguments,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::ProviderItem {
            id,
            origin_protocol,
            role,
            item_type,
            body,
            extra_body,
        } => CanonicalAffinityNode::ProviderItem {
            id,
            origin_protocol: *origin_protocol,
            role: *role,
            item_type,
            body,
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::ToolResult {
            signature,
            namespace,
            name,
            id,
            tool_type,
            call_id,
            is_error,
            content,
            extra_body,
            ..
        } => CanonicalAffinityNode::ToolResult {
            signature,
            namespace,
            name,
            id,
            tool_type: *tool_type,
            call_id,
            is_error: *is_error,
            content: content
                .iter()
                .map(canonical_affinity_tool_result_content)
                .collect(),
            extra_body: sorted_extra(extra_body),
        },
        urp::Node::NextDownstreamEnvelopeExtra { extra_body } => {
            CanonicalAffinityNode::NextDownstreamEnvelopeExtra {
                extra_body: sorted_extra(extra_body),
            }
        }
    }
}

#[cfg(test)]
mod canonical_field_regressions {
    use super::*;

    fn fingerprint(node: Value) -> String {
        let request: urp::UrpRequest =
            serde_json::from_value(json!({"model":"m","input":[node]})).unwrap();
        affinity_prefix_hash(&request)
    }

    #[test]
    fn affinity_hash_distinguishes_typed_signatures_namespaces_and_media() {
        for (base, field, changed) in [
            (
                json!({"type":"text","role":"assistant","content":"same"}),
                "signature",
                json!("signature"),
            ),
            (
                json!({"type":"text","role":"assistant","content":"same"}),
                "citations",
                json!([urp::Citation::decode(
                    json!({"uri":"https://example.com"}),
                    urp::ProviderProtocol::Gemini
                )]),
            ),
            (
                json!({"type":"tool_call","call_id":"c","name":"run","arguments":"{}"}),
                "namespace",
                json!("functions"),
            ),
            (
                json!({"type":"tool_call","call_id":"c","name":"run","arguments":"{}"}),
                "signature",
                json!("signature"),
            ),
            (
                json!({"type":"tool_result","call_id":"c","content":[]}),
                "namespace",
                json!("functions"),
            ),
            (
                json!({"type":"tool_result","call_id":"c","content":[]}),
                "name",
                json!("run"),
            ),
            (
                json!({"type":"reasoning","content":"same"}),
                "metadata",
                json!({"redacted":true}),
            ),
        ] {
            let mut other = base.clone();
            other[field] = changed;
            assert_ne!(fingerprint(base), fingerprint(other), "missing {field}");
        }
        for kind in ["image", "audio", "file"] {
            let base = json!({"type":kind,"role":"assistant","source":{"type":"base64","media_type":"image/png","data":"bytes"}});
            let mut changed = base.clone();
            changed["metadata"] = json!({"signature":"new","media_type":"image/png","transcript":"words","expires_at":20});
            assert_ne!(
                fingerprint(base),
                fingerprint(changed),
                "missing {kind} metadata"
            );
        }
    }

    #[test]
    fn affinity_reasoning_part_unknown_fields_have_stable_order() {
        let left: urp::Node = serde_json::from_value(json!({"type":"reasoning","content":"same","metadata":{"content_parts":[{"byte_length":4,"z":3,"a":1,"m":2}]}})).unwrap();
        let right: urp::Node = serde_json::from_value(json!({"type":"reasoning","content":"same","metadata":{"content_parts":[{"m":2,"a":1,"z":3,"byte_length":4}]}})).unwrap();
        assert_eq!(
            serde_json::to_string(&canonical_affinity_node(&left)).unwrap(),
            serde_json::to_string(&canonical_affinity_node(&right)).unwrap()
        );
    }

    #[test]
    fn gemini_native_tools_and_allowed_lists_survive_provider_filtering() {
        let mut request = urp::decode::gemini::decode_request(&json!({"contents":[],"tools":[
            {"computerUse":{"environment":"ENVIRONMENT_BROWSER"}}, {"googleSearch":{}},
            {"functionDeclarations":[{"name":"run","parametersJsonSchema":{"type":"object"}}]}],
            "toolConfig":{"functionCallingConfig":{"mode":"VALIDATED","allowedFunctionNames":["run"]}}})).unwrap();
        filter_tools_for_provider(
            &mut request,
            ProviderType::Gemini,
            DownstreamProtocol::Responses,
        );
        assert_eq!(request.tools.as_ref().unwrap().len(), 3);
        assert!(request.tool_choice.is_some());
        assert_eq!(
            urp::encode::gemini::encode_request(&request, "m")["toolConfig"]["functionCallingConfig"],
            json!({"mode":"VALIDATED","allowedFunctionNames":["run"]})
        );
        filter_tools_for_provider(
            &mut request,
            ProviderType::Responses,
            DownstreamProtocol::Responses,
        );
        assert_eq!(request.tools.as_ref().unwrap().len(), 1);
        assert_eq!(request.tools.as_ref().unwrap()[0].tool_type, "function");
    }
}

fn affinity_prefix_hash(req: &urp::UrpRequest) -> String {
    let mut writer = BoundedHashWriter::new(AFFINITY_PREFIX_BYTE_LIMIT);
    let result = (|| -> std::io::Result<()> {
        writer.write_all(b"[")?;
        for (index, node) in req
            .input
            .iter()
            .take(AFFINITY_PREFIX_NODE_LIMIT)
            .enumerate()
        {
            if index > 0 {
                writer.write_all(b",")?;
            }
            if let Err(error) = serde_json::to_writer(&mut writer, &canonical_affinity_node(node)) {
                if writer.limit_reached {
                    return Ok(());
                }
                return Err(std::io::Error::other(error));
            }
        }
        writer.write_all(b"]")
    })();
    if result.is_err() && !writer.limit_reached {
        return short_xxh3_hex("");
    }
    format!("{:016x}", writer.digest())
}

/// EST-1 byte sink: counts serialized bytes without storing them. Unlike
/// `BoundedHashWriter` there is no node-count or byte cap, because the estimate
/// must cover the whole request, not a prefix.
struct CountingWriter {
    bytes: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// EST-1: estimates the request's input tokens before routing. Serializes every
/// input node with the same canonical form as the affinity prefix hash (no caps),
/// adds the compact serialization of `tools` when present, and divides bytes by
/// four. Production `chat_completion` upstream bodies measure ~4.2 bytes per
/// reported prompt token, so the estimate errs low by at most ~5%. Routing
/// threshold input only; never persisted as usage.
pub(super) fn estimate_input_tokens(req: &urp::UrpRequest) -> u64 {
    let mut writer = CountingWriter { bytes: 0 };
    let nodes = req
        .input
        .iter()
        .map(canonical_affinity_node)
        .collect::<Vec<_>>();
    let _ = serde_json::to_writer(&mut writer, &nodes);
    if let Some(tools) = &req.tools {
        let _ = serde_json::to_writer(&mut writer, tools);
    }
    writer.bytes.div_ceil(4)
}

pub(super) fn build_routing_stub(
    req: &urp::UrpRequest,
    max_multiplier: Option<Multiplier>,
) -> UrpRequest {
    UrpRequest {
        audio_output_format: req
            .extra_body
            .get("audio")
            .and_then(|v| v.get("format"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        model: req.model.clone(),
        max_multiplier,
        server_tool_usage_classes: server_tool_usage_classes(req.tools.as_deref()),
        messages_custom_tool_names: HashSet::new(),
        affinity_explicit: stable_affinity_field(req),
        affinity_prefix_hash: affinity_prefix_hash(req),
        estimated_input_tokens: estimate_input_tokens(req),
        has_tools: req.tools.as_ref().is_some_and(|tools| !tools.is_empty()),
    }
}

pub(super) fn media_resource_scope(attempt: &MonoizeAttempt) -> Option<urp::MediaResource> {
    use sha2::{Digest, Sha256};
    let protocol = provider_type_protocol(attempt.provider_type)?;
    let mut hash = Sha256::new();
    hash.update(attempt.base_url.as_bytes());
    hash.update([0]);
    hash.update(attempt.api_key.as_bytes());
    Some(urp::MediaResource {
        protocol,
        provider_id: Some(attempt.provider_id.clone()),
        channel_id: Some(attempt.channel_id.clone()),
        credential_scope: Some(hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()),
    })
}

pub(super) fn bind_media_request_routes(
    request: &mut urp::UrpRequest,
    attempts: &mut Vec<MonoizeAttempt>,
) -> AppResult<()> {
    let refs = urp::media::resources(&request.input).map_err(|message| {
        AppError::new(StatusCode::BAD_REQUEST, "invalid_file_reference", message)
    })?;
    if refs.is_empty() {
        return Ok(());
    }
    attempts.retain(|attempt| {
        media_resource_scope(attempt).is_some_and(|scope| {
            refs.iter()
                .all(|reference| urp::media::resource_matches_scope(reference, &scope))
        })
    });
    let scopes: Vec<_> = attempts.iter().filter_map(media_resource_scope).collect();
    let scope = scopes.first().ok_or_else(|| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            "incompatible_file_reference",
            "No route matches the file reference source. Supply file bytes or a public URL.",
        )
    })?;
    if scopes.iter().any(|other| other != scope) {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "ambiguous_file_reference",
            "The file reference is not bound to one provider and credential scope. Supply file bytes or use an unambiguous source route.",
        ));
    }
    urp::media::bind_resources(&mut request.input, scope);
    Ok(())
}

pub(super) fn validate_media_request_route(
    request: &urp::UrpRequest,
    attempt: &MonoizeAttempt,
) -> AppResult<()> {
    let refs = urp::media::resources(&request.input).map_err(|message| {
        AppError::new(StatusCode::BAD_REQUEST, "invalid_file_reference", message)
    })?;
    if refs.is_empty() {
        return Ok(());
    }
    let valid = media_resource_scope(attempt).is_some_and(|scope| {
        refs.iter()
            .all(|reference| urp::media::resource_matches_scope(reference, &scope))
    });
    if !valid {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "incompatible_file_reference",
            "The file reference cannot be used with this provider or credential scope.",
        ));
    }
    Ok(())
}

pub(super) fn build_embeddings_routing_stub(
    model: &str,
    max_multiplier: Option<Multiplier>,
) -> UrpRequest {
    UrpRequest {
            audio_output_format: Default::default(),
            messages_custom_tool_names: Default::default(),
        model: model.to_string(),
        max_multiplier,
        server_tool_usage_classes: Vec::new(),
        affinity_explicit: None,
        affinity_prefix_hash: short_xxh3_hex(model),
        estimated_input_tokens: 0,
        has_tools: false,
    }
}

pub(super) fn server_tool_usage_classes(tools: Option<&[urp::ToolDefinition]>) -> Vec<String> {
    let Some(tools) = tools else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tool in tools {
        let Some(class) = server_tool_usage_class(tool.tool_type.as_str()) else {
            continue;
        };
        if !out.iter().any(|existing| existing == class) {
            out.push(class.to_string());
        }
    }
    out
}

fn server_tool_usage_class(tool_type: &str) -> Option<&'static str> {
    match tool_type {
        "web_search" | "web_search_preview" | "web_fetch" => Some("web_search"),
        "file_search" | "collections_search" | "attachment_search" => Some("file_search_tool_call"),
        "x_search" => Some("x_search"),
        "code_interpreter" => Some("code_interpreter_duration"),
        "code_execution" => Some("code_execution_duration"),
        _ => None,
    }
}

pub(super) fn is_valid_embeddings_input(input: &Value) -> bool {
    if input.as_str().is_some() {
        return true;
    }
    input
        .as_array()
        .is_some_and(|arr| arr.iter().all(|item| item.as_str().is_some()))
}

pub(super) fn read_max_multiplier_from_embeddings_body(body: &Value) -> Option<Multiplier> {
    body.as_object()
        .and_then(|obj| obj.get("max_multiplier"))
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
}

pub(super) fn resolve_max_multiplier_for_embeddings(
    body: &Value,
    headers: &HeaderMap,
    auth: &crate::auth::AuthResult,
) -> Option<Multiplier> {
    let ceiling = auth.max_multiplier;
    let requested = read_max_multiplier_from_embeddings_body(body)
        .or_else(|| parse_max_multiplier_header(headers));

    match (ceiling, requested) {
        (Some(c), Some(r)) => Some(r.min(c)),
        (Some(c), None) => Some(c),
        (None, Some(r)) => Some(r),
        (None, None) => None,
    }
}

pub(super) fn effective_sse_max_frame_length(
    provider_rules: &[TransformRuleConfig],
    global_rules: &[TransformRuleConfig],
    auth_rules: &[TransformRuleConfig],
    model: &str,
) -> Option<usize> {
    resolve_sse_max_frame_length_from_rules(provider_rules, model)
        .or_else(|| resolve_sse_max_frame_length_from_rules(global_rules, model))
        .or_else(|| resolve_sse_max_frame_length_from_rules(auth_rules, model))
}

fn resolve_sse_max_frame_length_from_rules(
    rules: &[TransformRuleConfig],
    model: &str,
) -> Option<usize> {
    rules
        .iter()
        .find(|rule| {
            rule.enabled
                && rule.phase == Phase::Response
                && rule.transform == "stream_split_sse_frames"
                && match &rule.models {
                    None => true,
                    Some(patterns) => patterns
                        .iter()
                        .any(|pattern| model_glob_match(pattern, model)),
                }
        })
        .map(|rule| {
            rule.config
                .get("max_frame_length")
                .and_then(|v| v.as_u64())
                .and_then(|v| usize::try_from(v).ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_MAX_FRAME_LENGTH)
        })
}

pub(super) fn requires_buffered_response_stream(
    provider_rules: &[TransformRuleConfig],
    global_rules: &[TransformRuleConfig],
    auth_rules: &[TransformRuleConfig],
    model: &str,
    downstream: DownstreamProtocol,
) -> bool {
    provider_rules
        .iter()
        .chain(global_rules.iter())
        .chain(auth_rules.iter())
        .filter(|rule| rule.enabled && rule.phase == Phase::Response)
        .filter(|rule| match &rule.models {
            None => true,
            Some(patterns) => patterns
                .iter()
                .any(|pattern| model_glob_match(pattern, model)),
        })
        .any(|rule| {
            rule.transform == "image_markdown_to_output"
                && !matches!(downstream, DownstreamProtocol::Responses)
        })
}

pub(super) fn convert_assistant_images_to_markdown(resp: &mut urp::UrpResponse) {
    let mut pending_markdown = String::new();
    let mut last_assistant_text_idx: Option<usize> = None;

    for (i, node) in resp.output.iter().enumerate() {
        match node {
            urp::Node::Image {
                role: urp::OrdinaryRole::Assistant,
                source,
                ..
            } => {
                let md = match source {
                    ImageSource::Url { url, .. } => format!("\n\n![image]({url})"),
                    ImageSource::Base64 { media_type, data } => {
                        format!("\n\n![image](data:{media_type};base64,{data})")
                    }
                    ImageSource::FileId { .. } => String::new(),
                };
                pending_markdown.push_str(&md);
            }
            urp::Node::Text {
                role: urp::OrdinaryRole::Assistant,
                ..
            } => {
                last_assistant_text_idx = Some(i);
            }
            _ => {}
        }
    }

    if pending_markdown.is_empty() {
        return;
    }

    if let Some(idx) = last_assistant_text_idx {
        if let urp::Node::Text { content, .. } = &mut resp.output[idx] {
            content.push_str(&pending_markdown);
        }
    } else {
        resp.output.push(urp::Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
            id: None,
            role: urp::OrdinaryRole::Assistant,
            content: pending_markdown,
            phase: None,
            extra_body: std::collections::HashMap::new(),
        });
    }

    resp.output.retain(|node| {
        !matches!(
            node,
            urp::Node::Image {
                role: urp::OrdinaryRole::Assistant,
                source: ImageSource::Url { .. } | ImageSource::Base64 { .. },
                ..
            }
        )
    });
}

pub(super) fn model_glob_match(pattern: &str, model: &str) -> bool {
    crate::glob::case_sensitive_glob_match(pattern, model)
}

/// Default upstream extra_body field whitelists per provider type.
///
/// Fields that the URP request decoder already extracts into typed struct
/// fields (model, stream, temperature, etc.) are NOT in extra_body at all;
/// these lists cover only the keys that remain in `UrpRequest.extra_body`
/// and are safe to forward to the given upstream API.
const EXTRA_WHITELIST_CHAT_COMPLETION: &[&str] = &[
    "audio",
    "frequency_penalty",
    "function_call",
    "functions",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "max_completion_tokens",
    "max_tokens",
    "metadata",
    "moderation",
    "n",
    "presence_penalty",
    "prompt_cache_options",
    "safety_identifier",
    "seed",
    "service_tier",
    "stop",
    "stream_options",
    "store",
    "web_search_options",
    "parallel_tool_calls",
    "debug",
    "image_config",
    "modalities",
    "cache_control",
    "top_k",
    "top_a",
    "min_p",
    "repetition_penalty",
    "prediction",
    "prompt_cache_key",
    "prompt_cache_retention",
    "route",
    "structured_outputs",
    "verbosity",
    // OpenRouter / third-party extension fields
    "provider",
    "plugins",
    "session_id",
    "stop_server_tools_when",
    "trace",
    "thinking",
    "include_reasoning",
    "user_id",
];

const EXTRA_WHITELIST_RESPONSES: &[&str] = &[
    "background",
    "context_management",
    "conversation",
    "include",
    "instructions",
    "metadata",
    "max_tool_calls",
    "moderation",
    "parallel_tool_calls",
    "previous_response_id",
    "prompt",
    "prompt_cache_key",
    "prompt_cache_options",
    "prompt_cache_retention",
    "safety_identifier",
    "service_tier",
    "store",
    "stream_options",
    "text",
    "top_logprobs",
    "truncation",
];

const EXTRA_WHITELIST_ANTHROPIC: &[&str] = &[
    "cache_control",
    "container",
    "max_tokens",
    "metadata",
    "output_config",
    "service_tier",
    "stop_sequences",
    "top_k",
    "inference_geo",
];

const EXTRA_WHITELIST_GEMINI: &[&str] = &[
    "generationConfig",
    "safetySettings",
    "cachedContent",
    "labels",
];

const EXTRA_WHITELIST_OPENAI_IMAGE: &[&str] = &[
    "size",
    "quality",
    "style",
    "response_format",
    "n",
    "background",
    "output_format",
    "output_compression",
    "moderation",
    "user",
];

fn default_extra_whitelist(provider_type: ProviderType) -> &'static [&'static str] {
    match provider_type {
        ProviderType::ChatCompletion => EXTRA_WHITELIST_CHAT_COMPLETION,
        ProviderType::Responses => EXTRA_WHITELIST_RESPONSES,
        ProviderType::Messages => EXTRA_WHITELIST_ANTHROPIC,
        ProviderType::Gemini => EXTRA_WHITELIST_GEMINI,
        ProviderType::OpenaiImage => EXTRA_WHITELIST_OPENAI_IMAGE,
        ProviderType::OpenaiVideo | ProviderType::FalVideo => &[],
        ProviderType::Group => &[],
        // Replicate model input schemas are model-specific; whitelist is
        // handled inside the encoder by routing fields into `input`.
        ProviderType::Replicate => &["*"],
    }
}

/// Filter `req.extra_body` to only contain fields allowed by the upstream
/// provider type's whitelist, optionally extended by a provider-level override.
///
/// If `provider_override` contains `"*"`, all fields pass through unfiltered.
pub(super) fn filter_extra_body_for_provider(
    req: &mut urp::UrpRequest,
    provider_type: ProviderType,
    provider_override: &Option<Vec<String>>,
) {
    if let Some(overrides) = provider_override {
        if overrides.iter().any(|s| s == "*") {
            return;
        }
    }

    let defaults = default_extra_whitelist(provider_type);
    if defaults.contains(&"*") {
        return;
    }

    let override_set: HashSet<&str> = provider_override
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    req.extra_body.retain(|k, _| {
        k.starts_with("_monoize_")
            || defaults.contains(&k.as_str())
            || override_set.contains(k.as_str())
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderNativeToolFamily {
    Responses,
    Messages,
}

const RESPONSES_NATIVE_TOOL_TYPES: &[&str] = &[
    "file_search",
    "code_interpreter",
    "web_search",
    "web_search_preview",
    "mcp",
    "namespace",
    "tool_search",
    "programmatic_tool_calling",
    "image_generation",
    "computer",
    "computer_use_preview",
    "local_shell",
    "shell",
    "apply_patch",
];

const MESSAGES_NATIVE_TOOL_PREFIXES: &[&str] = &[
    "computer_",
    "web_search_",
    "web_fetch_",
    "code_execution_",
    "tool_search_tool_",
    "bash_",
    "text_editor_",
    "memory_",
    "advisor_",
];

const MESSAGES_NATIVE_TOOL_TYPES: &[&str] = &[
    "mcp_toolset",
    "tool_search_tool_bm25",
    "tool_search_tool_regex",
];

fn tool_wire_name(tool: &urp::ToolDefinition) -> Option<&str> {
    match tool.tool_type.as_str() {
        "function" => tool
            .function
            .as_ref()
            .map(|function| function.name.as_str()),
        "custom" => tool.custom.as_ref().map(|custom| custom.name.as_str()),
        _ => tool.name.as_deref(),
    }
}

fn collect_additional_tool_leaves(value: &Value, output: &mut Vec<Value>) {
    let Some(object) = value.as_object() else {
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("namespace") => {
            if let Some(tools) = object.get("tools").and_then(Value::as_array) {
                for tool in tools {
                    let start = output.len();
                    collect_additional_tool_leaves(tool, output);
                    for leaf in &mut output[start..] {
                        if let Some(namespace) = object.get("name").and_then(Value::as_str) {
                            leaf["namespace"] = json!(namespace);
                        }
                    }
                }
            }
        }
        Some("function" | "custom") => output.push(value.clone()),
        _ => {}
    }
}

fn responses_additional_tool_leaves(req: &urp::UrpRequest) -> Vec<Value> {
    let mut output = Vec::new();
    for node in &req.input {
        let urp::Node::ProviderItem {
            origin_protocol: urp::ProviderProtocol::Responses,
            item_type,
            body,
            ..
        } = node
        else {
            continue;
        };
        if item_type != "additional_tools" {
            continue;
        }
        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            for tool in tools {
                collect_additional_tool_leaves(tool, &mut output);
            }
        }
    }
    output
}

pub(super) fn responses_additional_tools_require_messages_buffering(req: &urp::UrpRequest) -> bool {
    responses_additional_tool_leaves(req)
        .iter()
        .filter_map(urp::decode::parse_tool_definition)
        .any(|tool| tool.tool_type == "custom" && !custom_tool_has_messages_input_schema(&tool))
}

fn custom_tool_has_messages_input_schema(tool: &urp::ToolDefinition) -> bool {
    tool.custom.as_ref().is_some_and(|custom| {
        custom.extra_body.contains_key("input_schema")
            || tool.extra_body.contains_key("input_schema")
    })
}

fn messages_custom_bridge_function(tool: urp::ToolDefinition) -> urp::ToolDefinition {
    let custom = tool
        .custom
        .expect("custom tool promotion requires a custom definition");
    urp::ToolDefinition {
        namespace: None,
        tools: None,
        origin_protocol: None,
        config: None,

        tool_type: "function".to_string(),
        name: None,
        description: None,
        function: Some(urp::FunctionDefinition {
            response_schema: None,
            name: custom.name,
            description: custom.description,
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                },
                "required": ["input"],
                "additionalProperties": false
            })),
            strict: None,
            extra_body: HashMap::new(),
        }),
        custom: None,
        extra_body: HashMap::new(),
    }
}

fn tool_call_type(tool: &urp::ToolDefinition) -> Option<urp::ToolCallType> {
    match tool.tool_type.as_str() {
        "function" => Some(urp::ToolCallType::Function),
        "custom" => Some(urp::ToolCallType::Custom),
        _ => None,
    }
}

fn active_tool_transports(
    req: &urp::UrpRequest,
) -> impl Iterator<Item = (&str, &urp::ToolTransport)> {
    req.tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|tool| {
            let name = tool_wire_name(tool)?;
            let transport = req.context.tool_transports.get(name)?;
            (tool.namespace.is_none() && tool_call_type(tool) == Some(transport.wire_type))
                .then_some((name, transport))
        })
}

fn messages_custom_bridge_names(req: &urp::UrpRequest) -> HashSet<String> {
    active_tool_transports(req)
        .filter(|(_, transport)| {
            transport.protocol == urp::ProviderProtocol::Messages
                && transport.original.tool_type == urp::ToolCallType::Custom
                && transport.wire_type == urp::ToolCallType::Function
        })
        .map(|(name, _)| name.to_owned())
        .collect()
}

fn bridge_messages_custom_history(req: &mut urp::UrpRequest) {
    let names = messages_custom_bridge_names(req);
    if names.is_empty() {
        return;
    }

    let mut bridged_call_ids = HashSet::new();
    for node in &mut req.input {
        let urp::Node::ToolCall {
            tool_type,
            call_id,
            name,
            arguments,
            ..
        } = node
        else {
            continue;
        };
        if *tool_type == urp::ToolCallType::Custom && names.contains(name) {
            *tool_type = urp::ToolCallType::Function;
            *arguments = json!({ "input": arguments.clone() }).to_string();
            bridged_call_ids.insert(call_id.clone());
        }
    }
    for node in &mut req.input {
        let urp::Node::ToolResult {
            tool_type, call_id, ..
        } = node
        else {
            continue;
        };
        if *tool_type == urp::ToolCallType::Custom && bridged_call_ids.contains(call_id) {
            *tool_type = urp::ToolCallType::Function;
        }
    }

    let Some(urp::ToolChoice::Specific(Value::Object(selector))) = req.tool_choice.as_mut() else {
        return;
    };
    if selector.get("type").and_then(Value::as_str) != Some("custom") {
        return;
    }
    let Some(name) = selector_name(selector, "custom")
        .filter(|name| names.contains(*name))
        .map(ToOwned::to_owned)
    else {
        return;
    };
    *selector = serde_json::Map::from_iter([
        ("type".to_string(), Value::String("function".to_string())),
        ("function".to_string(), json!({ "name": name })),
    ]);
}

pub(super) fn promote_responses_additional_tools(
    req: &mut urp::UrpRequest,
    provider_type: ProviderType,
) {
    let protocol = match provider_type {
        ProviderType::ChatCompletion => urp::ProviderProtocol::ChatCompletion,
        ProviderType::Messages => urp::ProviderProtocol::Messages,
        ProviderType::Gemini => urp::ProviderProtocol::Gemini,
        _ => return,
    };
    let previous_transports = std::mem::take(&mut req.context.tool_transports);

    fn append_explicit_tool(tool: urp::ToolDefinition, output: &mut Vec<urp::ToolDefinition>) {
        if tool.tool_type == "namespace" {
            let namespace = tool.name;
            for mut child in tool.tools.unwrap_or_default() {
                if child.namespace.is_none() {
                    child.namespace = namespace.clone();
                }
                append_explicit_tool(child, output);
            }
        } else {
            output.push(tool);
        }
    }
    let had_tools = req.tools.is_some();
    let mut candidates = Vec::new();
    for tool in req.tools.take().unwrap_or_default() {
        append_explicit_tool(tool, &mut candidates);
    }
    candidates.extend(
        responses_additional_tool_leaves(req)
            .iter()
            .filter_map(urp::decode::parse_tool_definition),
    );
    let mut names: HashSet<String> = candidates
        .iter()
        .filter(|tool| tool.namespace.is_none())
        .filter_map(|tool| tool_wire_name(tool).map(ToOwned::to_owned))
        .collect();
    let mut identities = HashSet::new();
    let mut promoted = Vec::new();
    for mut tool in candidates {
        if !matches!(tool.tool_type.as_str(), "function" | "custom") {
            promoted.push(tool);
            continue;
        }
        let Some(name) = tool_wire_name(&tool).map(ToOwned::to_owned) else {
            promoted.push(tool);
            continue;
        };
        let namespace = tool.namespace.take();
        let original_type = tool_call_type(&tool).expect("client tool type was checked");
        let existing_transport = previous_transports.get(&name).filter(|transport| {
            transport.protocol == protocol
                && namespace.is_none()
                && transport.wire_type == original_type
        });
        let original = existing_transport
            .map(|transport| transport.original.clone())
            .unwrap_or_else(|| urp::ToolIdentity {
                namespace: namespace.clone(),
                name: name.clone(),
                tool_type: original_type,
            });
        if !identities.insert((original.namespace.clone(), original.name.clone())) {
            continue;
        }
        if let Some(namespace) = namespace {
            let prefix: String = format!("{namespace}_{name}")
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .take(40)
                .collect();
            let mut index = promoted.len();
            let alias = loop {
                let candidate = format!("{prefix}_{index}");
                if names.insert(candidate.clone()) {
                    break candidate;
                }
                index += 1;
            };
            if let Some(function) = &mut tool.function {
                function.name = alias.clone();
            }
            if let Some(custom) = &mut tool.custom {
                custom.name = alias;
            }
        }
        if provider_type == ProviderType::Messages
            && tool.tool_type == "custom"
            && !custom_tool_has_messages_input_schema(&tool)
        {
            tool = messages_custom_bridge_function(tool);
        }
        let wire_type = tool_call_type(&tool).expect("adapted client tool type");
        if let Some(transport) = existing_transport {
            req.context.tool_transports.insert(name, transport.clone());
        } else if original.namespace.is_some() || wire_type != original_type {
            req.context.tool_transports.insert(
                tool_wire_name(&tool).expect("adapted tool name").to_owned(),
                urp::ToolTransport {
                    protocol,
                    wire_type,
                    original,
                },
            );
        }
        promoted.push(tool);
    }
    req.tools = (had_tools || !promoted.is_empty()).then_some(promoted);
    let aliases = tool_namespace_aliases(req);
    let mut call_aliases = HashMap::new();
    for node in &mut req.input {
        if let urp::Node::ToolCall {
            call_id,
            name,
            namespace,
            ..
        } = node
            && let Some(current_namespace) = namespace.as_deref()
            && let Some((alias, _)) = aliases.iter().find(|(_, identity)| {
                identity.namespace.as_deref() == Some(current_namespace) && identity.name == *name
            })
        {
            call_aliases.insert(call_id.clone(), alias.clone());
            *name = alias.clone();
            *namespace = None;
        }
    }
    for node in &mut req.input {
        if let urp::Node::ToolResult {
            call_id,
            name,
            namespace,
            ..
        } = node
        {
            let alias = match (namespace.as_deref(), name.as_deref()) {
                (Some(namespace), Some(name)) => aliases.iter().find_map(|(alias, identity)| {
                    (identity.namespace.as_deref() == Some(namespace) && identity.name == name)
                        .then_some(alias)
                }),
                (None, Some(_)) => call_aliases.get(call_id),
                _ => None,
            };
            if let Some(alias) = alias {
                *name = Some(alias.clone());
                *namespace = None;
            }
        }
    }
    if let Some(urp::ToolChoice::Specific(choice)) = &mut req.tool_choice {
        bridge_namespace_selector(choice, &aliases);
    }
    if provider_type == ProviderType::Messages {
        bridge_messages_custom_history(req);
    }
}

pub(super) fn tool_namespace_aliases(req: &urp::UrpRequest) -> HashMap<String, urp::ToolIdentity> {
    active_tool_transports(req)
        .filter(|(_, transport)| transport.original.namespace.is_some())
        .map(|(name, transport)| (name.to_owned(), transport.original.clone()))
        .collect()
}

fn bridge_namespace_selector(value: &mut Value, aliases: &HashMap<String, urp::ToolIdentity>) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let kind = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if matches!(kind.as_str(), "function" | "custom") {
        let namespace = obj
            .get("namespace")
            .or_else(|| obj.get(&kind)?.get("namespace"));
        if let Some(namespace) = namespace {
            let name = selector_name(obj, &kind);
            if let Some((alias, _)) = aliases.iter().find(|(_, identity)| {
                identity.namespace.as_deref() == namespace.as_str()
                    && Some(identity.name.as_str()) == name
            }) {
                obj.remove("namespace");
                obj.remove("name");
                obj.insert(kind, json!({"name": alias}));
            }
        }
    } else if kind == "allowed_tools" {
        let tools = if obj.contains_key("allowed_tools") {
            obj.get_mut("allowed_tools")
                .and_then(|wrapper| wrapper.get_mut("tools"))
        } else {
            obj.get_mut("tools")
        };
        if let Some(tools) = tools.and_then(Value::as_array_mut) {
            for tool in tools {
                bridge_namespace_selector(tool, aliases);
            }
        }
    }
}

fn restore_tool_namespace(
    name: &mut String,
    target_namespace: &mut Option<String>,
    aliases: &HashMap<String, urp::ToolIdentity>,
) {
    if let Some(identity) = aliases.get(name) {
        *name = identity.name.clone();
        *target_namespace = identity.namespace.clone();
    }
}

pub(super) fn restore_tool_namespace_node(
    node: &mut urp::Node,
    aliases: &HashMap<String, urp::ToolIdentity>,
) {
    if let urp::Node::ToolCall {
        name, namespace, ..
    } = node
    {
        restore_tool_namespace(name, namespace, aliases);
    }
}

pub(super) fn restore_tool_namespace_event(
    event: &mut urp::UrpStreamEvent,
    aliases: &HashMap<String, urp::ToolIdentity>,
) {
    match event {
        urp::UrpStreamEvent::NodeStart {
            header: urp::NodeHeader::ToolCall {
                name, namespace, ..
            },
            ..
        } => {
            restore_tool_namespace(name, namespace, aliases);
        }
        urp::UrpStreamEvent::NodeDone { node, .. } => restore_tool_namespace_node(node, aliases),
        urp::UrpStreamEvent::ResponseDone { output, .. } => {
            for node in output {
                restore_tool_namespace_node(node, aliases);
            }
        }
        _ => {}
    }
}

pub(super) fn restore_messages_custom_tool_calls(
    request: &urp::UrpRequest,
    response: &mut urp::UrpResponse,
) {
    let names = messages_custom_bridge_names(request);
    if names.is_empty() {
        return;
    }
    for node in &mut response.output {
        let urp::Node::ToolCall {
            tool_type,
            name,
            arguments,
            ..
        } = node
        else {
            continue;
        };
        if *tool_type != urp::ToolCallType::Function || !names.contains(name) {
            continue;
        }
        let Ok(Value::Object(object)) = serde_json::from_str::<Value>(arguments) else {
            continue;
        };
        let Some(input) = object.get("input").and_then(Value::as_str) else {
            continue;
        };
        *tool_type = urp::ToolCallType::Custom;
        *arguments = input.to_string();
    }
}

fn provider_native_tool_family(tool_type: &str) -> Option<ProviderNativeToolFamily> {
    if RESPONSES_NATIVE_TOOL_TYPES.contains(&tool_type) {
        return Some(ProviderNativeToolFamily::Responses);
    }
    if MESSAGES_NATIVE_TOOL_TYPES.contains(&tool_type)
        || MESSAGES_NATIVE_TOOL_PREFIXES
            .iter()
            .any(|prefix| tool_type.starts_with(prefix))
        || has_versioned_messages_native_tool_suffix(tool_type)
    {
        return Some(ProviderNativeToolFamily::Messages);
    }
    None
}

fn has_versioned_messages_native_tool_suffix(tool_type: &str) -> bool {
    tool_type
        .rsplit_once('_')
        .map(|(_, suffix)| suffix.len() == 8 && suffix.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or(false)
}

fn provider_supports_native_tool_family(
    provider_type: ProviderType,
    family: ProviderNativeToolFamily,
) -> bool {
    matches!(
        (provider_type, family),
        (ProviderType::Responses, ProviderNativeToolFamily::Responses)
            | (ProviderType::Messages, ProviderNativeToolFamily::Messages)
    )
}

fn provider_supports_tool_definition(
    tool: &urp::ToolDefinition,
    provider_type: ProviderType,
    downstream: DownstreamProtocol,
) -> bool {
    if tool.tool_type == "function" {
        return true;
    }

    if tool.tool_type == "custom" {
        return provider_supports_custom_tool(tool, provider_type);
    }

    if let Some(origin) = tool.origin_protocol {
        return matches!(
            (origin, provider_type),
            (urp::ProviderProtocol::Gemini, ProviderType::Gemini)
                | (urp::ProviderProtocol::Responses, ProviderType::Responses)
                | (urp::ProviderProtocol::Messages, ProviderType::Messages)
                | (
                    urp::ProviderProtocol::ChatCompletion,
                    ProviderType::ChatCompletion
                )
        );
    }

    if let Some(family) = provider_native_tool_family(&tool.tool_type) {
        return provider_supports_native_tool_family(provider_type, family);
    }

    downstream.is_same_family(provider_type)
        && matches!(
            provider_type,
            ProviderType::Responses | ProviderType::Messages
        )
}

fn provider_supports_custom_tool(tool: &urp::ToolDefinition, provider_type: ProviderType) -> bool {
    match provider_type {
        ProviderType::ChatCompletion | ProviderType::Responses => tool.custom.is_some(),
        ProviderType::Messages => tool.custom.as_ref().is_some_and(|custom| {
            custom.extra_body.contains_key("input_schema")
                || tool.extra_body.contains_key("input_schema")
        }),
        _ => false,
    }
}

fn selector_name<'a>(obj: &'a serde_json::Map<String, Value>, kind: &str) -> Option<&'a str> {
    obj.get(kind)
        .and_then(Value::as_object)
        .and_then(|nested| nested.get("name"))
        .and_then(Value::as_str)
        .or_else(|| obj.get("name").and_then(Value::as_str))
}

fn selector_matches_tool(
    selector: &serde_json::Map<String, Value>,
    tool: &urp::ToolDefinition,
) -> bool {
    if let Some(namespace) = selector.get("namespace").and_then(Value::as_str) {
        if tool.tool_type != "namespace" || tool.name.as_deref() != Some(namespace) {
            return false;
        }
        let mut leaf_selector = selector.clone();
        leaf_selector.remove("namespace");
        return tool.tools.as_ref().is_some_and(|tools| {
            tools
                .iter()
                .any(|leaf| selector_matches_tool(&leaf_selector, leaf))
        });
    }
    match selector.get("type").and_then(Value::as_str) {
        Some("function") => {
            tool.tool_type == "function"
                && selector_name(selector, "function")
                    .zip(
                        tool.function
                            .as_ref()
                            .map(|function| function.name.as_str()),
                    )
                    .is_some_and(|(selected, available)| selected == available)
        }
        Some("custom") => {
            tool.tool_type == "custom"
                && selector_name(selector, "custom")
                    .zip(tool.custom.as_ref().map(|custom| custom.name.as_str()))
                    .is_some_and(|(selected, available)| selected == available)
        }
        Some("mcp") => {
            tool.tool_type == "mcp"
                && selector
                    .get("server_label")
                    .and_then(Value::as_str)
                    .zip(
                        tool.config
                            .as_ref()
                            .and_then(|config| config.get("server_label"))
                            .or_else(|| tool.extra_body.get("server_label"))
                            .and_then(Value::as_str),
                    )
                    .is_some_and(|(selected, available)| selected == available)
        }
        Some("auto" | "required" | "any" | "none" | "allowed_tools") | None => false,
        Some(native_type) => tool.tool_type == native_type,
    }
}

fn allowed_tool_references_mut(choice: &mut urp::ToolChoice) -> Option<&mut Vec<Value>> {
    let urp::ToolChoice::Specific(Value::Object(obj)) = choice else {
        return None;
    };
    if obj.get("type").and_then(Value::as_str) != Some("allowed_tools") {
        return None;
    }
    if obj.contains_key("allowed_tools") {
        return obj
            .get_mut("allowed_tools")
            .and_then(Value::as_object_mut)
            .and_then(|allowed| allowed.get_mut("tools"))
            .and_then(Value::as_array_mut);
    }
    obj.get_mut("tools").and_then(Value::as_array_mut)
}

pub(super) fn filter_tools_for_provider(
    req: &mut urp::UrpRequest,
    provider_type: ProviderType,
    downstream: DownstreamProtocol,
) {
    let Some(tools) = req.tools.as_mut() else {
        if matches!(req.tool_choice, Some(urp::ToolChoice::Specific(_))) {
            req.tool_choice = None;
        }
        return;
    };

    tools.retain(|tool| provider_supports_tool_definition(tool, provider_type, downstream));
    if tools.is_empty() {
        req.tools = None;
        req.tool_choice = None;
        return;
    }

    let Some(choice) = req.tool_choice.as_mut() else {
        return;
    };
    let is_allowed_tools = matches!(
        choice,
        urp::ToolChoice::Specific(Value::Object(obj))
            if obj.get("type").and_then(Value::as_str) == Some("allowed_tools")
    );
    if is_allowed_tools {
        if !matches!(
            provider_type,
            ProviderType::ChatCompletion | ProviderType::Responses | ProviderType::Gemini
        ) {
            req.tool_choice = None;
            return;
        }
        let Some(references) = allowed_tool_references_mut(choice) else {
            req.tool_choice = None;
            return;
        };
        let available = req.tools.as_deref().unwrap_or_default();
        references.retain(|reference| {
            reference.as_object().is_some_and(|selector| {
                available
                    .iter()
                    .any(|tool| selector_matches_tool(selector, tool))
            })
        });
        if references.is_empty() {
            req.tool_choice = None;
        }
        return;
    }

    if let urp::ToolChoice::Specific(Value::Object(selector)) = choice
        && !matches!(
            selector.get("type").and_then(Value::as_str),
            Some("auto" | "required" | "any" | "none") | None
        )
        && !req
            .tools
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|tool| selector_matches_tool(selector, tool))
    {
        req.tool_choice = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_extra_whitelists_cover_current_protocol_extension_fields() {
        for field in [
            "audio",
            "function_call",
            "functions",
            "moderation",
            "n",
            "prompt_cache_options",
            "safety_identifier",
            "service_tier",
            "store",
            "web_search_options",
            "stop_server_tools_when",
            "thinking",
            "include_reasoning",
            "user_id",
        ] {
            assert!(
                EXTRA_WHITELIST_CHAT_COMPLETION.contains(&field),
                "missing Chat Completions field {field}"
            );
        }

        for field in ["moderation", "prompt_cache_options", "stream_options"] {
            assert!(
                EXTRA_WHITELIST_RESPONSES.contains(&field),
                "missing Responses field {field}"
            );
        }
        assert!(EXTRA_WHITELIST_RESPONSES.contains(&"prompt_cache_retention"));

        for field in ["cache_control", "container"] {
            assert!(
                EXTRA_WHITELIST_ANTHROPIC.contains(&field),
                "missing Messages field {field}"
            );
        }
        assert!(!EXTRA_WHITELIST_CHAT_COMPLETION.contains(&"models"));
        assert!(!EXTRA_WHITELIST_ANTHROPIC.contains(&"fallbacks"));
    }

    #[test]
    fn affinity_prefix_hash_matches_bounded_normalized_json() {
        let request = urp::decode::openai_responses::decode_request(&json!({
            "model": "gpt-5.6-sol",
            "input": "x".repeat(AFFINITY_PREFIX_BYTE_LIMIT * 4),
        }))
        .expect("decode request");
        let material = serde_json::to_string(
            &request
                .input
                .iter()
                .take(AFFINITY_PREFIX_NODE_LIMIT)
                .collect::<Vec<_>>(),
        )
        .expect("serialize normalized input");
        let bounded_material = &material.as_bytes()[..AFFINITY_PREFIX_BYTE_LIMIT];

        assert_eq!(
            affinity_prefix_hash(&request),
            format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bounded_material))
        );
    }

    #[test]
    fn affinity_prefix_hash_ignores_nodes_after_the_eighth() {
        let base_nodes = (0..8)
            .map(|index| {
                json!({
                    "type": "message",
                    "role": "user",
                    "content": format!("prefix-{index}"),
                })
            })
            .collect::<Vec<_>>();
        let mut first_nodes = base_nodes.clone();
        first_nodes.push(json!({
            "type": "message",
            "role": "user",
            "content": "first suffix",
        }));
        let mut second_nodes = base_nodes;
        second_nodes.push(json!({
            "type": "message",
            "role": "user",
            "content": "second suffix",
        }));
        let first = urp::decode::openai_responses::decode_request(&json!({
            "model": "gpt-5.6-sol",
            "input": first_nodes,
        }))
        .expect("decode first request");
        let second = urp::decode::openai_responses::decode_request(&json!({
            "model": "gpt-5.6-sol",
            "input": second_nodes,
        }))
        .expect("decode second request");

        assert_eq!(affinity_prefix_hash(&first), affinity_prefix_hash(&second));
    }

    #[test]
    fn provider_extra_filter_preserves_internal_adapter_state() {
        let mut request = urp::decode::openai_responses::decode_request(&json!({
            "model": "gpt-5-mini",
            "instructions": [{ "type": "input_text", "text": "policy" }],
            "input": "answer",
            "unknown_wire_field": true
        }))
        .expect("decode request");

        filter_extra_body_for_provider(&mut request, ProviderType::Responses, &None);

        assert_eq!(
            urp::encode::openai_responses::encode_request(&request, &request.model)["instructions"],
            json!([{ "type": "input_text", "text": "policy" }])
        );
        assert!(!request.extra_body.contains_key("unknown_wire_field"));
    }

    fn response_rule(transform: &str) -> TransformRuleConfig {
        TransformRuleConfig {
            transform: transform.to_string(),
            enabled: true,
            models: None,
            phase: Phase::Response,
            config: json!({}),
        }
    }

    #[test]
    fn assistant_markdown_images_to_output_stays_passthrough_for_responses() {
        assert!(!requires_buffered_response_stream(
            &[response_rule("image_markdown_to_output")],
            &[],
            &[],
            "gpt-5-mini",
            DownstreamProtocol::Responses,
        ));
    }

    #[test]
    fn assistant_markdown_images_to_output_still_buffers_for_chat_and_messages() {
        assert!(requires_buffered_response_stream(
            &[response_rule("image_markdown_to_output")],
            &[],
            &[],
            "gpt-5-mini",
            DownstreamProtocol::ChatCompletions,
        ));
        assert!(requires_buffered_response_stream(
            &[response_rule("image_markdown_to_output")],
            &[],
            &[],
            "gpt-5-mini",
            DownstreamProtocol::AnthropicMessages,
        ));
    }

    #[test]
    fn converts_assistant_image_parts_to_markdown_and_removes_images() {
        let mut resp = urp::UrpResponse {
            outcome: Default::default(),
            id: "resp_1".to_string(),
            model: "gpt-image-1".to_string(),
            created_at: None,
            output: vec![
                urp::Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
                    id: None,
                    role: urp::OrdinaryRole::Assistant,
                    content: "Here you go".to_string(),
                    phase: None,
                    extra_body: std::collections::HashMap::new(),
                },
                urp::Node::Image {
            metadata: Default::default(),
                    id: None,
                    role: urp::OrdinaryRole::Assistant,
                    source: urp::ImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: "QUJD".to_string(),
                    },
                    extra_body: std::collections::HashMap::new(),
                },
                urp::Node::Image {
            metadata: Default::default(),
                    id: None,
                    role: urp::OrdinaryRole::Assistant,
                    source: urp::ImageSource::Url {
                        url: "https://example.com/two.png".to_string(),
                        detail: None,
                    },
                    extra_body: std::collections::HashMap::new(),
                },
            ],
            finish_reason: Some(urp::FinishReason::Stop),
            usage: None,
            extra_body: std::collections::HashMap::new(),
        };

        convert_assistant_images_to_markdown(&mut resp);

        assert!(matches!(
            &resp.output[0],
            urp::Node::Text { content, .. }
            if content == "Here you go\n\n![image](data:image/png;base64,QUJD)\n\n![image](https://example.com/two.png)"
        ));
        assert_eq!(resp.output.len(), 1);
    }

    #[test]
    fn assistant_image_markdown_conversion_preserves_file_id_images() {
        let mut resp = urp::UrpResponse {
            outcome: Default::default(),
            id: "resp_1".to_string(),
            model: "gpt-image-1".to_string(),
            created_at: None,
            output: vec![
                urp::Node::Image {
            metadata: Default::default(),
                    id: None,
                    role: urp::OrdinaryRole::Assistant,
                    source: urp::ImageSource::Url {
                        url: "https://example.com/one.png".to_string(),
                        detail: None,
                    },
                    extra_body: std::collections::HashMap::new(),
                },
                urp::Node::Image {
            metadata: Default::default(),
                    id: None,
                    role: urp::OrdinaryRole::Assistant,
                    source: urp::ImageSource::FileId {
                        file_id: "file_img_1".to_string(),
                        detail: Some("high".to_string()),
                    },
                    extra_body: std::collections::HashMap::new(),
                },
            ],
            finish_reason: Some(urp::FinishReason::Stop),
            usage: None,
            extra_body: std::collections::HashMap::new(),
        };

        convert_assistant_images_to_markdown(&mut resp);

        assert!(resp.output.iter().any(|node| matches!(
            node,
            urp::Node::Image {
                source: urp::ImageSource::FileId { file_id, .. },
                ..
            } if file_id == "file_img_1"
        )));
        assert!(resp.output.iter().any(|node| matches!(
            node,
            urp::Node::Text { content, .. }
                if content == "\n\n![image](https://example.com/one.png)"
        )));
    }

    #[test]
    fn estimate_input_tokens_is_serialized_bytes_divided_by_four() {
        let req = urp::UrpRequest {
            model: "estimate-model".to_string(),
            input: vec![
                urp::Node::text(urp::OrdinaryRole::System, "You are a probe."),
                urp::Node::text(urp::OrdinaryRole::User, "hello world"),
            ],
            tools: None,
            ..urp_request_defaults()
        };
        let nodes = req
            .input
            .iter()
            .map(canonical_affinity_node)
            .collect::<Vec<_>>();
        let expected_bytes = serde_json::to_string(&nodes).expect("serializable").len() as u64;
        assert_eq!(estimate_input_tokens(&req), expected_bytes.div_ceil(4));
    }

    #[test]
    fn estimate_input_tokens_includes_tools_bytes() {
        let without_tools = urp::UrpRequest {
            model: "estimate-model".to_string(),
            input: vec![urp::Node::text(urp::OrdinaryRole::User, "hi")],
            tools: None,
            ..urp_request_defaults()
        };
        let tools = vec![urp::ToolDefinition {
            tools: Default::default(),
            config: Default::default(),
            namespace: Default::default(),
            origin_protocol: Default::default(),
            tool_type: "function".to_string(),
            name: Some("read_file".to_string()),
            description: Some("Read a file from disk.".to_string()),
            function: None,
            custom: None,
            extra_body: Default::default(),
        }];
        let with_tools = urp::UrpRequest {
            model: "estimate-model".to_string(),
            input: vec![urp::Node::text(urp::OrdinaryRole::User, "hi")],
            tools: Some(tools),
            ..urp_request_defaults()
        };
        let base = estimate_input_tokens(&without_tools);
        let with = estimate_input_tokens(&with_tools);
        assert!(
            with > base,
            "tools must add to the estimate: {with} vs {base}"
        );
    }

    #[test]
    fn estimate_input_tokens_empty_request_is_zero() {
        let req = urp::UrpRequest {
            model: "estimate-model".to_string(),
            input: Vec::new(),
            tools: None,
            ..urp_request_defaults()
        };
        // "[]" serializes to 2 bytes, so the floor is 1, not 0; a truly empty
        // serialization cannot happen because input serializes as an array.
        assert_eq!(estimate_input_tokens(&req), 1);
    }

    fn urp_request_defaults() -> urp::UrpRequest {
        urp::UrpRequest {
            context: Default::default(),
            image_generation: Default::default(),
            instructions_format: Default::default(),
            sampling: Default::default(),
            logprobs: Default::default(),
            model: String::new(),
            input: Vec::new(),
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
            extra_body: Default::default(),
        }
    }
}

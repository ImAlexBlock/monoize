use super::*;
use crate::urp::stream_decode::stream_upstream_to_urp_events;
use crate::urp::stream_encode::encode_urp_stream;
use futures_util::StreamExt;

type ForwardEventStream = futures_util::stream::Map<
    tokio_stream::wrappers::ReceiverStream<Event>,
    fn(Event) -> Result<Event, std::convert::Infallible>,
>;

fn event_ok(event: Event) -> Result<Event, std::convert::Infallible> {
    Ok(event)
}

fn receiver_event_stream(rx: mpsc::Receiver<Event>) -> ForwardEventStream {
    tokio_stream::wrappers::ReceiverStream::new(rx)
        .map(event_ok as fn(Event) -> Result<Event, std::convert::Infallible>)
}

#[cfg(test)]
#[path = "stream_terminal_tests.rs"]
mod stream_terminal_tests;

async fn emit_stream_error_if_needed(
    downstream: DownstreamProtocol,
    err: &AppError,
    tx: &mpsc::Sender<Event>,
    capture_frames: Option<&crate::request_capture::SseFrameCapture>,
) {
    if err.downstream_stream_terminal_sent {
        return;
    }
    let (event_name, body) = match downstream {
        DownstreamProtocol::Responses => (Some("error"), responses_stream_error_json(1, err)),
        DownstreamProtocol::ChatCompletions => (None, openai_error_json(err)),
        DownstreamProtocol::AnthropicMessages => (
            Some("error"),
            json!({"type": "error", "error": {"type": err.code, "message": err.message}}),
        ),
    };
    let data = body.to_string();
    let event = match event_name {
        Some(name) => Event::default().event(name).data(&data),
        None => Event::default().data(&data),
    };
    if let Some(frames) = capture_frames {
        frames
            .record(match event_name {
                Some(name) => format!("event: {name}\ndata: {data}\n\n"),
                None => format!("data: {data}\n\n"),
            })
            .await;
    }
    if tx.send(event).await.is_err() {
        return;
    }
    if matches!(
        downstream,
        DownstreamProtocol::ChatCompletions | DownstreamProtocol::Responses
    ) {
        if let Some(frames) = capture_frames {
            frames.record("data: [DONE]\n\n".to_string()).await;
        }
        let _ = tx.send(Event::default().data("[DONE]")).await;
    }
}

fn combine_stream_stage_results(results: [AppResult<()>; 4]) -> AppResult<()> {
    // Channel closure may be caused by an earlier stage; preserve its diagnostic once the
    // encoder has sent the fallback terminal, while preventing any duplicate terminal frames.
    if let Some(marked) = results.iter().filter_map(|result| result.as_ref().err())
        .find(|err| err.downstream_stream_terminal_sent) {
        if marked.code == "upstream_stream_incomplete" {
            if let Some(original) = results.iter().filter_map(|result| result.as_ref().err()).next() {
                return Err(original.clone().with_downstream_stream_terminal_sent(true));
            }
        }
        return Err(marked.clone());
    }
    for result in results { result?; }
    Ok(())
}

fn estimated_tokens_from_utf8_bytes(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

fn decoded_visible_output_bytes(output: &[urp::Node]) -> u64 {
    output.iter().fold(0u64, |total, node| {
        let bytes = match node {
            urp::Node::Text { content, .. } | urp::Node::Refusal { content, .. } => {
                content.len() as u64
            }
            _ => 0,
        };
        total.saturating_add(bytes)
    })
}

async fn retain_decoded_terminal_output(
    mut rx: mpsc::Receiver<urp::UrpStreamEvent>,
    tx: mpsc::Sender<urp::UrpStreamEvent>,
    terminal_output: Arc<Mutex<Vec<urp::Node>>>,
    aliases: HashMap<String, urp::ToolIdentity>,
) -> AppResult<()> {
    while let Some(mut event) = rx.recv().await {
        restore_tool_namespace_event(&mut event, &aliases);
        if let urp::UrpStreamEvent::ResponseDone { output, .. } = &event {
            *terminal_output.lock().await = output.clone();
        }
        let _ = tx.send(event).await;
    }
    Ok(())
}

fn stream_error_code(err: &AppError) -> String {
    err.upstream_code.as_ref().unwrap_or(&err.code).to_string()
}

fn stream_terminal_error_from_app(err: &AppError) -> StreamTerminalError {
    StreamTerminalError {
        code: stream_error_code(err),
        // SAN-9: the terminal request-log row keeps the internal detail while
        // the downstream frame carries the sanitized client message.
        message: err
            .internal_message
            .clone()
            .unwrap_or_else(|| err.message.clone()),
        http_status: err.upstream_status.unwrap_or(err.status.as_u16()),
        error_type: err
            .upstream_type
            .clone()
            .or_else(|| Some(err.error_type.clone())),
        param: err.upstream_param.clone().or_else(|| err.param.clone()),
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_stream_attempt_error(
    state: &AppState,
    auth: &crate::auth::AuthResult,
    attempt: &MonoizeAttempt,
    model: &str,
    started_at: std::time::Instant,
    request_id: Option<String>,
    request_ip: Option<String>,
    ttfb_ms: Option<u64>,
    error: &AppError,
    reasoning_effort: Option<String>,
    tried_providers: Vec<TriedProvider>,
) {
    spawn_request_log_stream_terminal_error(
        state,
        auth,
        attempt,
        model,
        started_at,
        request_id,
        request_ip,
        ttfb_ms,
        stream_terminal_error_from_app(error),
        reasoning_effort,
        tried_providers,
        None,
    );
}

fn prestream_error_stream(
    downstream: DownstreamProtocol,
    err: AppError,
    logical_model: String,
) -> ForwardEventStream {
    let (tx, rx) = mpsc::channel::<Event>(8);
    tokio::spawn(async move {
        match downstream {
            DownstreamProtocol::Responses => {
                let responses_error = responses_stream_error_json(1, &err);
                let _ = tx
                    .send(
                        Event::default()
                            .event("error")
                            .data(responses_error.to_string()),
                    )
                    .await;
                // SE1c: the bare `error` frame is not observable by a Responses client -- the
                // Codex reader has no branch for it and discards it, leaving the stream with no
                // terminal event and the client reporting a generic truncation. Emit the
                // terminal that clients act on, carrying the same code and message.
                let now = chrono::Utc::now().timestamp();
                let failed = json!({
                    "type": "response.failed",
                    "sequence_number": 2,
                    "response": {
                        "id": format!("resp_{}", uuid::Uuid::new_v4()),
                        "object": "response",
                        "created_at": now,
                        "completed_at": now,
                        "model": logical_model,
                        "status": "failed",
                        "output": [],
                        "error": {
                            "code": responses_error.get("code").cloned().unwrap_or(Value::Null),
                            "message": responses_error
                                .get("message")
                                .cloned()
                                .unwrap_or(Value::Null),
                        },
                        "incomplete_details": Value::Null,
                        "usage": Value::Null,
                        "metadata": {},
                    }
                });
                let _ = tx
                    .send(
                        Event::default()
                            .event("response.failed")
                            .data(failed.to_string()),
                    )
                    .await;
                let _ = tx.send(Event::default().data("[DONE]")).await;
            }
            DownstreamProtocol::ChatCompletions => {
                let error_json = openai_error_json(&err);
                let _ = tx.send(Event::default().data(error_json.to_string())).await;
                let _ = tx.send(Event::default().data("[DONE]")).await;
            }
            DownstreamProtocol::AnthropicMessages => {
                let code = stream_error_code(&err);
                let anthropic_error = json!({
                    "type": "error",
                    "error": {
                        "type": code,
                        "message": err.message
                    }
                });
                let _ = tx
                    .send(
                        Event::default()
                            .event("error")
                            .data(anthropic_error.to_string()),
                    )
                    .await;
            }
        }
    });
    receiver_event_stream(rx)
}

pub(super) fn deferred_forward_event_stream<F, S>(
    downstream: DownstreamProtocol,
    forwarding: F,
    downstream_gone: std::sync::Arc<std::sync::atomic::AtomicBool>,
    logical_model: String,
) -> futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>
where
    F: std::future::Future<Output = AppResult<S>> + Send + 'static,
    S: futures_util::Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Event>(64);
    tokio::spawn(async move {
        tokio::pin!(forwarding);
        let forwarding_result = tokio::select! {
            biased;
            _ = tx.closed() => {
                downstream_gone.store(true, std::sync::atomic::Ordering::Release);
                forwarding.await
            }
            result = &mut forwarding => result,
        };
        match forwarding_result {
            Ok(stream) => {
                tokio::pin!(stream);
                let mut downstream_open = true;
                loop {
                    let next = if downstream_open {
                        tokio::select! {
                            biased;
                            _ = tx.closed() => {
                                downstream_open = false;
                                downstream_gone.store(
                                    true,
                                    std::sync::atomic::Ordering::Release,
                                );
                                continue;
                            }
                            next = stream.next() => next,
                        }
                    } else {
                        stream.next().await
                    };
                    let Some(Ok(event)) = next else {
                        break;
                    };
                    if downstream_open && tx.send(event).await.is_err() {
                        downstream_open = false;
                        downstream_gone.store(true, std::sync::atomic::Ordering::Release);
                    }
                }
            }
            Err(err) => {
                let err_stream = prestream_error_stream(downstream, err, logical_model);
                tokio::pin!(err_stream);
                let mut downstream_open = true;
                loop {
                    let next = if downstream_open {
                        tokio::select! {
                            biased;
                            _ = tx.closed() => {
                                downstream_open = false;
                                downstream_gone.store(
                                    true,
                                    std::sync::atomic::Ordering::Release,
                                );
                                continue;
                            }
                            next = err_stream.next() => next,
                        }
                    } else {
                        err_stream.next().await
                    };
                    let Some(Ok(event)) = next else {
                        break;
                    };
                    if downstream_open && tx.send(event).await.is_err() {
                        downstream_open = false;
                        downstream_gone.store(true, std::sync::atomic::Ordering::Release);
                    }
                }
            }
        }
    });
    receiver_event_stream(rx).boxed()
}

pub(super) async fn forward_stream_typed(
    state: AppState,
    auth: crate::auth::AuthResult,
    mut req: urp::UrpRequest,
    max_multiplier: Option<Multiplier>,
    downstream: DownstreamProtocol,
    request_id: Option<String>,
    request_ip: Option<String>,
    client_session_id: Option<String>,
    capture: RequestCaptureContext,
    downstream_gone: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> AppResult<
    impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static,
> {
    let started_at = std::time::Instant::now();
    let mut last_failed_attempt: Option<MonoizeAttempt> = None;
    let mut tried_providers: Vec<TriedProvider> = Vec::new();
    let transform_match_model = resolve_model_suffix(&state, &mut req, auth.account_class).await?;
    // Preserve the suffix-normalized request so each per-attempt iteration can
    // re-derive the transformed request from a pristine base (see the matching
    // comment in `execute_nonstream_typed`).
    let mut original_req = req.clone();
    let logical_model = req.model.clone();
    let routing_stub = build_routing_stub(&req, max_multiplier);
    let mut attempts = build_monoize_attempts(&state, &routing_stub, &auth).await?;
    bind_media_request_routes(&mut original_req, &mut attempts)?;
    attach_client_session_id(&mut attempts, client_session_id, Some(&req));
    let funding_scope = ensure_balance_before_forward_for_attempts(
        &state,
        &auth,
        &attempts,
        Some(&req),
        request_id.as_deref(),
    )
    .await?;
    let outcome = async {
        let pending_request_log_guard = insert_pending_request_log(
            &state,
            &auth,
            &req.model,
            true,
            request_id.as_deref(),
            request_ip.as_deref(),
            started_at,
        )
        .await?;

        let mut execution_state = AttemptExecutionState::default();

        for mut attempt in attempts {
            if execution_state.should_skip(&attempt) {
                continue;
            }

        let global_transforms = state.monoize_runtime.read().await.global_transforms.clone();

        let sse_max_frame_length = effective_sse_max_frame_length(
            &attempt.provider_transforms,
            &global_transforms,
            &auth.transforms,
            &logical_model,
        );
        let requires_buffered_stream = requires_buffered_response_stream(
            &attempt.provider_transforms,
            &global_transforms,
            &auth.transforms,
            &logical_model,
            downstream,
        ) || attempt.provider_type == ProviderType::Replicate
            || (matches!(downstream, DownstreamProtocol::Responses)
                && attempt.provider_type == ProviderType::Messages
                && responses_additional_tools_require_messages_buffering(&original_req));
        let max_channel_attempts = same_channel_attempt_slots(&attempt);

        'channel_attempts: for channel_attempt in 0..max_channel_attempts {
            if execution_state.should_skip(&attempt) {
                break;
            }

            let attempt_number = execution_state.record_upstream_attempt(&attempt);
            // Clone from the pristine original request (pre-transforms) so
            // that the cross-family strip runs BEFORE provider, global, and
            // API-key transforms; see `execute_nonstream_typed`.
            let mut req_attempt = original_req.clone();
            if matches!(downstream, DownstreamProtocol::Responses) {
                promote_responses_additional_tools(&mut req_attempt, attempt.provider_type);
            }
            if let Some(target_protocol) = provider_type_protocol(attempt.provider_type) {
                urp::retain_provider_items_for_protocol(&mut req_attempt.input, target_protocol);
                if target_protocol == urp::ProviderProtocol::Responses {
                    urp::remove_downstream_only_reasoning_for_responses(&mut req_attempt.input);
                }
            }
            if attempt.strip_cross_protocol_nested_extra
                && !downstream.is_same_family(attempt.provider_type)
            {
                urp::strip_nested_extra_body(&mut req_attempt.input);
            }
            inject_monoize_context(&auth, &mut req_attempt);
            req_attempt.model = attempt.upstream_model.clone();
            // Unwrap mz2 reasoning envelopes BEFORE any request-phase transform
            // observes the request input. See `nonstream.rs` for rationale and
            // spec references (urp-transform-system PIPE-1 step 6, PIPE-1d).
            urp::filter_and_unwrap_reasoning_envelopes_for_upstream(
                &mut req_attempt.input,
                reasoning_envelope_provider_type(attempt.provider_type),
                &req_attempt.model,
                auth.reasoning_envelope_enabled,
            );
            if let Err(err) = apply_transform_rules_request(
                &state,
                &mut req_attempt,
                &attempt.provider_transforms,
                &transform_match_model,
                Some(attempt.provider_type),
            )
            .await
            {
                spawn_stream_attempt_error(
                    &state,
                    &auth,
                    &attempt,
                    &logical_model,
                    started_at,
                    request_id.clone(),
                    request_ip.clone(),
                    None,
                    &err,
                    req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                    tried_providers.clone(),
                );
                return Err(err);
            }
            if let Err(err) = apply_transform_rules_request(
                &state,
                &mut req_attempt,
                &global_transforms,
                &transform_match_model,
                Some(attempt.provider_type),
            )
            .await
            {
                spawn_stream_attempt_error(
                    &state,
                    &auth,
                    &attempt,
                    &logical_model,
                    started_at,
                    request_id.clone(),
                    request_ip.clone(),
                    None,
                    &err,
                    req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                    tried_providers.clone(),
                );
                return Err(err);
            }
            if let Err(err) = apply_transform_rules_request(
                &state,
                &mut req_attempt,
                &auth.transforms,
                &transform_match_model,
                Some(attempt.provider_type),
            )
            .await
            {
                spawn_stream_attempt_error(
                    &state,
                    &auth,
                    &attempt,
                    &logical_model,
                    started_at,
                    request_id.clone(),
                    request_ip.clone(),
                    None,
                    &err,
                    req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                    tried_providers.clone(),
                );
                return Err(err);
            }
            strip_monoize_context(&mut req_attempt);
            let capture_transform_chain = crate::request_capture::build_transform_chain(
                &attempt.provider_transforms,
                &global_transforms,
                &auth.transforms,
                &transform_match_model,
            );

            if requires_buffered_stream {
                let mut nonstream_req = req_attempt.clone();
                nonstream_req.stream = Some(false);
                let upstream_body =
                    match encode_request_for_provider(&mut nonstream_req, &attempt, downstream) {
                        Ok(body) => body,
                        Err(err) => {
                            spawn_stream_attempt_error(
                                &state,
                                &auth,
                                &attempt,
                                &logical_model,
                                started_at,
                                request_id.clone(),
                                request_ip.clone(),
                                None,
                                &err,
                                req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                                tried_providers.clone(),
                            );
                            return Err(err);
                        }
                    };
                attempt.session_affinity_value =
                    resolve_session_affinity_value(&attempt, &upstream_body);
                let provider = build_channel_provider_config(&attempt);
                let path =
                    upstream_path_for_model(attempt.provider_type, &req_attempt.model, false);
                let http = client_http_for_attempt(&state, &attempt)?;
                mark_plan_routed_before_dispatch(&funding_scope).await?;
                let call = upstream::call_upstream_with_timeout_and_headers(
                    &http,
                    &provider,
                    &attempt.api_key,
                    &path,
                    &upstream_body,
                    attempt.request_timeout_ms,
                    &attempt_extra_headers(&attempt, &upstream_body),
                )
                .await;
                match call {
                    Ok(value) => {
                        let upstream_response_model = upstream_response_model_from_json(
                            &nonstream_req.model,
                            attempt.provider_type,
                            &value,
                        );
                        if let Some(session) = capture.session.as_ref() {
                            session
                                .push_attempt(crate::request_capture::build_attempt_dump(
                                    attempt_number,
                                    &attempt.provider_id,
                                    Some(&attempt.channel_id),
                                    attempt.provider_type,
                                    &logical_model,
                                    &nonstream_req.model,
                                    &path,
                                    capture.raw_input.as_ref().clone(),
                                    &nonstream_req,
                                    upstream_body.clone(),
                                    Some(value.clone()),
                                    None,
                                    capture_transform_chain.clone(),
                                    None,
                                ))
                                .await;
                        }
                        update_pending_channel_info(
                            &state,
                            &auth,
                            &attempt,
                            &logical_model,
                            true,
                            request_id.as_deref(),
                            request_ip.as_deref(),
                            started_at,
                        )
                        .await;
                        let mut resp = match decode_response_from_provider(
                            attempt.provider_type,
                            &value,
                            &nonstream_req.model,
                            state.monoize_runtime.read().await.mask_sensitive_info,
                            &nonstream_req,
                        ) {
                            Ok(resp) => resp,
                            Err(err) => {
                                let same_channel_retryable =
                                    is_same_channel_retryable_app_error(&err);
                                let passive_failure_class = same_channel_retryable
                                    .then(|| classify_retryable_app_failure(&err));
                                record_upstream_attempt_failure(
                                    &state,
                                    &attempt,
                                    attempt_number,
                                    &err,
                                    passive_failure_class,
                                    &mut tried_providers,
                                    &mut execution_state,
                                )
                                .await;
                                last_failed_attempt = Some(attempt.clone());
                                if allow_same_channel_retry(
                                    &state,
                                    &attempt,
                                    &execution_state,
                                    channel_attempt + 1,
                                    passive_failure_class,
                                )
                                .await
                                {
                                    maybe_sleep_before_channel_retry(&attempt).await;
                                    continue 'channel_attempts;
                                }
                                break 'channel_attempts;
                            }
                        };
                        let missing_usage_substituted =
                            substitute_zero_usage_if_allowed(&mut resp.usage, &attempt);
                        if resp.usage.is_none() {
                            let err = AppError::new(
                                StatusCode::BAD_GATEWAY,
                                "upstream_usage_required",
                                "upstream response did not include billable usage",
                            );
                            let same_channel_retryable = is_same_channel_retryable_app_error(&err);
                            let passive_failure_class = same_channel_retryable
                                .then(|| classify_retryable_app_failure(&err));
                            record_upstream_attempt_failure(
                                &state,
                                &attempt,
                                attempt_number,
                                &err,
                                passive_failure_class,
                                &mut tried_providers,
                                &mut execution_state,
                            )
                            .await;
                            last_failed_attempt = Some(attempt.clone());
                            if allow_same_channel_retry(
                                &state,
                                &attempt,
                                &execution_state,
                                channel_attempt + 1,
                                passive_failure_class,
                            )
                            .await
                            {
                                maybe_sleep_before_channel_retry(&attempt).await;
                                continue 'channel_attempts;
                            }
                            break 'channel_attempts;
                        }
                        mark_channel_success(&state, &attempt).await;
                        refresh_channel_affinity(&state, &attempt).await;
                        // Wrap newly produced encrypted reasoning payloads in
                        // mz2 envelopes BEFORE response-phase transforms run.
                        // See `nonstream.rs` and PIPE-1d in
                        // spec/urp-transform-system.spec.md for rationale.
                        if auth.reasoning_envelope_enabled {
                            urp::wrap_reasoning_envelopes_in_response(
                                &mut resp,
                                reasoning_envelope_provider_type(attempt.provider_type),
                                &nonstream_req.model,
                            );
                        }
                        if let Err(err) = apply_transform_rules_response(
                            &state,
                            &mut resp,
                            &attempt.provider_transforms,
                            &logical_model,
                            Some(attempt.provider_type),
                        )
                        .await
                        {
                            if let Some(session) = capture.session.as_ref() {
                                session.persist_with_result(None, false).await;
                            }
                            spawn_stream_attempt_error(
                                &state,
                                &auth,
                                &attempt,
                                &logical_model,
                                started_at,
                                request_id.clone(),
                                request_ip.clone(),
                                None,
                                &err,
                                req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                                tried_providers.clone(),
                            );
                            return Err(err);
                        }
                        if let Err(err) = apply_transform_rules_response(
                            &state,
                            &mut resp,
                            &global_transforms,
                            &logical_model,
                            Some(attempt.provider_type),
                        )
                        .await
                        {
                            if let Some(session) = capture.session.as_ref() {
                                session.persist_with_result(None, false).await;
                            }
                            spawn_stream_attempt_error(
                                &state,
                                &auth,
                                &attempt,
                                &logical_model,
                                started_at,
                                request_id.clone(),
                                request_ip.clone(),
                                None,
                                &err,
                                req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                                tried_providers.clone(),
                            );
                            return Err(err);
                        }
                        if let Err(err) = apply_transform_rules_response(
                            &state,
                            &mut resp,
                            &auth.transforms,
                            &logical_model,
                            Some(attempt.provider_type),
                        )
                        .await
                        {
                            if let Some(session) = capture.session.as_ref() {
                                session.persist_with_result(None, false).await;
                            }
                            spawn_stream_attempt_error(
                                &state,
                                &auth,
                                &attempt,
                                &logical_model,
                                started_at,
                                request_id.clone(),
                                request_ip.clone(),
                                None,
                                &err,
                                req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                                tried_providers.clone(),
                            );
                            return Err(err);
                        }
                        if attempt.provider_type == ProviderType::OpenaiImage
                            && !matches!(downstream, DownstreamProtocol::Responses)
                        {
                            convert_assistant_images_to_markdown(&mut resp);
                        }
                        let (tx, rx) = mpsc::channel::<Event>(64);
                        let logical_model_for_stream = logical_model.clone();
                        let state_for_log = state.clone();
                        let auth_for_log = auth.clone();
                        let attempt_for_log = attempt.clone();
                        let request_id_for_log = request_id.clone();
                        let request_ip_for_log = request_ip.clone();
                        let reasoning_effort_for_log =
                            req.reasoning.as_ref().and_then(|r| r.effort.clone());
                        let tried_providers_for_log = tried_providers;
                        let capture_session = capture.session.clone();
                    let pending_request_log_guard_for_stream = pending_request_log_guard;
                        let funding_scope_for_owner = funding_scope.clone();
                        let downstream_gone_for_log = downstream_gone.clone();
                        tokio::spawn(async move {
                            let _pending_request_log_guard = pending_request_log_guard_for_stream;
                            let tx_err = tx.clone();
                            let synthetic_reasoning_duration_secs =
                                Some(started_at.elapsed().as_secs());
                            let stream_result =
                                crate::urp::stream_encode::emit_synthetic_stream_from_urp_response(
                                    downstream,
                                    &logical_model_for_stream,
                                    &resp,
                                    synthetic_reasoning_duration_secs,
                                    sse_max_frame_length,
                                    tx,
                                )
                                .await;
                            let owner_success = match stream_result {
                                Ok(()) => {
                                    let settled = match maybe_charge_response(
                                        &state_for_log,
                                        &auth_for_log,
                                        &attempt_for_log,
                                        &logical_model_for_stream,
                                        &resp,
                                        missing_usage_substituted,
                                        request_id_for_log.as_deref(),
                                    )
                                    .await
                                    {
                                        Ok(charge) => {
                                            spawn_request_log(
                                                &state_for_log,
                                                &auth_for_log,
                                                &attempt_for_log,
                                                &logical_model_for_stream,
                                                resp.usage.clone(),
                                                charge.charge_nano_usd,
                                                charge.billing_breakdown,
                                                true,
                                                started_at,
                                                request_id_for_log,
                                                request_ip_for_log,
                                                attempt_for_log.channel_id.clone(),
                                                Some(started_at.elapsed().as_millis() as u64),
                                                None,
                                                reasoning_effort_for_log,
                                                tried_providers_for_log,
                                                tx_err.is_closed()
                                                    || downstream_gone_for_log.load(
                                                        std::sync::atomic::Ordering::Acquire,
                                                    ),
                                                upstream_response_model,
                                            );
                                            true
                                        }
                                        Err(err) => {
                                            tracing::error!(
                                                code = %err.code,
                                                "failed to settle buffered stream billing: {}",
                                                err.message
                                            );
                                            spawn_request_log_stream_terminal_error(
                                                &state_for_log,
                                                &auth_for_log,
                                                &attempt_for_log,
                                                &logical_model_for_stream,
                                                started_at,
                                                request_id_for_log,
                                                request_ip_for_log,
                                                Some(started_at.elapsed().as_millis() as u64),
                                                StreamTerminalError {
                                                    code: "billing_settlement_failed".to_string(),
                                                    message: format!(
                                                        "{}: {}",
                                                        err.code, err.message
                                                    ),
                                                    http_status: err.status.as_u16(),
                                                    error_type: Some("billing_error".to_string()),
                                                    param: err.param.clone(),
                                                },
                                                reasoning_effort_for_log,
                                                tried_providers_for_log,
                                                resp.usage.clone(),
                                            );
                                            false
                                        }
                                    };
                                    if let Some(session) = capture_session.as_ref() {
                                        session
                                            .persist_with_result(resp.usage.as_ref(), false)
                                            .await;
                                    }
                                    settled
                                }
                                Err(err) => {
                                    tracing::warn!("synthetic stream failed: {}", err.message);
                                    spawn_stream_attempt_error(
                                        &state_for_log,
                                        &auth_for_log,
                                        &attempt_for_log,
                                        &logical_model_for_stream,
                                        started_at,
                                        request_id_for_log,
                                        request_ip_for_log,
                                        Some(started_at.elapsed().as_millis() as u64),
                                        &err,
                                        reasoning_effort_for_log,
                                        tried_providers_for_log,
                                    );
                                    emit_stream_error_if_needed(downstream, &err, &tx_err, None).await;
                                    if let Some(session) = capture_session.as_ref() {
                                        session.persist_with_result(None, true).await;
                                    }
                                    false
                                }
                            };
                            finish_stream_owner(&funding_scope_for_owner, owner_success).await;
                        });
                        return Ok(funded_event_stream(
                            receiver_event_stream(rx),
                            funding_scope.clone(),
                        ));
                    }
                    Err(err) => {
                        if let Some(session) = capture.session.as_ref() {
                            session
                                .push_attempt(crate::request_capture::build_attempt_dump(
                                    attempt_number,
                                    &attempt.provider_id,
                                    Some(&attempt.channel_id),
                                    attempt.provider_type,
                                    &logical_model,
                                    &nonstream_req.model,
                                    &path,
                                    capture.raw_input.as_ref().clone(),
                                    &nonstream_req,
                                    upstream_body.clone(),
                                    None,
                                    None,
                                    capture_transform_chain.clone(),
                                    Some(json!({
                                        "message": err.message,
                                        "code": err.code,
                                        "status": err.status.map(|status| status.as_u16()),
                                    })),
                                ))
                                .await;
                        }
                        let same_channel_retryable = is_same_channel_retryable_error(&err);
                        let passive_failure_class =
                            same_channel_retryable.then(|| classify_retryable_failure(&err));
                        let mask_sensitive_info =
                            state.monoize_runtime.read().await.mask_sensitive_info;
                        let app_err = upstream_error_to_app(err, mask_sensitive_info);
                        record_upstream_attempt_failure(
                            &state,
                            &attempt,
                            attempt_number,
                            &app_err,
                            passive_failure_class,
                            &mut tried_providers,
                            &mut execution_state,
                        )
                        .await;
                        last_failed_attempt = Some(attempt.clone());
                        if allow_same_channel_retry(
                            &state,
                            &attempt,
                            &execution_state,
                            channel_attempt + 1,
                            passive_failure_class,
                        )
                        .await
                        {
                            maybe_sleep_before_channel_retry(&attempt).await;
                            continue;
                        }
                        break;
                    }
                }
            }

            let upstream_body =
                match encode_request_for_provider(&mut req_attempt, &attempt, downstream) {
                    Ok(body) => body,
                    Err(err) => {
                        spawn_stream_attempt_error(
                            &state,
                            &auth,
                            &attempt,
                            &logical_model,
                            started_at,
                            request_id.clone(),
                            request_ip.clone(),
                            None,
                            &err,
                            req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                            tried_providers.clone(),
                        );
                        return Err(err);
                    }
                };
            attempt.session_affinity_value =
                resolve_session_affinity_value(&attempt, &upstream_body);
            let estimated_input_tokens = estimated_tokens_from_utf8_bytes(
                u64::try_from(upstream_body.to_string().len()).unwrap_or(u64::MAX),
            );
            let provider = build_channel_provider_config(&attempt);
            let path = upstream_path_for_model(attempt.provider_type, &req_attempt.model, true);
            let http = client_http_for_attempt(&state, &attempt)?;
            mark_plan_routed_before_dispatch(&funding_scope).await?;
            let call = upstream::call_upstream_stream_with_timeout_and_headers(
                &http,
                &provider,
                &attempt.api_key,
                &path,
                &upstream_body,
                attempt.request_timeout_ms.saturating_mul(10).max(600_000),
                &attempt_extra_headers(&attempt, &upstream_body),
            )
            .await;
            match call {
                Ok(upstream_resp) => {
                    update_pending_channel_info(
                        &state,
                        &auth,
                        &attempt,
                        &logical_model,
                        true,
                        request_id.as_deref(),
                        request_ip.as_deref(),
                        started_at,
                    )
                    .await;
                    mark_channel_success(&state, &attempt).await;
                    let legacy = match typed_request_to_legacy(&req_attempt, max_multiplier) {
                        Ok(legacy) => legacy,
                        Err(err) => {
                            spawn_stream_attempt_error(
                                &state,
                                &auth,
                                &attempt,
                                &logical_model,
                                started_at,
                                request_id.clone(),
                                request_ip.clone(),
                                None,
                                &err,
                                req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                                tried_providers.clone(),
                            );
                            return Err(err);
                        }
                    };
                    let pending_request_envelope_extra =
                        req.input.clone().into_iter().find_map(|node| match node {
                            crate::urp::Node::NextDownstreamEnvelopeExtra { extra_body }
                                if !extra_body.is_empty() =>
                            {
                                Some(extra_body)
                            }
                            _ => None,
                        });
                    let provider_type = attempt.provider_type;
                    let (tx, rx) = mpsc::channel::<Event>(64);
                    let capture_frames = capture
                        .session
                        .as_ref()
                        .map(|_| crate::request_capture::SseFrameCapture::new());
                    let runtime_metrics = Arc::new(Mutex::new(StreamRuntimeMetrics {
                        ttfb_ms: None,
                        usage: None,
                        response_id: None,
                        response_service_tier: None,
                        response_model: None,
                        response_model_terminal: false,
                        terminal: StreamTerminalDiagnostics::default(),
                        estimated_output_tokens: 0,
                        visible_output_bytes: 0,
                    }));
                    let decoded_terminal_output = Arc::new(Mutex::new(Vec::<urp::Node>::new()));
                    let metrics_for_stream = runtime_metrics.clone();
                    let state_for_log = state.clone();
                    let auth_for_log = auth.clone();
                    let attempt_for_log = attempt.clone();
                    let model_for_log = logical_model.clone();
                    let sent_model_for_log = req_attempt.model.clone();
                    let model_for_encode = logical_model.clone();
                    let model_for_transform = logical_model.clone();
                    let request_id_for_log = request_id.clone();
                    let request_ip_for_log = request_ip.clone();
                    let channel_id_for_log = attempt.channel_id.clone();
                    let capture_session = capture.session.clone();
                    let capture_raw_input = capture.raw_input.clone();
                    let capture_transform_chain_for_task = capture_transform_chain.clone();
                    let capture_req_attempt = req_attempt.clone();
                    let capture_upstream_body = upstream_body.clone();
                    let capture_path = path.clone();
                    let capture_provider_id = attempt.provider_id.clone();
                    let capture_channel_id = attempt.channel_id.clone();
                    let capture_provider_type = attempt.provider_type;
                    let transform_provider_type = attempt.provider_type;
                    let capture_upstream_model = req_attempt.model.clone();
                    let capture_logical_model = logical_model.clone();
                    let capture_attempt_number = attempt_number;
                    let capture_frames_for_task = capture_frames.clone();
                    let reasoning_effort_for_log =
                        req.reasoning.as_ref().and_then(|r| r.effort.clone());
                    let tried_providers_for_log = tried_providers.clone();
                    let (stream_idle_timeout_ms, mask_sensitive_info) = {
                        let runtime = state.monoize_runtime.read().await;
                        (
                            runtime.stream_idle_timeout_ms.max(1),
                            runtime.mask_sensitive_info,
                        )
                    };
                    let state_for_transform = state.clone();
                    let provider_rules_for_transform = attempt.provider_transforms.clone();
                    let global_rules_for_transform = global_transforms.clone();
                    let auth_rules_for_transform = auth.transforms.clone();
                    let reasoning_envelope_for_transform =
                        auth.reasoning_envelope_enabled.then(|| {
                            (
                                reasoning_envelope_provider_type(attempt.provider_type).to_string(),
                                req_attempt.model.clone(),
                            )
                        });
                    let namespace_aliases = tool_namespace_aliases(&req_attempt);
                    let pending_request_log_guard_for_stream = pending_request_log_guard;
                    let funding_scope_for_owner = funding_scope.clone();
                    let downstream_gone_for_log = downstream_gone.clone();
                    tokio::spawn(async move {
                        let _pending_request_log_guard = pending_request_log_guard_for_stream;
                        let tx_err = tx.clone();
                        let stream_future = async {
                            let (decoded_tx, decoded_rx) =
                                mpsc::channel::<crate::urp::UrpStreamEvent>(64);
                            let (metered_tx, metered_rx) =
                                mpsc::channel::<crate::urp::UrpStreamEvent>(64);
                            let (transformed_tx, transformed_rx) =
                                mpsc::channel::<crate::urp::UrpStreamEvent>(64);

                            let decode_handle = {
                                let metrics = metrics_for_stream.clone();
                                crate::request_capture::spawn_with_sse_capture(async move {
                                    stream_upstream_to_urp_events(
                                        &legacy,
                                        pending_request_envelope_extra,
                                        provider_type,
                                        upstream_resp,
                                        decoded_tx,
                                        Some(started_at),
                                        Some(metrics),
                                        stream_idle_timeout_ms,
                                    )
                                    .await
                                })
                            };

                            let retain_output_handle = {
                                let terminal_output = decoded_terminal_output.clone();
                                crate::request_capture::spawn_with_sse_capture(async move {
                                    retain_decoded_terminal_output(
                                        decoded_rx,
                                        metered_tx,
                                        terminal_output,
                                        namespace_aliases,                                    )
                                    .await
                                })
                            };

                            let transform_handle =
                                crate::request_capture::spawn_with_sse_capture(async move {
                                    let reasoning_envelope = reasoning_envelope_for_transform
                                        .as_ref()
                                        .map(|(provider_type, upstream_model)| {
                                            (provider_type.as_str(), upstream_model.as_str())
                                        });
                                    transform_urp_stream(
                                        &state_for_transform,
                                        metered_rx,
                                        transformed_tx,
                                        &provider_rules_for_transform,
                                        &global_rules_for_transform,
                                        &auth_rules_for_transform,
                                        &model_for_transform,
                                        Some(transform_provider_type),
                                        reasoning_envelope,
                                    )
                                    .await
                                });

                            let encode_handle =
                                crate::request_capture::spawn_with_sse_capture(async move {
                                    encode_urp_stream(
                                        downstream,
                                        transformed_rx,
                                        tx,
                                        &model_for_encode,
                                        started_at,
                                        sse_max_frame_length,
                                        mask_sensitive_info,
                                    )
                                    .await
                                });

                            let (
                                decode_result,
                                retain_output_result,
                                transform_result,
                                encode_result,
                            ) = tokio::join!(
                                decode_handle,
                                retain_output_handle,
                                transform_handle,
                                encode_handle
                            );
                            combine_stream_stage_results([
                                decode_result.unwrap_or_else(|e| Err(AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "task_panic", e.to_string()))),
                                retain_output_result.unwrap_or_else(|e| Err(AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "task_panic", e.to_string()))),
                                transform_result.unwrap_or_else(|e| Err(AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "task_panic", e.to_string()))),
                                encode_result.unwrap_or_else(|e| Err(AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "task_panic", e.to_string()))),
                            ])
                        };
                        let stream_result = if let Some(frames) = capture_frames_for_task.clone() {
                            crate::request_capture::with_sse_capture(frames, stream_future).await
                        } else {
                            stream_future.await
                        };
                        let settled_output = decoded_terminal_output.lock().await.clone();
                        let terminal_visible_output_bytes =
                            decoded_visible_output_bytes(&settled_output);

                        let (
                            ttfb_ms,
                            actual_upstream_usage,
                            usage,
                            is_estimated,
                            missing_usage_substituted,
                            terminal_diagnostics,
                            response_id,
                            response_service_tier,
                            response_model,
                        ) = {
                            let guard = runtime_metrics.lock().await;
                            let actual_upstream_usage = guard.usage.clone();
                            let (usage, is_estimated, missing_usage_substituted) = match guard
                                .usage
                                .clone()
                            {
                                Some(u) => (Some(u), false, false),
                                None if attempt_for_log.allow_missing_usage => {
                                    (Some(urp::Usage::default()), false, true)
                                }
                                None => {
                                    let visible_output_bytes = guard
                                        .visible_output_bytes
                                        .max(terminal_visible_output_bytes);
                                    let estimated_output_tokens =
                                        estimated_tokens_from_utf8_bytes(visible_output_bytes);
                                    tracing::warn!(
                                        estimated_input_tokens,
                                        estimated_output_tokens,
                                        "upstream stream ended without usage; billing from estimate"
                                    );
                                    (
                                        Some(urp::Usage {
            iterations: Default::default(),
                                            input_tokens: estimated_input_tokens,
                                            output_tokens: estimated_output_tokens,
                                            input_details: None,
                                            output_details: None,
                                            extra_body: std::collections::HashMap::new(),
                                        }),
                                        true,
                                        false,
                                    )
                                }
                            };
                            (
                                guard.ttfb_ms,
                                actual_upstream_usage,
                                usage,
                                is_estimated,
                                missing_usage_substituted,
                                guard.terminal.clone(),
                                guard.response_id.clone(),
                                guard.response_service_tier.clone(),
                                guard.response_model.clone(),
                            )
                        };

                        if let Some(terminal_error) = terminal_diagnostics.terminal_error.clone() {
                            if let Some(failure_class) = midstream_terminal_failure_class(
                                terminal_error.http_status,
                                Some(&terminal_error.code),
                                terminal_error.error_type.as_deref(),
                            ) {
                                record_midstream_terminal_failure(
                                    &state_for_log,
                                    &attempt_for_log,
                                    failure_class,
                                )
                                .await;
                            }
                            spawn_request_log_stream_terminal_error(
                                &state_for_log,
                                &auth_for_log,
                                &attempt_for_log,
                                &model_for_log,
                                started_at,
                                request_id_for_log,
                                request_ip_for_log,
                                ttfb_ms,
                                terminal_error,
                                reasoning_effort_for_log,
                                tried_providers_for_log,
                                actual_upstream_usage.clone(),
                            );
                            if let Some(session) = capture_session.as_ref() {
                                let frames = if let Some(frames) = capture_frames_for_task.as_ref()
                                {
                                    Some(frames.snapshot().await)
                                } else {
                                    None
                                };
                                let error_json =
                                    terminal_diagnostics.terminal_error.as_ref().map(|err| {
                                        json!({
                                            "message": err.message,
                                            "code": err.code,
                                            "status": err.http_status,
                                        })
                                    });
                                session
                                    .push_attempt(crate::request_capture::build_attempt_dump(
                                        capture_attempt_number,
                                        &capture_provider_id,
                                        Some(&capture_channel_id),
                                        capture_provider_type,
                                        &capture_logical_model,
                                        &capture_upstream_model,
                                        &capture_path,
                                        capture_raw_input.as_ref().clone(),
                                        &capture_req_attempt,
                                        capture_upstream_body,
                                        None,
                                        frames,
                                        capture_transform_chain_for_task,
                                        error_json,
                                    ))
                                    .await;
                                session
                                    .persist_with_result(actual_upstream_usage.as_ref(), true)
                                    .await;
                            }
                            finish_stream_owner(&funding_scope_for_owner, false).await;
                            return;
                        }

                        if let Err(ref err) = stream_result {
                            tracing::warn!("stream passthrough adapter failed: {}", err.message);
                            if is_upstream_adapter_failure(err) {
                                record_midstream_terminal_failure(
                                    &state_for_log,
                                    &attempt_for_log,
                                    RetryableFailureClass::Transient,
                                )
                                .await;
                            }
                            spawn_stream_attempt_error(
                                &state_for_log,
                                &auth_for_log,
                                &attempt_for_log,
                                &model_for_log,
                                started_at,
                                request_id_for_log,
                                request_ip_for_log,
                                ttfb_ms,
                                err,
                                reasoning_effort_for_log,
                                tried_providers_for_log,
                            );

                            emit_stream_error_if_needed(downstream, err, &tx_err, capture_frames_for_task.as_ref()).await;
                            if let Some(session) = capture_session.as_ref() {
                                let frames = if let Some(frames) = capture_frames_for_task.as_ref()
                                {
                                    Some(frames.snapshot().await)
                                } else {
                                    None
                                };
                                session
                                    .push_attempt(crate::request_capture::build_attempt_dump(
                                        capture_attempt_number,
                                        &capture_provider_id,
                                        Some(&capture_channel_id),
                                        capture_provider_type,
                                        &capture_logical_model,
                                        &capture_upstream_model,
                                        &capture_path,
                                        capture_raw_input.as_ref().clone(),
                                        &capture_req_attempt,
                                        capture_upstream_body,
                                        None,
                                        frames,
                                        capture_transform_chain_for_task,
                                        Some(json!({
                                            "message": err.message,
                                            "code": err.code,
                                            "status": err.status.as_u16(),
                                        })),
                                    ))
                                    .await;
                                session
                                    .persist_with_result(actual_upstream_usage.as_ref(), true)
                                    .await;
                            }
                            finish_stream_owner(&funding_scope_for_owner, false).await;
                            return;
                        }

                        let usage_row = usage
                            .as_ref()
                            .expect("stream usage or a deterministic estimate must exist");
                        let mut charge = match maybe_charge_stream_usage(
                            &state_for_log,
                            &auth_for_log,
                            &attempt_for_log,
                            &model_for_log,
                            usage_row,
                            missing_usage_substituted,
                            &settled_output,
                            response_service_tier.as_deref(),
                            request_id_for_log.as_deref(),
                        )
                        .await
                        {
                            Ok(value) => value,
                            Err(err) => {
                                tracing::error!(
                                    code = %err.code,
                                    "failed to settle passthrough stream billing: {}",
                                    err.message
                                );
                                let terminal_error = StreamTerminalError {
                                    code: "billing_settlement_failed".to_string(),
                                    message: format!("{}: {}", err.code, err.message),
                                    http_status: err.status.as_u16(),
                                    error_type: Some("billing_error".to_string()),
                                    param: err.param.clone(),
                                };
                                spawn_request_log_stream_terminal_error(
                                    &state_for_log,
                                    &auth_for_log,
                                    &attempt_for_log,
                                    &model_for_log,
                                    started_at,
                                    request_id_for_log,
                                    request_ip_for_log,
                                    ttfb_ms,
                                    terminal_error,
                                    reasoning_effort_for_log,
                                    tried_providers_for_log,
                                    usage.clone(),
                                );
                                if let Some(session) = capture_session.as_ref() {
                                    session.persist_with_result(usage.as_ref(), true).await;
                                }
                                finish_stream_owner(&funding_scope_for_owner, false).await;
                                return;
                            }
                        };
                        if is_estimated {
                            if let Some(ref mut breakdown) = charge.billing_breakdown {
                                if let Some(obj) = breakdown.as_object_mut() {
                                    obj.insert(
                                        "estimated".to_string(),
                                        serde_json::Value::Bool(true),
                                    );
                                }
                            }
                        }

                        refresh_channel_affinity(&state_for_log, &attempt_for_log).await;
                        if attempt_for_log.provider_type == ProviderType::Responses
                            && let Some(response_id) = response_id.as_deref()
                        {
                            refresh_response_id_affinity(
                                &state_for_log,
                                &auth_for_log,
                                &model_for_log,
                                response_id,
                                &attempt_for_log,
                            )
                            .await;
                        }

                        spawn_request_log(
                            &state_for_log,
                            &auth_for_log,
                            &attempt_for_log,
                            &model_for_log,
                            usage,
                            charge.charge_nano_usd,
                            charge.billing_breakdown,
                            true,
                            started_at,
                            request_id_for_log,
                            request_ip_for_log,
                            channel_id_for_log,
                            ttfb_ms,
                            Some(terminal_diagnostics),
                            reasoning_effort_for_log,
                            tried_providers_for_log,
                            tx_err.is_closed()
                                || downstream_gone_for_log.load(
                                    std::sync::atomic::Ordering::Acquire,
                                ),
                            response_model.as_deref().and_then(|observed| {
                                mismatched_upstream_response_model(&sent_model_for_log, observed)
                            }),
                        );

                        if let Some(session) = capture_session.as_ref() {
                            let frames = if let Some(frames) = capture_frames_for_task.as_ref() {
                                Some(frames.snapshot().await)
                            } else {
                                None
                            };
                            session
                                .push_attempt(crate::request_capture::build_attempt_dump(
                                    capture_attempt_number,
                                    &capture_provider_id,
                                    Some(&capture_channel_id),
                                    capture_provider_type,
                                    &capture_logical_model,
                                    &capture_upstream_model,
                                    &capture_path,
                                    capture_raw_input.as_ref().clone(),
                                    &capture_req_attempt,
                                    capture_upstream_body,
                                    None,
                                    frames,
                                    capture_transform_chain_for_task,
                                    None,
                                ))
                                .await;
                            session
                                .persist_with_result(actual_upstream_usage.as_ref(), false)
                                .await;
                        }
                        finish_stream_owner(&funding_scope_for_owner, true).await;
                    });
                    return Ok(funded_event_stream(
                        receiver_event_stream(rx),
                        funding_scope.clone(),
                    ));
                }
                Err(err) => {
                    if let Some(session) = capture.session.as_ref() {
                        session
                            .push_attempt(crate::request_capture::build_attempt_dump(
                                attempt_number,
                                &attempt.provider_id,
                                Some(&attempt.channel_id),
                                attempt.provider_type,
                                &logical_model,
                                &req_attempt.model,
                                &path,
                                capture.raw_input.as_ref().clone(),
                                &req_attempt,
                                upstream_body.clone(),
                                None,
                                None,
                                capture_transform_chain.clone(),
                                Some(json!({
                                    "message": err.message,
                                    "code": err.code,
                                    "status": err.status.map(|status| status.as_u16()),
                                })),
                            ))
                            .await;
                    }
                    let same_channel_retryable = is_same_channel_retryable_error(&err);
                    let passive_failure_class =
                        same_channel_retryable.then(|| classify_retryable_failure(&err));
                    let mask_sensitive_info =
                        state.monoize_runtime.read().await.mask_sensitive_info;
                    let app_err = upstream_error_to_app(err, mask_sensitive_info);
                    record_upstream_attempt_failure(
                        &state,
                        &attempt,
                        attempt_number,
                        &app_err,
                        passive_failure_class,
                        &mut tried_providers,
                        &mut execution_state,
                    )
                    .await;
                    last_failed_attempt = Some(attempt.clone());
                    if allow_same_channel_retry(
                        &state,
                        &attempt,
                        &execution_state,
                        channel_attempt + 1,
                        passive_failure_class,
                    )
                    .await
                    {
                        maybe_sleep_before_channel_retry(&attempt).await;
                        continue;
                    }
                    break;
                }
            }
        }
    }
        let final_err = if tried_providers.is_empty() {
            no_attempt_error(&state, &logical_model, &auth).await
        } else {
            build_exhausted_upstream_error(&logical_model, &tried_providers)
        };
        if let Some(attempt) = last_failed_attempt {
            let terminal_error = stream_terminal_error_from_app(&final_err);
            spawn_request_log_stream_terminal_error(
                &state,
                &auth,
                &attempt,
                &logical_model,
                started_at,
                request_id,
                request_ip,
                None,
                terminal_error,
                req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                tried_providers,
                None,
            );
        } else {
            spawn_request_log_error_no_attempt(
                &state,
                &auth,
                &logical_model,
                true,
                started_at,
                request_id,
                request_ip,
                &final_err,
                req.reasoning.as_ref().and_then(|r| r.effort.clone()),
                tried_providers,
            );
        }
        if let Some(session) = capture.session.as_ref() {
            session.persist_with_result(None, true).await;
        }
        Err(final_err)
    }
    .await;

    finish_stream_forwarding_outcome(funding_scope, outcome).await
}

pub(super) async fn finish_stream_forwarding_outcome<T>(
    funding_scope: AdmittedFundingScope,
    outcome: AppResult<T>,
) -> AppResult<T> {
    match outcome {
        Ok(value) => Ok(value),
        Err(error) => funding_scope.finish(Err(error)).await,
    }
}

async fn finish_stream_owner(funding_scope: &AdmittedFundingScope, success: bool) {
    let outcome = if success {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "stream_terminal_failed",
            "stream owner did not complete billing",
        ))
    };
    if let Err(error) = funding_scope.clone().finish(outcome).await
        && success
    {
        tracing::error!(
            code = %error.code,
            message = %error.message,
            "plan-funded stream owner finalization failed"
        );
    }
}

fn funded_event_stream<S>(
    stream: S,
    funding_scope: AdmittedFundingScope,
) -> impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static
where
    S: futures_util::Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static,
{
    struct State<S> {
        stream: std::pin::Pin<Box<S>>,
        funding_scope: Option<AdmittedFundingScope>,
    }

    futures_util::stream::unfold(
        State {
            stream: Box::pin(stream),
            funding_scope: Some(funding_scope),
        },
        |mut state| async move {
            match futures_util::StreamExt::next(&mut state.stream).await {
                Some(item) => Some((item, state)),
                None => {
                    if let Some(scope) = state.funding_scope.take() {
                        if let Err(error) = scope.finish(Ok(())).await {
                            tracing::error!(
                                code = error.code,
                                message = %error.message,
                                "plan-funded stream finalization failed"
                            );
                        }
                    }
                    None
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_forwarding_allows_sse_keep_alive_before_upstream_headers() {
        let forwarding = std::future::pending::<AppResult<ForwardEventStream>>();
        let stream = deferred_forward_event_stream(
            DownstreamProtocol::Responses,
            forwarding,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            "test-model".to_string(),
        );
        let response = Sse::new(stream)
            .keep_alive(
                KeepAlive::new()
                    .interval(std::time::Duration::from_millis(1))
                    .text("heartbeat"),
            )
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let mut body = response.into_body().into_data_stream();
        let chunk = tokio::time::timeout(std::time::Duration::from_millis(100), body.next())
            .await
            .expect("keep-alive must not wait for upstream headers")
            .expect("SSE body remains open")
            .expect("SSE keep-alive frame is valid");
        assert_eq!(chunk.as_ref(), b": heartbeat\n\n");
    }

    #[tokio::test]
    async fn deferred_stream_drains_inner_stream_after_client_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let polled = std::sync::Arc::new(AtomicUsize::new(0));
        let observed = polled.clone();
        let inner = futures_util::stream::iter((0..100).map(move |index| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Event::default().data(index.to_string()))
        }));
        let mut stream = deferred_forward_event_stream(
            DownstreamProtocol::Responses,
            async move { Ok::<_, AppError>(inner) },
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            "test-model".to_string(),
        );
        assert!(stream.next().await.is_some());
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while polled.load(Ordering::SeqCst) != 100 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn deferred_stream_marks_disconnect_without_waiting_for_another_event() {
        let downstream_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let inner = futures_util::stream::once(async {
            Ok::<_, std::convert::Infallible>(Event::default().data("first"))
        })
        .chain(futures_util::stream::pending());
        let mut stream = deferred_forward_event_stream(
            DownstreamProtocol::Responses,
            async move { Ok::<_, AppError>(inner) },
            downstream_gone.clone(),
            "test-model".to_string(),
        );
        assert!(stream.next().await.is_some());
        drop(stream);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !downstream_gone.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("outer receiver closure must be observed while the inner stream is idle");
    }

    #[tokio::test]
    async fn deferred_stream_marks_disconnect_while_forwarding_is_pending() {
        let downstream_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let stream = deferred_forward_event_stream(
            DownstreamProtocol::Responses,
            async move {
                let _ = gate_rx.await;
                Ok::<_, AppError>(futures_util::stream::empty())
            },
            downstream_gone.clone(),
            "test-model".to_string(),
        );
        drop(stream);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !downstream_gone.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("outer receiver closure must be observed before forwarding completes");
        let _ = gate_tx.send(());
    }

    #[tokio::test]
    async fn decoded_terminal_output_is_retained_before_forwarding() {
        let (input_tx, input_rx) = mpsc::channel(1);
        let (output_tx, mut output_rx) = mpsc::channel(1);
        let retained = Arc::new(Mutex::new(Vec::new()));
        let relay = tokio::spawn(retain_decoded_terminal_output(
            input_rx,
            output_tx,
            retained.clone(),
            HashMap::new(),        ));
        let nodes = vec![urp::Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),
            id: None,
            role: urp::OrdinaryRole::Assistant,
            content: "拒绝".to_string(),
            phase: None,
            extra_body: HashMap::new(),
        }];

        input_tx
            .send(urp::UrpStreamEvent::ResponseDone {
            outcome: Default::default(),
                finish_reason: None,
                usage: None,
                output: nodes.clone(),
                extra_body: HashMap::new(),
            })
            .await
            .expect("relay input should stay open");
        drop(input_tx);

        assert!(matches!(
            output_rx.recv().await,
            Some(urp::UrpStreamEvent::ResponseDone { .. })
        ));
        relay.await.expect("relay task should join").expect("relay");
        assert_eq!(*retained.lock().await, nodes);
        assert_eq!(decoded_visible_output_bytes(&nodes), 6);
        assert_eq!(estimated_tokens_from_utf8_bytes(6), 2);
    }
}

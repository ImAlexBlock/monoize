use super::*;
use axum::response::IntoResponse;

const TARGETS: [DownstreamProtocol; 3] = [
    DownstreamProtocol::Responses,
    DownstreamProtocol::ChatCompletions,
    DownstreamProtocol::AnthropicMessages,
];

fn media_response() -> urp::UrpResponse {
    let mut response = urp::decode::openai_responses::decode_response(&json!({
        "id": "resp_media", "object": "response", "model": "test-model", "status": "completed",
        "output": [{"type": "message", "role": "assistant", "content": [
            {"type": "output_image", "url": "https://example.com/image.png"}
        ]}]
    }))
    .unwrap();
    if let urp::Node::Image { source, .. } = &mut response.output[0] {
        *source = urp::ImageSource::FileId { file_id: "private-file".into(), detail: None };
    }
    response
}

async fn collect_wire(mut rx: mpsc::Receiver<Event>) -> String {
    let mut frames = Vec::new();
    while let Some(event) = rx.recv().await {
        frames.push(Ok::<_, std::convert::Infallible>(event));
    }
    let body = axum::response::Sse::new(futures_util::stream::iter(frames))
        .into_response()
        .into_body();
    String::from_utf8(
        axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn assert_single_terminal_error(wire: &str, downstream: DownstreamProtocol) {
    let data: Vec<&str> = wire
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect();
    let errors: Vec<usize> = data
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let value: Value = serde_json::from_str(value).ok()?;
            (value.get("error").is_some()
                || value["type"] == "error"
                || value["type"] == "response.failed")
                .then_some(index)
        })
        .collect();
    assert_eq!(errors.len(), 1, "{wire}");
    let has_done = !matches!(downstream, DownstreamProtocol::AnthropicMessages);
    assert_eq!(
        data.iter().filter(|data| **data == "[DONE]").count(),
        usize::from(has_done),
        "{wire}"
    );
    assert_eq!(
        errors[0] + 1 + usize::from(has_done),
        data.len(),
        "frames after terminal error: {wire}"
    );
    if has_done {
        assert_eq!(data.last(), Some(&"[DONE]"));
    }
    assert!(
        !wire.contains("response.completed") && !wire.contains("message_stop"),
        "{wire}"
    );
    assert!(!wire.contains("downstream_stream_terminal_sent"));
}

async fn exercise_media_failure(synthetic: bool) {
    for downstream in TARGETS {
        let response = media_response();
        let (tx, rx) = mpsc::channel(1024);
        let result = if synthetic {
            urp::stream_encode::emit_synthetic_stream_from_urp_response(
                downstream,
                "test-model",
                &response,
                None,
                None,
                tx.clone(),
            )
            .await
        } else {
            let (event_tx, event_rx) = mpsc::channel(16);
            event_tx
                .send(urp::UrpStreamEvent::NodeDone {
                    node_index: 0,
                    node: response.output[0].clone(),
                    usage: None,
                    extra_body: HashMap::new(),
                })
                .await
                .unwrap();
            event_tx
                .send(urp::UrpStreamEvent::ResponseDone {
            outcome: Default::default(),
                    finish_reason: response.finish_reason,
                    usage: response.usage.clone(),
                    output: response.output.clone(),
                    extra_body: HashMap::new(),
                })
                .await
                .unwrap();
            drop(event_tx);
            encode_urp_stream(
                downstream,
                event_rx,
                tx.clone(),
                "test-model",
                Instant::now(),
                None,
                false,
            )
            .await
        };
        let err = result.unwrap_err();
        assert!(err.downstream_stream_terminal_sent);
        let original_code = err.code.clone();
        let original_message = err.message.clone();
        let err = combine_stream_stage_results([
            Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                "stream_transform_failed",
                "receiver closed",
            )),
            Ok(()),
            Ok(()),
            Err(err),
        ])
        .unwrap_err();
        assert_eq!(err.code, original_code);
        assert_eq!(err.message, original_message);
        assert!(err.downstream_stream_terminal_sent);
        let capture = crate::request_capture::SseFrameCapture::new();
        let capture_before = format!("{:?}", capture.snapshot().await);
        emit_stream_error_if_needed(downstream, &err, &tx, Some(&capture)).await;
        assert_eq!(format!("{:?}", capture.snapshot().await), capture_before);
        drop(tx);
        assert_single_terminal_error(&collect_wire(rx).await, downstream);
    }
}

#[tokio::test]
async fn synthetic_media_failure_wrapper_does_not_repeat_terminal_frames() {
    exercise_media_failure(true).await;
}

#[tokio::test]
async fn live_media_failure_wrapper_preserves_encoder_error_and_terminal_frames() {
    exercise_media_failure(false).await;
}

#[tokio::test]
async fn unmarked_transport_failure_emits_one_target_error_sequence() {
    for downstream in TARGETS {
        let err = AppError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_transport_failed",
            "connection interrupted",
        );
        assert!(!err.downstream_stream_terminal_sent);
        let (tx, rx) = mpsc::channel(16);
        let capture = crate::request_capture::SseFrameCapture::new();
        emit_stream_error_if_needed(downstream, &err, &tx, Some(&capture)).await;
        drop(tx);
        let wire = collect_wire(rx).await;
        assert_single_terminal_error(&wire, downstream);
        let expected_code = match downstream {
            DownstreamProtocol::Responses => "server_error",
            _ => "upstream_transport_failed",
        };
        assert!(wire.contains(expected_code) && wire.contains("connection interrupted"));
    }
}

#[test]
fn ordinary_stage_errors_keep_the_original_stage_order() {
    let err = combine_stream_stage_results([
        Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "decode_failed",
            "decode failed",
        )),
        Ok(()),
        Ok(()),
        Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "encode_failed",
            "encode failed",
        )),
    ])
    .unwrap_err();
    assert_eq!(err.code, "decode_failed");
    assert!(!err.downstream_stream_terminal_sent);
    assert!(combine_stream_stage_results([Ok(()), Ok(()), Ok(()), Ok(())]).is_ok());
}

#[tokio::test]
async fn failed_terminal_send_is_not_marked_as_sent() {
    for downstream in TARGETS {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let err = urp::stream_encode::emit_synthetic_stream_from_urp_response(
            downstream,
            "test-model",
            &media_response(),
            None,
            None,
            tx,
        )
        .await
        .unwrap_err();
        assert!(!err.downstream_stream_terminal_sent);
    }
}

#[tokio::test]
async fn gemini_stream_failures_mark_the_emitted_terminal_error() {
    for conflicting_terminal in [false, true] {
        let (event_tx, event_rx) = mpsc::channel(16);
        if conflicting_terminal {
            event_tx
                .send(urp::UrpStreamEvent::NodeDone {
                    node_index: 0,
                    node: serde_json::from_value(
                        json!({"type":"text","role":"assistant","content":"before"}),
                    )
                    .unwrap(),
                    usage: None,
                    extra_body: HashMap::new(),
                })
                .await
                .unwrap();
            event_tx
                .send(urp::UrpStreamEvent::ResponseDone {
            outcome: Default::default(),
                    finish_reason: Some(urp::FinishReason::Stop),
                    usage: None,
                    output: vec![],
                    extra_body: HashMap::new(),
                })
                .await
                .unwrap();
        }
        drop(event_tx);
        let (tx, rx) = mpsc::channel(16);
        let err =
            urp::stream_encode::gemini::encode_urp_stream_as_gemini(event_rx, tx, "test-model")
                .await
                .unwrap_err();
        assert!(err.downstream_stream_terminal_sent);
        assert_eq!(err.code, "stream_encode_failed");
        assert_eq!(
            err.message,
            if conflicting_terminal {
                "Gemini cannot delete a Part that was already emitted"
            } else {
                "Gemini stream has no canonical terminal event"
            }
        );
        let wire = collect_wire(rx).await;
        let values: Vec<Value> = wire
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        assert_eq!(
            values
                .iter()
                .filter(|value| value.get("error").is_some())
                .count(),
            1
        );
        assert!(values.last().unwrap().get("error").is_some());
        assert!(!wire.contains("[DONE]") && !wire.contains("downstream_stream_terminal_sent"));
    }
}

#[test]
fn incomplete_encoder_terminal_preserves_original_transport_error_without_repeating_it() {
    let result = combine_stream_stage_results([
        Err(AppError::new(StatusCode::BAD_GATEWAY, "upstream_transport_failed", "connection ended")),
        Ok(()),
        Ok(()),
        Err(AppError::new(StatusCode::BAD_GATEWAY, "upstream_stream_incomplete", "missing terminal")
            .with_downstream_stream_terminal_sent(true)),
    ]).unwrap_err();
    assert_eq!(result.code, "upstream_transport_failed");
    assert_eq!(result.message, "connection ended");
    assert!(result.downstream_stream_terminal_sent);
}

#[tokio::test]
async fn upstream_sse_error_status_preserves_explicit_values_and_classifies_server_failures() {
    let cases = [
        (json!({"code":"server_is_overloaded","type":"service_unavailable_error"}), None, 503, 503),
        (json!({"code":"provider_failure","type":" SERVICE_UNAVAILABLE_ERROR "}), None, 503, 503),
        (json!({"code":" SERVER_IS_OVERLOADED ","type":"server_error"}), None, 503, 503),
        (json!({"code":"server_error","type":"server_error"}), None, 502, 502),
        (json!({"code":"internal_server_error"}), None, 502, 502),
        (json!({"code":"invalid_value","type":"invalid_request_error","status":400}), None, 400, 400),
        (json!({"code":"server_error","status":400}), None, 400, 400),
        (json!({"code":"server_is_overloaded","status":401}), None, 401, 401),
        (json!({"code":"server_error","status_code":503}), None, 503, 503),
        (json!({"code":"server_error","status":500,"status_code":503}), Some(504), 500, 500),
        (json!({"code":"server_error","status":200}), Some(429), 429, 429),
        (json!({"code":"server_error","status":600}), Some(504), 504, 504),
        (json!({"code":"server_error","status":"503"}), None, 502, 502),
        (json!({"code":"invalid_value","type":"invalid_request_error"}), None, 400, 502),
        (json!({"code":"provider_failure","message":"server_is_overloaded; you can retry your request"}), None, 400, 502),
        (json!({"code":429}), None, 400, 429),
        (json!({"code":429,"status":400}), None, 400, 400),
    ];
    for (error, event_status, responses_status, chat_status) in cases {
        for shape in ["responses", "responses_bare", "responses_failed", "chat", "chat_choice"] {
            let is_responses = shape.starts_with("responses");
            let mut event = match shape {
                "responses" => json!({"type":"error","error":error.clone()}),
                "responses_bare" => json!({"error":error.clone()}),
                "responses_failed" => json!({
                    "type":"response.failed",
                    "response":{"id":"resp_failure","status":"failed","output":[],"error":error.clone()}
                }),
                "chat_choice" => json!({"choices":[{"index":0,"delta":{},"finish_reason":"error","error":error.clone()}]}),
                _ => json!({"error":error.clone()}),
            };
            if let Some(status) = event_status {
                event["status"] = json!(status);
            }
            let preamble = if is_responses {
                json!({"type":"response.created","response":{"id":"resp_failure","status":"in_progress","output":[]}})
            } else {
                json!({"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]})
            };
            let upstream = reqwest::Response::from(
                axum::http::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(format!("data: {preamble}\n\ndata: {event}\n\n"))
                    .unwrap(),
            );
            let request = UrpRequest {
                model: "test-model".to_string(),
                estimated_input_tokens: Default::default(),
                has_tools: false,
                max_multiplier: None,
                audio_output_format: None,
                server_tool_usage_classes: vec![],
                messages_custom_tool_names: Default::default(),
                affinity_explicit: None,
                affinity_prefix_hash: String::new(),
            };
            let metrics = Arc::new(Mutex::new(StreamRuntimeMetrics::default()));
            let (tx, mut rx) = mpsc::channel(64);
            crate::urp::stream_decode::stream_upstream_to_urp_events(
                &request,
                None,
                if is_responses { ProviderType::Responses } else { ProviderType::ChatCompletion },
                upstream,
                tx,
                None,
                Some(metrics.clone()),
                1000,
            )
            .await
            .unwrap();
            while rx.recv().await.is_some() {}
            let terminal = metrics.lock().await.terminal.terminal_error.clone().expect("terminal error");
            assert_eq!(
                terminal.http_status,
                if is_responses { responses_status } else { chat_status },
                "{shape}: {event}"
            );
            if let Some(code) = error["code"].as_str() {
                assert_eq!(terminal.code, code, "{shape}: {event}");
            }
            assert_eq!(terminal.error_type.as_deref(), error["type"].as_str(), "{shape}: {event}");
        }
    }
}

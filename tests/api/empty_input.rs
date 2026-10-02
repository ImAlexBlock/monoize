use super::*;

#[tokio::test]
async fn empty_converted_input_is_rejected_without_upstream_dispatch() {
    let ctx = setup().await;
    for stream in [false, true] {
        for (path, mut request) in [
            (
                "/v1/chat/completions",
                json!({"model":"gpt-5-mini-chat", "messages":[]}),
            ),
            (
                "/v1/chat/completions",
                json!({"model":"gpt-5-mini-chat", "messages":[{"role":"user", "content":""}]}),
            ),
            (
                "/v1/chat/completions",
                json!({"model":"gpt-5-mini", "messages":[]}),
            ),
            ("/v1/responses", json!({"model":"gpt-5-mini", "input":[]})),
            ("/v1/responses", json!({"model":"gpt-5-mini"})),
            (
                "/v1/responses",
                json!({"model":"gpt-5-mini", "input":[], "previous_response_id":null, "conversation":null, "prompt":null}),
            ),
            (
                "/v1/responses",
                json!({"model":"gpt-5-mini-chat", "previous_response_id":"resp_prior", "input":[{"type":"function_call_output", "call_id":"call_prior", "output":"result"}]}),
            ),
            (
                "/v1/responses",
                json!({"model":"gpt-5-mini-chat", "input":[{"type":"item_reference", "id":"item_prior"}]}),
            ),
        ] {
            request["stream"] = json!(stream);
            let (status, body) = json_post(&ctx, path, request.clone()).await;
            assert_eq!(
                status,
                if stream {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_REQUEST
                },
                "{request}: {body}"
            );
            assert!(
                body.contains("empty_input_after_conversion"),
                "{request}: {body}"
            );
            assert!(body.contains("full conversation history"), "{body}");
            assert!(
                ctx.captured_bodies.lock().unwrap().is_empty(),
                "empty request reached upstream: {request}"
            );
        }
    }
}

#[tokio::test]
async fn empty_input_preserves_native_responses_state_and_prompt() {
    let ctx = setup().await;
    for stream in [false, true] {
        for reference in [
            json!({"previous_response_id":"resp_prior"}),
            json!({"conversation":"conv_prior"}),
            json!({"prompt":{"id":"pmpt_saved"}}),
        ] {
            let mut request = reference.clone();
            request["model"] = json!("gpt-5-mini");
            request["stream"] = json!(stream);
            request["input"] = json!([]);
            let (status, body) = json_post(&ctx, "/v1/responses", request).await;
            assert_eq!(status, StatusCode::OK, "{reference}: {body}");
            assert!(!body.contains("empty_input_after_conversion"), "{body}");
            let captured = ctx.captured_bodies.lock().unwrap();
            let (endpoint, upstream) = captured.last().unwrap();
            assert_eq!(endpoint, "responses");
            assert_eq!(upstream["input"], json!([]));
            for (key, value) in reference.as_object().unwrap() {
                assert_eq!(&upstream[key], value);
            }
        }
    }
}

#[tokio::test]
async fn empty_cross_family_attempt_does_not_block_native_responses_channel() {
    for stream in [false, true] {
        let ctx = setup().await;
        let providers = ctx.state.monoize_store.list_providers().await.unwrap();
        let native = providers
            .iter()
            .find(|provider| provider.name == "up-resp")
            .unwrap();
        let cross = create_test_provider_in_group(
            &ctx.state,
            "empty-cross-family",
            monoize::monoize_routing::MonoizeProviderType::ChatCompletion,
            "gpt-5-mini",
            &native.channel.base_url,
            "upstream-key",
            &native.group_id,
        )
        .await;
        ctx.state
            .monoize_store
            .update_provider(
                &cross.id,
                serde_json::from_value(json!({"priority":-10, "channel_max_retries":2})).unwrap(),
            )
            .await
            .unwrap();
        let (status, body) = json_post(
            &ctx,
            "/v1/responses",
            json!({
                "model":"gpt-5-mini", "stream":stream,
                "previous_response_id":"resp_prior",
                "input":[{"type":"function_call_output", "call_id":"call_prior", "output":"result"}]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("empty_input_after_conversion"), "{body}");
        let captured = ctx.captured_bodies.lock().unwrap();
        assert_eq!(captured.len(), 1, "{captured:?}");
        assert_eq!(captured[0].0, "responses");
        assert_eq!(captured[0].1["previous_response_id"], "resp_prior");
        assert_eq!(captured[0].1["input"][0]["type"], "function_call_output");
        assert_eq!(captured[0].1["input"][0]["output"], "result");
    }
}

#[tokio::test]
async fn empty_input_is_rejected_on_buffered_stream_path() {
    let ctx = setup().await;
    ctx.state.monoize_runtime.write().await.global_transforms =
        vec![monoize::transforms::TransformRuleConfig {
            transform: "image_markdown_to_output".into(),
            enabled: true,
            models: None,
            phase: monoize::transforms::Phase::Response,
            config: json!({}),
        }];
    let (status, body) = json_post(
        &ctx,
        "/v1/chat/completions",
        json!({
            "model":"gpt-5-mini-chat", "stream":true, "messages":[]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("empty_input_after_conversion"), "{body}");
    assert!(ctx.captured_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_input_does_not_fall_back_to_recent_model() {
    let ctx = setup().await;
    let (status, body) = json_post(&ctx, "/v1/chat/completions", json!({
        "model":"gpt-5-mini-chat", "messages":[{"role":"user", "content":"establish recent model"}]
    })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let user = ctx
        .state
        .user_store
        .get_user_by_username("tenant-1")
        .await
        .unwrap()
        .unwrap();
    let mut established = false;
    for _ in 0..40 {
        ctx.state.user_store.flush_all_batchers().await;
        let (logs, _, _) = ctx
            .state
            .user_store
            .list_request_logs_by_user(
                &user.id,
                10,
                0,
                Some("gpt-5-mini-chat"),
                Some("success"),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        if !logs.is_empty() {
            established = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(established);
    let before = ctx.captured_bodies.lock().unwrap().len();
    for stream in [false, true] {
        let (status, body) = json_post(
            &ctx,
            "/v1/responses",
            json!({
                "model":"gpt-5-mini", "stream":stream, "input":[]
            }),
        )
        .await;
        assert_eq!(
            status,
            if stream {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            },
            "{body}"
        );
        assert!(body.contains("empty_input_after_conversion"), "{body}");
    }
    assert_eq!(ctx.captured_bodies.lock().unwrap().len(), before);
    for _ in 0..40 {
        ctx.state.user_store.flush_all_batchers().await;
        let (logs, _, _) = ctx
            .state
            .user_store
            .list_request_logs_by_user(
                &user.id,
                10,
                0,
                Some("gpt-5-mini"),
                Some("error"),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        if logs.len() == 2 {
            for log in logs {
                assert_eq!(
                    log.error.code.as_deref(),
                    Some("empty_input_after_conversion")
                );
                assert!(log.billing.charge_nano_usd.is_none());
            }
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("expected one local terminal log per empty request");
}

#[tokio::test]
async fn empty_attempt_does_not_replace_actual_upstream_failure() {
    async fn fail() -> impl IntoResponse {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{
                "code":"upstream_state_rejected", "message":"state rejected", "type":"invalid_request_error"
            }})),
        )
    }
    for stream in [false, true] {
        let ctx = setup().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/responses", post(fail)))
                .await
                .unwrap();
        });
        let native = ctx
            .state
            .monoize_store
            .list_providers()
            .await
            .unwrap()
            .into_iter()
            .find(|provider| provider.name == "up-resp")
            .unwrap();
        ctx.state.monoize_store.update_provider(&native.id, serde_json::from_value(json!({
            "priority":-10, "channel":{
                "name":native.channel.name, "provider_type":"responses",
                "base_url":format!("http://{address}"), "api_key":"upstream-key", "models":native.channel.models
            }
        })).unwrap()).await.unwrap();
        create_test_provider_in_group(
            &ctx.state,
            "later-empty-channel",
            monoize::monoize_routing::MonoizeProviderType::ChatCompletion,
            "gpt-5-mini",
            &native.channel.base_url,
            "upstream-key",
            &native.group_id,
        )
        .await;
        let (status, body) = json_post(&ctx, "/v1/responses", json!({
            "model":"gpt-5-mini", "stream":stream, "input":[], "previous_response_id":"resp_prior"
        })).await;
        assert_eq!(
            status,
            if stream {
                StatusCode::OK
            } else {
                StatusCode::BAD_GATEWAY
            },
            "{body}"
        );
        assert!(body.contains("upstream_state_rejected"), "{body}");
        assert!(!body.contains("empty_input_after_conversion"), "{body}");
        assert!(ctx.captured_bodies.lock().unwrap().is_empty());
    }
}

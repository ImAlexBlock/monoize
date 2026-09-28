use super::*;

const MODEL: &str = "protocol-settlement-test";

async fn context(provider_type: &str, response: Value) -> TestContext {
    async fn upstream(axum::extract::State(response): axum::extract::State<Value>) -> Json<Value> {
        Json(response)
    }
    let ctx = setup().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().fallback(upstream).with_state(response);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    ctx.state.monoize_store.create_provider(serde_json::from_value(json!({
        "name": "protocol-settlement", "confirm_public_exposure": true,
        "pricing_profile": "default",
        "transforms": [{"transform": "image_markdown_to_output", "enabled": true, "phase": "response", "config": {}}],
        "channel": {"name": "protocol-settlement", "provider_type": provider_type,
            "base_url": format!("http://{address}"), "api_key": "mock-key",
            "models": {(MODEL): {}}}
    })).unwrap()).await.unwrap();
    seed_test_model_pricing(&ctx.state, &[MODEL]).await;
    let user = ctx.state.user_store.get_user_by_username("tenant-1").await.unwrap().unwrap();
    ctx.state.user_store.update_user(&user.id, None, None, None, None,
        Some("1000000000"), Some(false), None, None).await.unwrap();
    ctx
}

async fn assert_failed_without_charge(ctx: &TestContext, stream: bool) {
    let user = ctx.state.user_store.get_user_by_username("tenant-1").await.unwrap().unwrap();
    let mut terminal = None;
    for _ in 0..40 {
        ctx.state.user_store.flush_all_batchers().await;
        let (logs, _, _) = ctx.state.user_store.list_request_logs_by_user(
            &user.id, 100, 0, Some(MODEL), None, None, None, None, None).await.unwrap();
        let completed: Vec<_> = logs.into_iter().filter(|log| log.status != "pending").collect();
        if !completed.is_empty() {
            assert_eq!(completed.len(), 1);
            terminal = completed.into_iter().next();
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let log = terminal.expect("terminal request log");
    assert_eq!(log.status, "error");
    assert_eq!(log.is_stream, stream);
    assert_eq!(log.billing.charge_nano_usd, None);
    assert_eq!(log.tokens.input, None);
    assert_eq!(log.tokens.output, None);
    let after = ctx.state.user_store.get_user_by_username("tenant-1").await.unwrap().unwrap();
    assert_eq!(after.balance_nano_usd.to_string(), "1000000000");
}

async fn rejected_media(stream: bool) {
    let ctx = context("gemini", json!({
        "responseId": "image-result", "modelVersion": MODEL,
        "candidates": [{"content": {"role": "model", "parts": [{"inlineData": {"mimeType": "image/png", "data": "aW1hZ2U="}}]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 2, "totalTokenCount": 5}
    })).await;
    let (status, body) = json_post(&ctx, "/v1/chat/completions", json!({
        "model": MODEL, "messages": [{"role": "user", "content": "draw"}], "stream": stream
    })).await;
    if !stream { assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}"); }
    assert!(body.contains(if stream { "unsupported_media" } else { "unsupported_output_media" }), "{body}");
    if stream { assert_eq!(body.matches("data: [DONE]").count(), 1, "{body}"); }
    assert_failed_without_charge(&ctx, stream).await;
}

#[tokio::test]
async fn unrepresentable_nonstream_media_is_not_billed() { rejected_media(false).await; }

#[tokio::test]
async fn unrepresentable_synthetic_stream_media_is_not_billed() { rejected_media(true).await; }

async fn rejected_outcome(stream: bool) {
    for status in ["failed", "cancelled"] {
        let ctx = context("responses", json!({
            "id": "failed-result", "object": "response", "model": MODEL,
            "status": status, "output": [],
            "error": {"code": "insufficient_quota", "message": "insufficient quota for private-provider-account", "type": "upstream_error"},
            "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5}
        })).await;
        ctx.state.monoize_runtime.write().await.mask_sensitive_info = false;
        let (path, request) = if stream {
            ("/v1/chat/completions", json!({"model": MODEL, "messages": [{"role": "user", "content": "answer"}], "stream": true}))
        } else {
            ("/v1/responses", json!({"model": MODEL, "input": "answer"}))
        };
        let (http_status, body) = json_post(&ctx, path, request).await;
        if !stream { assert_eq!(http_status, StatusCode::BAD_GATEWAY, "{body}"); }
        assert!(!body.contains("private-provider-account"), "{body}");
        assert!(body.contains("insufficient_quota"), "{body}");
        assert_failed_without_charge(&ctx, stream).await;
    }
}

#[tokio::test]
async fn failed_nonstream_outcomes_are_sanitized_and_not_billed() { rejected_outcome(false).await; }

#[tokio::test]
async fn failed_synthetic_stream_outcomes_are_sanitized_and_not_billed() { rejected_outcome(true).await; }

use super::*;
use std::fs;

fn dumps_dir(ctx: &TestContext) -> std::path::PathBuf {
    let db_path = ctx._temp_dir.path().join("monoize.db");
    db_path.parent().expect("db parent exists").join("dumps")
}

fn assert_stream_request_snapshots(ctx: &TestContext, dump: &Value, input: &str) {
    let attempt = &dump["attempts"][0];
    let transformed = &attempt["transformed_urp_request"];
    assert_eq!(transformed["model"], "gpt-5-mini");
    assert_eq!(transformed["stream"], true);
    assert_eq!(transformed["input"][0]["type"], "text");
    assert_eq!(transformed["input"][0]["content"], input);

    let upstream_requests = ctx.captured_bodies.lock().expect("upstream requests lock");
    let (_, sent_body) = upstream_requests
        .iter()
        .find(|(protocol, _)| protocol == "responses")
        .expect("Responses request reached upstream");
    assert_eq!(&attempt["upstream_request"], sent_body);
}

async fn enable_request_capture(ctx: &TestContext) {
    let settings = ctx
        .state
        .settings_store
        .get_all()
        .await
        .expect("settings load");
    let updated_settings = monoize::settings::SystemSettings {
        monoize_request_capture_enabled: true,
        ..settings
    };
    ctx.state
        .settings_store
        .update_all(&updated_settings)
        .await
        .expect("settings update");
    {
        let mut runtime = ctx.state.monoize_runtime.write().await;
        runtime.request_capture_enabled = updated_settings.monoize_request_capture_enabled;
        runtime.request_capture_retention_days =
            updated_settings.monoize_request_capture_retention_days;
    }

    let token = ctx
        .auth_header
        .strip_prefix("Bearer ")
        .expect("bearer token present");
    let key = ctx
        .state
        .user_store
        .get_api_key_by_prefix(&token[..12])
        .await
        .expect("lookup succeeds")
        .expect("api key exists");
    ctx.state
        .user_store
        .update_api_key(
            &key.id,
            monoize::users::UpdateApiKeyInput {
                name: None,
                enabled: None,
                sub_account_enabled: None,
                sub_account_balance_nano_usd: None,
                model_limits_enabled: None,
                model_limits: None,
                ip_whitelist: None,
                group_ids: None,
                channel_bindings: None,
                model_bindings: None,
                max_multiplier: None,
                transforms: None,
                model_redirects: None,
                reasoning_envelope_enabled: None,
                request_capture_mode: Some(monoize::users::RequestCaptureMode::CaptureAll),
                expires_at: None,
                spend_limit_daily_nano_usd: None,
                spend_limit_hourly_nano_usd: None,
                spend_limit_total_nano_usd: None,
            },
            false,
        )
        .await
        .expect("api key update");
}

#[tokio::test]
async fn nonstream_request_capture_writes_dump_with_sanitized_prefix() {
    let ctx = setup().await;
    enable_request_capture(&ctx).await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, ctx.auth_header.clone())
        .header("x-request-id", "../evil42")
        .body(Body::from(
            json!({
                "model": "gpt-5-mini",
                "input": "capture me"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = ctx.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let dump_dir = dumps_dir(&ctx);
    let entries: Vec<_> = fs::read_dir(&dump_dir)
        .expect("dump dir exists")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(entries.len(), 1);
    let filename = entries[0]
        .file_name()
        .and_then(|name| name.to_str())
        .expect("utf8 filename")
        .to_string();
    assert!(filename.starts_with("___evil4_"));
    let dump: Value =
        serde_json::from_slice(&fs::read(&entries[0]).expect("dump readable")).expect("dump json");
    assert_eq!(dump["request_id"].as_str(), Some("../evil42"));
    assert_eq!(
        dump["attempts"][0]["raw_input"]["input"].as_str(),
        Some("capture me")
    );
}

#[tokio::test]
async fn streaming_request_capture_records_downstream_sse_frames() {
    let ctx = setup().await;
    enable_request_capture(&ctx).await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, ctx.auth_header.clone())
        .header("x-request-id", "stream123")
        .body(Body::from(
            json!({
                "model": "gpt-5-mini",
                "input": "stream capture",
                "stream": true,
                "emit_usage": true
            })
            .to_string(),
        ))
        .unwrap();
    let resp = ctx.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _bytes = resp.into_body().collect().await.unwrap().to_bytes();

    let dump_dir = dumps_dir(&ctx);
    let mut entries: Vec<_> = fs::read_dir(&dump_dir)
        .expect("dump dir exists")
        .map(|entry| entry.expect("entry").path())
        .collect();
    entries.sort();
    let dump: Value = serde_json::from_slice(
        &fs::read(entries.last().expect("dump path")).expect("dump readable"),
    )
    .expect("dump json");
    assert_stream_request_snapshots(&ctx, &dump, "stream capture");
    let frames = dump["attempts"][0]["downstream_sse_frames"]
        .as_array()
        .expect("frames array");
    assert!(!frames.is_empty());
    assert!(frames.iter().any(|frame| {
        frame
            .as_str()
            .is_some_and(|s| s.contains("response.output_text.delta"))
    }));
    assert!(
        frames
            .iter()
            .any(|frame| frame.as_str().is_some_and(|s| s.contains("[DONE]")))
    );
}

#[tokio::test]
async fn streaming_request_capture_records_downstream_error_sse_frames() {
    let ctx = setup().await;
    enable_request_capture(&ctx).await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, ctx.auth_header.clone())
        .header("x-request-id", "streamerr")
        .body(Body::from(
            json!({
                "model": "gpt-5-mini",
                "input": "stream capture error",
                "stream": true,
                "stream_mode": "error_event"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = ctx.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("event: response.failed"),
        "downstream stream: {text}"
    );

    let dump_dir = dumps_dir(&ctx);
    let mut entries: Vec<_> = fs::read_dir(&dump_dir)
        .expect("dump dir exists")
        .map(|entry| entry.expect("entry").path())
        .collect();
    entries.sort();
    let dump: Value = serde_json::from_slice(
        &fs::read(entries.last().expect("dump path")).expect("dump readable"),
    )
    .expect("dump json");
    assert_stream_request_snapshots(&ctx, &dump, "stream capture error");
    let frames = dump["attempts"][0]["downstream_sse_frames"]
        .as_array()
        .expect("frames array");
    assert!(
        frames.iter().any(|frame| {
            frame.as_str().is_some_and(|s| {
                s.contains("event: response.failed") && s.contains("mock_stream_error")
            })
        }),
        "captured frames: {frames:?}"
    );
}

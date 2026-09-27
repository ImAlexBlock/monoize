use super::*;
use sea_orm::ConnectionTrait;

const LOGICAL_MODEL: &str = "upstream-response-log-test";
const SENT_MODEL: &str = "sent-model";

async fn response_model_context(observed: Option<&str>, transforms: Value) -> TestContext {
    async fn upstream(
        axum::extract::State(observed): axum::extract::State<Option<String>>,
        uri: axum::http::Uri,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        assert_eq!(body["model"], SENT_MODEL);
        let usage = json!({"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5});
        if body["stream"] == true {
            if uri.path().ends_with("responses") {
                let mut response = json!({"id": "response-model-test", "status": "completed",
                    "output": [{"type": "message", "id": "message-test", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]}],
                    "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5}});
                if let Some(model) = observed {
                    response["model"] = json!(model);
                }
                let event = json!({"type": "response.completed", "response": response});
                return (
                    [(CONTENT_TYPE, "text/event-stream")],
                    format!("event: response.completed\ndata: {event}\n\n"),
                )
                    .into_response();
            }
            let mut event = json!({
                "id": "response-model-test", "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": "stop"}],
                "usage": usage
            });
            if let Some(model) = observed {
                event["model"] = json!(model);
            }
            return (
                [(CONTENT_TYPE, "text/event-stream")],
                format!("data: {event}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        let mut response = if uri.path().ends_with("embeddings") {
            json!({"object": "list", "data": [{"object": "embedding", "index": 0, "embedding": [0.5]}], "usage": usage})
        } else {
            json!({"id": "response-model-test", "object": "chat.completion", "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}], "usage": usage})
        };
        if let Some(model) = observed {
            response["model"] = json!(model);
        }
        Json(response).into_response()
    }
    let ctx = setup().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = Router::new()
        .route("/v1/responses", post(upstream))
        .route("/v1/chat/completions", post(upstream))
        .route("/v1/embeddings", post(upstream))
        .with_state(observed.map(str::to_owned));
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let provider_type = if transforms
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| rule["transform"] == "stream_force")
    {
        "responses"
    } else {
        "chat_completion"
    };
    ctx.state
        .monoize_store
        .create_provider(
            serde_json::from_value(json!({
                "name": "response-model-observer", "confirm_public_exposure": true,
                "pricing_profile": "default", "transforms": transforms,
                "channel": {"name": "response-model-channel", "provider_type": provider_type,
                    "base_url": format!("http://{address}"), "api_key": "mock-key",
                    "models": {(LOGICAL_MODEL): {"redirect": SENT_MODEL}}}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    seed_test_model_pricing(&ctx.state, &[LOGICAL_MODEL, SENT_MODEL]).await;
    ctx
}

async fn logged_request(
    ctx: &TestContext,
    stream: bool,
    embeddings: bool,
) -> monoize::users::RequestLogRow {
    let (path, body) = if embeddings {
        (
            "/v1/embeddings",
            json!({"model": LOGICAL_MODEL, "input": "hello"}),
        )
    } else {
        (
            "/v1/chat/completions",
            json!({"model": LOGICAL_MODEL, "messages": [{"role": "user", "content": "hello"}], "stream": stream}),
        )
    };
    let (status, body) = json_post(ctx, path, body).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let user = ctx
        .state
        .user_store
        .get_user_by_username("tenant-1")
        .await
        .unwrap()
        .unwrap();
    for _ in 0..40 {
        ctx.state.user_store.flush_all_batchers().await;
        let (rows, _, _) = ctx
            .state
            .user_store
            .list_request_logs_by_user(
                &user.id,
                10,
                0,
                Some(LOGICAL_MODEL),
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        if let Some(row) = rows.into_iter().next() {
            assert_eq!(row.status, "success", "{body}");
            assert_eq!(row.model, LOGICAL_MODEL);
            assert_eq!(row.upstream_model.as_deref(), Some(SENT_MODEL));
            return row;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("terminal request log missing: {body}");
}

#[tokio::test]
async fn streaming_response_model_matches_sent_model_after_routing() {
    let ctx = response_model_context(Some(SENT_MODEL), json!([])).await;
    assert_eq!(
        logged_request(&ctx, true, false)
            .await
            .upstream_response_model,
        None
    );
}

#[tokio::test]
async fn buffered_stream_response_model_matches_sent_model_after_routing() {
    let ctx = response_model_context(Some(SENT_MODEL), json!([
        {"transform": "image_markdown_to_output", "enabled": true, "phase": "response", "config": {}}
    ])).await;
    assert_eq!(
        logged_request(&ctx, true, false)
            .await
            .upstream_response_model,
        None
    );
}

#[tokio::test]
async fn collected_stream_does_not_infer_an_undeclared_response_model() {
    let ctx = response_model_context(None, json!([
        {"transform": "stream_force", "enabled": true, "phase": "request", "config": {"enabled": true}}
    ])).await;
    assert_eq!(
        logged_request(&ctx, false, false)
            .await
            .upstream_response_model,
        None
    );
}

#[tokio::test]
async fn embeddings_record_the_upstream_response_model_before_downstream_rewrite() {
    let ctx = response_model_context(Some("actual-model"), json!([])).await;
    assert_eq!(
        logged_request(&ctx, false, true)
            .await
            .upstream_response_model
            .as_deref(),
        Some("actual-model")
    );
}

async fn dashboard_rows(ctx: &TestContext, token: &str, uri: &str) -> Value {
    let response = ctx
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn upstream_response_model_is_admin_only_for_personal_and_org_logs() {
    let ctx = response_model_context(Some(" actual-model "), json!([])).await;
    let log = logged_request(&ctx, false, false).await;
    assert_eq!(log.upstream_response_model.as_deref(), Some("actual-model"));
    let tenant = ctx
        .state
        .user_store
        .get_user_by_username("tenant-1")
        .await
        .unwrap()
        .unwrap();
    let admin = ctx
        .state
        .user_store
        .create_user(
            "log-admin",
            "password123",
            monoize::users::UserRole::Admin,
            None,
        )
        .await
        .unwrap();
    let tenant_session = ctx
        .state
        .user_store
        .create_session(&tenant.id, 7)
        .await
        .unwrap();
    let admin_session = ctx
        .state
        .user_store
        .create_session(&admin.id, 7)
        .await
        .unwrap();
    for masking in [true, false] {
        ctx.state.monoize_runtime.write().await.mask_sensitive_info = masking;
        let uri = format!("/api/dashboard/request-logs?model={LOGICAL_MODEL}");
        assert!(
            dashboard_rows(&ctx, &tenant_session.token, &uri).await["data"][0]
                .get("upstream_response_model")
                .is_none()
        );
        assert_eq!(
            dashboard_rows(&ctx, &admin_session.token, &uri).await["data"][0]["upstream_response_model"],
            "actual-model"
        );
    }
    let org = ctx
        .state
        .user_store
        .create_user(
            "log-org",
            "password123",
            monoize::users::UserRole::User,
            None,
        )
        .await
        .unwrap();
    let now = Utc::now().to_rfc3339();
    let backend = ctx.state.db_pool.read().get_database_backend();
    let write = ctx.state.db_pool.write().await;
    write.execute(sea_orm::Statement::from_sql_and_values(backend,
        "INSERT INTO orgs (id, owner_user_id, display_name, avatar_emoji, avatar_color, invite_token, invite_code, invite_created_at, created_at, updated_at) VALUES ($1, $2, 'Log Org', 'L', '#112233', 'log-org-token', 'LOG123', $3, $3, $3)",
        [org.id.clone().into(), tenant.id.clone().into(), now.clone().into()])).await.unwrap();
    for user_id in [&tenant.id, &admin.id] {
        write.execute(sea_orm::Statement::from_sql_and_values(backend,
            "INSERT INTO org_members (org_id, user_id, role, joined_at) VALUES ($1, $2, 'member', $3)",
            [org.id.clone().into(), user_id.clone().into(), now.clone().into()])).await.unwrap();
    }
    write
        .execute(sea_orm::Statement::from_sql_and_values(
            backend,
            "UPDATE request_logs SET user_id = $1 WHERE id = $2",
            [org.id.clone().into(), log.id.into()],
        ))
        .await
        .unwrap();
    drop(write);
    for masking in [true, false] {
        ctx.state.monoize_runtime.write().await.mask_sensitive_info = masking;
        let uri = format!("/api/dashboard/orgs/{}/request-logs", org.id);
        assert!(
            dashboard_rows(&ctx, &tenant_session.token, &uri).await["data"][0]
                .get("upstream_response_model")
                .is_none()
        );
        assert_eq!(
            dashboard_rows(&ctx, &admin_session.token, &uri).await["data"][0]["upstream_response_model"],
            "actual-model"
        );
    }
}

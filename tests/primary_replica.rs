//! `primary-replica-deployment.spec.md` test matrix (T1–T8).

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine as _;
use futures_util::{StreamExt as _, stream};
use http_body_util::BodyExt;
use monoize::db_cache::{LastUsedBatcher, RequestLogBatcher};
use monoize::node_config::NodeRole;
use monoize::replica::metering::{
    BalanceDelta, DeltaSpool, HEARTBEAT_EVICT_INTERVALS, MeteringAck, MeteringBatch,
    MeteringSpoolCapacity, PendingDeductions, REPLICA_IDENTITY_FILE_NAME, ReplicaHeartbeat,
    ReplicaHeartbeatRecord, ReplicaHeartbeatSource, ReplicaMetering, ShipTick,
    apply_metering_batch, drain_delta_spool_to_local_db, evict_expired_heartbeats,
    resolve_replica_identity,
};
use monoize::store_billing::admission_runtime::{TerminalApplyInput, terminal_digest};
use monoize::store_billing::admission_token::{
    AdmissionClaimStore, PlanTerminalAcknowledgement, PlanTerminalWire,
    TerminalAcknowledgementResult, TerminalKind, TerminalSpoolInput, VerifiedAdmission,
};
use sea_orm::ConnectionTrait;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tower::ServiceExt;

const TEST_REPLICA_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn test_runtime(database_dsn: String) -> monoize::app::RuntimeConfig {
    monoize::app::RuntimeConfig {
        listen: "127.0.0.1:0".to_string(),
        metrics_path: "/metrics".to_string(),
        database_dsn,
        request_log_spool_dir: None,
        node: monoize::node_config::NodeSettings::primary_default(),
    }
}

fn delta(kind: &str, user_id: &str, api_key_id: Option<&str>, amount: i128) -> BalanceDelta {
    BalanceDelta {
        delta_id: uuid::Uuid::new_v4().to_string(),
        kind: kind.to_string(),
        user_id: user_id.to_string(),
        api_key_id: api_key_id.map(str::to_string),
        amount_nano_usd: amount.to_string(),
        meta_json: serde_json::json!({ "request_id": "req-1" }),
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn oversized_pending_response() -> Response {
    let first = stream::once(async { Ok::<_, Infallible>(bytes::Bytes::from(vec![b'x'; 65_537])) });
    let pending = stream::pending::<Result<bytes::Bytes, Infallible>>();
    Body::from_stream(first.chain(pending)).into_response()
}

fn capacity_admission(now: chrono::DateTime<chrono::Utc>) -> VerifiedAdmission {
    VerifiedAdmission {
        issuer: "lynshen-primary".to_string(),
        key_id: "key".to_string(),
        audience: TEST_REPLICA_ID.to_string(),
        token_id: "capacity-token".to_string(),
        reservation_id: "capacity-reservation".to_string(),
        request_id: "capacity-request".to_string(),
        entitlement_id: "capacity-entitlement".to_string(),
        generation: 1,
        maximum_nano_usd: 100,
        reserved_fen_cny: 1,
        pricing_revision: "pricing-v1".to_string(),
        issued_at: now,
        expires_at: now + chrono::Duration::seconds(30),
    }
}

#[test]
fn plan_terminal_wire_fields_default_for_backward_compatibility() {
    let batch: MeteringBatch = serde_json::from_value(serde_json::json!({
        "request_logs": [],
        "last_used": [],
        "balance_deltas": []
    }))
    .unwrap();
    assert!(batch.plan_terminals.is_empty());
    let ack: MeteringAck = serde_json::from_value(serde_json::json!({
        "applied_request_logs": 0,
        "applied_last_used": 0,
        "applied_balance_deltas": 0
    }))
    .unwrap();
    assert!(ack.plan_terminal_acks.is_empty());
}

#[test]
fn pending_ack_preserves_a_concurrent_new_deduction() {
    let pending = PendingDeductions::default();
    let barrier = std::sync::Barrier::new(2);
    pending.add("user", 1);
    let first_mismatch = std::thread::scope(|scope| {
        let producer = scope.spawn(|| {
            for _ in 0..50_000 {
                barrier.wait();
                pending.add("user", 1);
                barrier.wait();
            }
        });
        let mut first_mismatch = None;
        for round in 0..50_000 {
            barrier.wait();
            pending.subtract("user", 1);
            barrier.wait();
            let observed = pending.outstanding("user");
            if observed != 1 && first_mismatch.is_none() {
                first_mismatch = Some((round, observed));
            }
        }
        producer.join().unwrap();
        first_mismatch
    });
    assert_eq!(first_mismatch, None, "acknowledgement lost a new deduction");
    assert_eq!(pending.outstanding("user"), 1);
}

#[tokio::test]
async fn delta_and_claim_publication_share_one_capacity_limit() {
    let now = chrono::Utc::now();
    let admission = capacity_admission(now);
    let sizing = TempDir::new().unwrap();
    let sizing_capacity = Arc::new(MeteringSpoolCapacity::new(1024 * 1024));
    let sizing_store =
        AdmissionClaimStore::new_with_capacity(sizing.path(), sizing_capacity.clone())
            .await
            .unwrap();
    sizing_store.claim(&admission).await.unwrap();
    let exact_claim_capacity = sizing_capacity.accounted_bytes();

    let temp = TempDir::new().unwrap();
    let root = temp.path().join("metering");
    let capacity = Arc::new(MeteringSpoolCapacity::new(exact_claim_capacity));
    let spool = Arc::new(
        DeltaSpool::new_with_capacity(root.clone(), capacity.clone()).expect("delta spool"),
    );
    let claims =
        AdmissionClaimStore::new_with_capacity(root.join("plan-admission"), capacity.clone())
            .await
            .unwrap();
    let delta = delta("request_charge", "capacity-user", None, 1);
    let (delta_result, claim_result) =
        tokio::join!(spool.enqueue(&delta), claims.claim(&admission));

    assert_eq!(
        usize::from(delta_result.is_ok()) + usize::from(claim_result.is_ok()),
        1
    );
    assert!(capacity.accounted_bytes() <= capacity.max_bytes());
}

#[tokio::test]
async fn replica_metering_owns_one_capacity_verifier_and_claim_store() {
    let temp = TempDir::new().unwrap();
    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        "http://127.0.0.1:9",
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .unwrap();
    assert!(Arc::ptr_eq(
        metering.spool_capacity(),
        metering.admission_claims().capacity()
    ));
    assert!(
        metering
            .admission_verifier()
            .key_ids(chrono::Utc::now())
            .is_empty()
    );
    assert!(temp.path().join("metering/plan-admission/claims").is_dir());
    assert!(
        temp.path()
            .join("metering/plan-admission/terminal")
            .is_dir()
    );
}

#[tokio::test]
async fn replica_metering_and_admission_client_share_the_refreshed_verifier_snapshot() {
    let temp = TempDir::new().unwrap();
    let seed = [91_u8; 32];
    let public = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .as_bytes(),
    );
    let app = Router::new().route(
        monoize::replica::admission_http::ADMISSION_KEYSET_PATH,
        axum::routing::get(move || {
            let public = public.clone();
            async move {
                axum::Json(serde_json::json!({
                    "keys": [{
                        "key_id": "shared-key",
                        "public_key_base64": public,
                        "state": "active",
                        "activated_at": chrono::Utc::now() - chrono::Duration::minutes(1),
                        "verify_until": null
                    }]
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        &address,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .unwrap()
    .with_heartbeat_source(ReplicaHeartbeatSource {
        id: TEST_REPLICA_ID.to_string(),
        hostname: "replica-a".to_string(),
        listen: "127.0.0.1:8080".to_string(),
        version: "test".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
    });

    metering
        .admission_client()
        .refresh_if_due(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        metering.admission_verifier().key_ids(chrono::Utc::now()),
        vec!["shared-key"]
    );
}

#[tokio::test]
async fn exact_plan_terminal_acknowledgement_deletes_terminal_and_sends_audience_header() {
    let temp = TempDir::new().unwrap();
    let app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(
            |headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                let batch: MeteringBatch = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    headers
                        .get("x-monoize-replica-id")
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    TEST_REPLICA_ID
                );
                assert_eq!(batch.plan_terminals.len(), 1);
                let terminal = &batch.plan_terminals[0];
                axum::Json(MeteringAck {
                    applied_request_logs: 0,
                    applied_last_used: 0,
                    applied_balance_deltas: 0,
                    plan_terminal_acks: vec![PlanTerminalAcknowledgement {
                        token_id: terminal.token_id.clone(),
                        canonical_digest: terminal.canonical_digest.clone(),
                        result: TerminalAcknowledgementResult::Applied,
                    }],
                })
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        &addr,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .unwrap()
    .with_heartbeat_source(ReplicaHeartbeatSource {
        id: TEST_REPLICA_ID.to_string(),
        hostname: "replica-a".to_string(),
        listen: "127.0.0.1:8080".to_string(),
        version: "test".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
    });
    let admission = capacity_admission(chrono::Utc::now());
    metering.admission_claims().claim(&admission).await.unwrap();
    let terminal = metering
        .spool_plan_terminal(TerminalSpoolInput::release(&admission, chrono::Utc::now()))
        .await
        .unwrap();
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(16);
    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Success
    );
    assert!(!terminal.path.exists());
    assert!(
        metering
            .admission_claims()
            .marker_exists(&admission.token_id)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn ship_tick_publishes_release_for_release_pending_claim_without_terminal() {
    let temp = TempDir::new().unwrap();
    let app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(|body: axum::body::Bytes| async move {
            let batch: MeteringBatch = serde_json::from_slice(&body).unwrap();
            assert_eq!(batch.plan_terminals.len(), 1);
            let terminal = &batch.plan_terminals[0];
            assert_eq!(terminal.kind, TerminalKind::Release);
            axum::Json(MeteringAck {
                applied_request_logs: 0,
                applied_last_used: 0,
                applied_balance_deltas: 0,
                plan_terminal_acks: vec![PlanTerminalAcknowledgement {
                    token_id: terminal.token_id.clone(),
                    canonical_digest: terminal.canonical_digest.clone(),
                    result: TerminalAcknowledgementResult::Applied,
                }],
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        &addr,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .unwrap()
    .with_heartbeat_source(ReplicaHeartbeatSource {
        id: TEST_REPLICA_ID.to_string(),
        hostname: "replica-a".to_string(),
        listen: "127.0.0.1:8080".to_string(),
        version: "test".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
    });
    let mut admission = capacity_admission(chrono::Utc::now());
    admission.audience = TEST_REPLICA_ID.to_string();
    admission.token_id = "release-pending-token".to_string();
    admission.request_id = "release-pending-request".to_string();
    metering.admission_claims().claim(&admission).await.unwrap();
    metering
        .admission_claims()
        .mark_release_pending(&admission.token_id)
        .await
        .unwrap();
    assert!(
        metering
            .admission_claims()
            .load_pending_terminals(10)
            .await
            .unwrap()
            .is_empty()
    );
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(16);
    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Success
    );
    assert!(
        metering
            .admission_claims()
            .load_pending_terminals(10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_plan_ack_retains_terminal_but_releases_balance_delta() {
    let temp = TempDir::new().unwrap();
    let app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(|body: axum::body::Bytes| async move {
            let batch: MeteringBatch = serde_json::from_slice(&body).unwrap();
            let terminal = &batch.plan_terminals[0];
            axum::Json(MeteringAck {
                applied_request_logs: 0,
                applied_last_used: 0,
                applied_balance_deltas: batch.balance_deltas.len() as u64,
                plan_terminal_acks: vec![
                    PlanTerminalAcknowledgement {
                        token_id: terminal.token_id.clone(),
                        canonical_digest: terminal.canonical_digest.clone(),
                        result: TerminalAcknowledgementResult::Applied,
                    },
                    PlanTerminalAcknowledgement {
                        token_id: terminal.token_id.clone(),
                        canonical_digest: "0".repeat(64),
                        result: TerminalAcknowledgementResult::Applied,
                    },
                ],
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        &addr,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .unwrap()
    .with_heartbeat_source(ReplicaHeartbeatSource {
        id: TEST_REPLICA_ID.to_string(),
        hostname: "replica-a".to_string(),
        listen: "127.0.0.1:8080".to_string(),
        version: "test".to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
    });
    let admission = capacity_admission(chrono::Utc::now());
    metering.admission_claims().claim(&admission).await.unwrap();
    let terminal = metering
        .spool_plan_terminal(TerminalSpoolInput::release(&admission, chrono::Utc::now()))
        .await
        .unwrap();
    metering
        .enqueue_balance_delta("request_charge", "u-1", None, 1, &serde_json::json!({}))
        .await
        .unwrap();
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(16);
    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Failure
    );
    assert_eq!(metering.delta_spool().pending_files(), 0);
    assert!(terminal.path.exists());
}

#[tokio::test]
async fn non_terminal_batch_sends_canonical_replica_identity_header() {
    let temp = TempDir::new().unwrap();
    let spool_dir = temp.path().join("metering");
    let replica_id = resolve_replica_identity(None, &spool_dir).unwrap();
    let expected_id = replica_id.clone();
    let app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let expected_id = expected_id.clone();
                async move {
                    assert_eq!(
                        headers
                            .get("x-monoize-replica-id")
                            .and_then(|value| value.to_str().ok()),
                        Some(expected_id.as_str())
                    );
                    let batch: MeteringBatch = serde_json::from_slice(&body).unwrap();
                    assert!(batch.plan_terminals.is_empty());
                    axum::Json(MeteringAck {
                        applied_request_logs: batch.request_logs.len() as u64,
                        applied_last_used: batch.last_used.len() as u64,
                        applied_balance_deltas: batch.balance_deltas.len() as u64,
                        plan_terminal_acks: vec![],
                    })
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let metering =
        ReplicaMetering::new(spool_dir, 1024 * 1024, &addr, "token", 10, replica_id).unwrap();
    metering
        .enqueue_balance_delta("request_charge", "u-1", None, 1, &serde_json::json!({}))
        .await
        .unwrap();
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(16);

    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Success
    );
}

#[tokio::test]
async fn oversized_streaming_metering_ack_stops_at_limit_and_retains_durable_batch() {
    let temp = TempDir::new().unwrap();
    let spool_dir = temp.path().join("metering-oversized-ack");
    let app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(|| async { oversized_pending_response() }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let metering = ReplicaMetering::new(
        spool_dir,
        1024 * 1024,
        &addr,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .unwrap();
    metering
        .enqueue_balance_delta("request_charge", "u-1", None, 1, &serde_json::json!({}))
        .await
        .unwrap();
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool-oversized-ack"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(16);

    let tick = tokio::time::timeout(
        Duration::from_millis(500),
        metering.ship_once(&log_batcher, &last_used),
    )
    .await
    .expect("metering reader must stop after byte 65537");
    assert_eq!(tick, ShipTick::Failure);
    assert_eq!(metering.delta_spool().pending_files(), 1);
}

async fn boot() -> (TempDir, monoize::app::AppState) {
    let temp = TempDir::new().unwrap();
    let dsn = format!("sqlite://{}", temp.path().join("m.db").display());
    let state = monoize::app::load_state_with_runtime(test_runtime(dsn))
        .await
        .expect("state loads");
    (temp, state)
}

async fn balance_delta_fixture() -> (TempDir, monoize::app::AppState, String, String) {
    let (temp, state) = boot().await;
    let user = state
        .user_store
        .create_user("negative_delta", "pw", monoize::users::UserRole::User, None)
        .await
        .unwrap();
    let (key, _) = state
        .user_store
        .create_api_key(&user.id, "negative-delta-key", None)
        .await
        .unwrap();
    {
        let write = state.db_pool.write().await;
        write
            .execute(state.db_pool.stmt(
                "UPDATE users SET balance_nano_usd = '100' WHERE id = $1",
                vec![user.id.clone().into()],
            ))
            .await
            .unwrap();
        write
            .execute(state.db_pool.stmt(
                "UPDATE api_keys SET sub_account_enabled = 1, sub_account_balance_nano = '50' WHERE id = $1",
                vec![key.id.clone().into()],
            ))
            .await
            .unwrap();
    }
    (temp, state, user.id, key.id)
}

async fn assert_negative_delta_left_balances_unchanged(
    state: &monoize::app::AppState,
    user_id: &str,
    api_key_id: &str,
) {
    let user = state
        .user_store
        .get_user_balance_uncached(user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.balance_nano_usd, 100);
    let key = state
        .user_store
        .get_api_key_by_id(api_key_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key.sub_account_balance_nano, "50");
    assert!(
        state
            .user_store
            .list_billing_ledger(user_id, 50)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn metering_ingest_rejects_negative_charge_without_partial_apply() {
    let (_temp, mut state, user_id, api_key_id) = balance_delta_fixture().await;
    let mut node = (*state.node).clone();
    node.replica_token = Some("tok".to_string());
    state.node = Arc::new(node);
    state.metering_token_digest = Some(monoize::replica::metering::sha256_hex_lower("tok"));
    let app = monoize::app::build_app(state.clone());
    for (kind, key_id) in [
        ("request_charge", None),
        ("api_key_charge", Some(api_key_id.as_str())),
    ] {
        for amount in [-1, i128::MIN] {
            let batch = MeteringBatch {
                balance_deltas: vec![
                    delta("request_charge", &user_id, None, 1),
                    delta(kind, &user_id, key_id, amount),
                ],
                ..Default::default()
            };
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(monoize::replica::metering::METERING_INGEST_PATH)
                        .header("authorization", "Bearer tok")
                        .header("x-monoize-replica-id", TEST_REPLICA_ID)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&batch).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "metering_batch_invalid");
            assert_negative_delta_left_balances_unchanged(&state, &user_id, &api_key_id).await;
        }
    }
}

#[tokio::test]
async fn shared_metering_apply_rejects_negative_charge_without_partial_apply() {
    let (_temp, state, user_id, api_key_id) = balance_delta_fixture().await;
    for (kind, key_id) in [
        ("request_charge", None),
        ("api_key_charge", Some(api_key_id.as_str())),
    ] {
        for amount in [-1, i128::MIN] {
            let batch = MeteringBatch {
                balance_deltas: vec![
                    delta("request_charge", &user_id, None, 1),
                    delta(kind, &user_id, key_id, amount),
                ],
                ..Default::default()
            };
            assert!(apply_metering_batch(&state.db_pool, &batch).await.is_err());
            assert_negative_delta_left_balances_unchanged(&state, &user_id, &api_key_id).await;
        }
    }
}

#[tokio::test]
async fn delta_enqueue_rejects_negative_charge_before_spool_or_pending_mutation() {
    let temp = TempDir::new().unwrap();
    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        "http://127.0.0.1:9",
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .unwrap();
    for (kind, key_id, subject) in [
        ("request_charge", None, "user"),
        ("api_key_charge", Some("key"), "key"),
    ] {
        assert!(
            metering
                .enqueue_balance_delta(kind, "user", key_id, -1, &serde_json::json!({}))
                .await
                .is_err()
        );
        assert!(
            metering
                .delta_spool()
                .enqueue(&delta(kind, "user", key_id, -1))
                .await
                .is_err()
        );
        assert_eq!(metering.pending().outstanding(subject), 0);
        assert_eq!(metering.delta_spool().pending_files(), 0);
        assert_eq!(metering.spool_capacity().accounted_bytes(), 0);
    }
}

#[tokio::test]
async fn persisted_negative_charge_blocks_replica_startup_and_promotion_drain() {
    let (temp, state, user_id, api_key_id) = balance_delta_fixture().await;
    let spool_dir = temp.path().join("negative-delta-spool");
    std::fs::create_dir(&spool_dir).unwrap();
    let spool_path = spool_dir.join("00000000000000000001-negative.json");
    let negative = delta("request_charge", &user_id, None, -1);
    std::fs::write(&spool_path, serde_json::to_vec(&negative).unwrap()).unwrap();
    assert!(
        ReplicaMetering::new(
            spool_dir.clone(),
            1024 * 1024,
            "http://127.0.0.1:9",
            "token",
            10,
            TEST_REPLICA_ID.to_string(),
        )
        .is_err()
    );
    assert!(spool_path.exists());
    let spool = DeltaSpool::new(spool_dir, 1024 * 1024).unwrap();
    assert!(
        drain_delta_spool_to_local_db(&state.db_pool, &spool)
            .await
            .is_err()
    );
    assert!(spool_path.exists());
    assert_negative_delta_left_balances_unchanged(&state, &user_id, &api_key_id).await;
}

#[tokio::test]
async fn persisted_negative_charge_with_invalid_subject_blocks_startup() {
    for kind in ["invalid_charge", "api_key_charge"] {
        let temp = TempDir::new().unwrap();
        let spool_path = temp.path().join("00000000000000000001-negative.json");
        let negative = delta(kind, "user", None, -1);
        std::fs::write(&spool_path, serde_json::to_vec(&negative).unwrap()).unwrap();
        assert!(
            ReplicaMetering::new(
                temp.path().to_path_buf(),
                1024 * 1024,
                "http://127.0.0.1:9",
                "token",
                10,
                TEST_REPLICA_ID.to_string(),
            )
            .is_err()
        );
        assert!(spool_path.exists());
    }
}

#[tokio::test]
async fn ingest_applies_balance_delta_idempotently() {
    let (_temp, state) = boot().await;
    let user = state
        .user_store
        .create_user("delta_user", "pw", monoize::users::UserRole::User, None)
        .await
        .expect("user");
    state
        .user_store
        .update_user(
            &user.id,
            None,
            None,
            None,
            None,
            Some("5000000000"),
            None,
            None,
            None,
        )
        .await
        .expect("seed balance");

    let batch = monoize::replica::metering::MeteringBatch {
        replica: None,
        request_logs: vec![],
        last_used: vec![],
        plan_terminals: vec![],
        balance_deltas: vec![delta("request_charge", &user.id, None, 1_000_000_000)],
    };

    // T2 first delivery: one ledger row, balance reduced.
    let ack1 = apply_metering_batch(&state.db_pool, &batch)
        .await
        .expect("apply");
    assert_eq!(ack1.applied_balance_deltas, 1);
    let balance = state
        .user_store
        .get_user_balance(&user.id)
        .await
        .expect("balance")
        .expect("row");
    assert_eq!(balance.balance_nano_usd, 4_000_000_000);

    // I6 replay: nothing changes, counts report zero new applies.
    let ack2 = apply_metering_batch(&state.db_pool, &batch)
        .await
        .expect("replay");
    assert_eq!(ack2.applied_balance_deltas, 0);
    assert_eq!(ack2.applied_request_logs, 0);
    let balance2 = state
        .user_store
        .get_user_balance(&user.id)
        .await
        .expect("balance")
        .unwrap();
    assert_eq!(balance2.balance_nano_usd, 4_000_000_000);
}

#[tokio::test]
async fn ingest_allows_negative_result_and_counts_unlimited_as_applied_without_update() {
    let (_temp, state) = boot().await;
    let limited = state
        .user_store
        .create_user("limited", "pw", monoize::users::UserRole::User, None)
        .await
        .expect("limited");
    let unlimited = state
        .user_store
        .create_user("unl", "pw", monoize::users::UserRole::User, None)
        .await
        .expect("unlimited");
    state
        .user_store
        .update_user(
            &unlimited.id,
            None,
            None,
            None,
            None,
            Some("0"),
            Some(true),
            None,
            None,
        )
        .await
        .expect("make unlimited");

    // T3 negative result allowed on the limited user.
    let batch = monoize::replica::metering::MeteringBatch {
        replica: None,
        request_logs: vec![],
        last_used: vec![],
        plan_terminals: vec![],
        balance_deltas: vec![delta("request_charge", &limited.id, None, 100)],
    };
    let ack = apply_metering_batch(&state.db_pool, &batch)
        .await
        .expect("apply");
    assert_eq!(ack.applied_balance_deltas, 1);
    let bal = state
        .user_store
        .get_user_balance(&limited.id)
        .await
        .expect("bal")
        .unwrap();
    assert_eq!(bal.balance_nano_usd, -100);

    // T3 unlimited owner: ledger event recorded but balance untouched.
    let batch_u = monoize::replica::metering::MeteringBatch {
        replica: None,
        request_logs: vec![],
        last_used: vec![],
        plan_terminals: vec![],
        balance_deltas: vec![delta("request_charge", &unlimited.id, None, 77)],
    };
    let ack_u = apply_metering_batch(&state.db_pool, &batch_u)
        .await
        .expect("apply u");
    assert_eq!(ack_u.applied_balance_deltas, 1);
    let bal_u = state
        .user_store
        .get_user_balance(&unlimited.id)
        .await
        .expect("bal u")
        .unwrap();
    assert_eq!(bal_u.balance_nano_usd, 0);
}

/// T4: shipment releases spool data only after an HTTP 200 and retains it otherwise.
#[tokio::test]
async fn shipper_acks_delete_and_failures_retain() {
    let temp = TempDir::new().unwrap();
    let spool_dir = temp.path().join("metering");

    // Failing primary first.
    let failing = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fail_flag = failing.clone();
    let fail_app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(move |body: axum::body::Bytes| async move {
            fail_flag.fetch_add(1, Ordering::SeqCst);
            let batch: monoize::replica::metering::MeteringBatch =
                serde_json::from_slice(&body).unwrap();
            assert_eq!(batch.balance_deltas.len(), 1);
            Err::<axum::response::Response, _>((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "boom",
            ))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, fail_app).await.unwrap() });

    let metering = ReplicaMetering::new(
        spool_dir.clone(),
        1024 * 1024,
        &addr,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .expect("metering");
    metering
        .enqueue_balance_delta("request_charge", "u-1", None, 1234, &serde_json::json!({}))
        .await
        .expect("enqueue");
    assert_eq!(
        metering.pending().outstanding("u-1"),
        1234,
        "M3 pending counter increments on enqueue"
    );

    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(16);

    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Failure
    );
    assert_eq!(failing.load(Ordering::SeqCst), 1, "one POST attempt made");
    assert_eq!(
        metering.pending().outstanding("u-1"),
        1234,
        "M5 failure retains the pending counter"
    );
    assert_eq!(
        metering.delta_spool().pending_files(),
        1,
        "M5 failure retains the durable delta file"
    );

    // Succeeding primary next: same data ships and is released only after 200.
    let ok_app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(|body: axum::body::Bytes| async move {
            let batch: monoize::replica::metering::MeteringBatch =
                serde_json::from_slice(&body).unwrap();
            assert_eq!(batch.balance_deltas[0].amount_nano_usd, "1234");
            axum::Json(monoize::replica::metering::MeteringAck {
                applied_request_logs: batch.request_logs.len() as u64,
                applied_last_used: batch.last_used.len() as u64,
                applied_balance_deltas: batch.balance_deltas.len() as u64,
                plan_terminal_acks: vec![],
            })
        }),
    );
    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = format!("http://{}", listener2.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener2, ok_app).await.unwrap() });

    let metering_ok = ReplicaMetering::new(
        spool_dir,
        1024 * 1024,
        &addr2,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .expect("metering 2");
    assert_eq!(
        metering_ok.pending().outstanding("u-1"),
        1234,
        "restart restores pending deductions from durable spool"
    );
    assert_eq!(
        metering_ok.ship_once(&log_batcher, &last_used).await,
        ShipTick::Success
    );
    assert_eq!(
        metering_ok.delta_spool().pending_files(),
        0,
        "successful ack deletes the durable file"
    );
    assert_eq!(
        metering_ok.pending().outstanding("u-1"),
        0,
        "successful ack clears the pending deduction"
    );
}

/// T7: a promoted node drains leftover deltas into its own database before serving.
#[tokio::test]
async fn promotion_drain_applies_leftover_deltas_locally() {
    let temp = TempDir::new().unwrap();
    let dsn = format!("sqlite://{}", temp.path().join("m.db").display());
    let state = monoize::app::load_state_with_runtime(test_runtime(dsn))
        .await
        .expect("state");
    let user = state
        .user_store
        .create_user("drain_user", "pw", monoize::users::UserRole::User, None)
        .await
        .expect("user");

    let spool_dir = temp.path().join("leftover-metering");
    std::fs::create_dir_all(&spool_dir).unwrap();
    let spool = DeltaSpool::new(spool_dir.clone(), 1024 * 1024).unwrap();
    spool
        .enqueue(&delta("request_charge", &user.id, None, 250))
        .await
        .expect("enqueue leftover");
    assert_eq!(spool.pending_files(), 1);

    drain_delta_spool_to_local_db(&state.db_pool, &spool)
        .await
        .expect("drain");
    assert_eq!(spool.pending_files(), 0, "PRP9 drain empties the spool");
    let balance = state
        .user_store
        .get_user_balance(&user.id)
        .await
        .expect("bal")
        .unwrap();
    assert_eq!(balance.balance_nano_usd, -250);
}

fn dummy_request_log() -> monoize::users::InsertRequestLog {
    monoize::users::InsertRequestLog {
        request_id: Some(uuid::Uuid::new_v4().to_string()),
        user_id: "u".to_string(),
        api_key_id: None,
        model: "m".to_string(),
        provider_id: None,
        upstream_model: None,
        upstream_response_model: None,
        channel_id: None,
        names: monoize::users::RequestLogNameSnapshots::default(),
        is_stream: false,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        tool_prompt_tokens: None,
        reasoning_tokens: None,
        accepted_prediction_tokens: None,
        rejected_prediction_tokens: None,
        provider_multiplier: None,
        charge_nano_usd: None,
        status: monoize::users::REQUEST_LOG_STATUS_SUCCESS.to_string(),
        usage_breakdown_json: None,
        billing_breakdown_json: None,
        error_code: None,
        error_message: None,
        error_http_status: None,
        duration_ms: None,
        ttfb_ms: None,
        request_ip: None,
        reasoning_effort: None,
        tried_providers_json: None,
        request_kind: None,
        effective_provider_type: None,
        affinity_hit: None,
        affinity_key_hash: None,
        affinity_target: None,
        session_affinity_value: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn shipper_idle_tick_is_not_a_failure() {
    let temp = TempDir::new().unwrap();
    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        "http://127.0.0.1:1",
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .expect("metering");
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(16);
    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Idle
    );
}

#[tokio::test]
async fn shipper_does_not_post_a_second_batch_after_combined_failure() {
    let temp = TempDir::new().unwrap();
    let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let posts_flag = posts.clone();
    let fail_app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(move |_body: axum::body::Bytes| async move {
            posts_flag.fetch_add(1, Ordering::SeqCst);
            Err::<axum::response::Response, _>((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "boom",
            ))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, fail_app).await.unwrap() });

    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        &addr,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .expect("metering");
    metering
        .enqueue_balance_delta("request_charge", "u-1", None, 9, &serde_json::json!({}))
        .await
        .expect("enqueue");
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    log_batcher
        .push(dummy_request_log())
        .await
        .expect("push log");
    let last_used = LastUsedBatcher::with_capacity(16);
    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Failure
    );
    assert_eq!(
        posts.load(Ordering::SeqCst),
        1,
        "M4 allows at most one POST"
    );
}

#[tokio::test]
async fn shipper_caps_last_used_so_batch_stays_under_hard_cap() {
    let temp = TempDir::new().unwrap();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_flag = seen.clone();
    let ok_app = Router::new().route(
        monoize::replica::metering::METERING_INGEST_PATH,
        post(move |body: axum::body::Bytes| async move {
            let batch: monoize::replica::metering::MeteringBatch =
                serde_json::from_slice(&body).unwrap();
            seen_flag.store(batch.last_used.len(), Ordering::SeqCst);
            assert!(
                batch.request_logs.len()
                    + batch.plan_terminals.len()
                    + batch.balance_deltas.len()
                    + batch.last_used.len()
                    <= monoize::replica::metering::METERING_BATCH_HARD_CAP
            );
            axum::Json(monoize::replica::metering::MeteringAck {
                applied_request_logs: batch.request_logs.len() as u64,
                applied_last_used: batch.last_used.len() as u64,
                applied_balance_deltas: batch.balance_deltas.len() as u64,
                plan_terminal_acks: vec![],
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, ok_app).await.unwrap() });

    let metering = ReplicaMetering::new(
        temp.path().join("metering"),
        1024 * 1024,
        &addr,
        "token",
        10,
        TEST_REPLICA_ID.to_string(),
    )
    .expect("metering");
    let log_batcher = RequestLogBatcher::new_with_limits(
        8,
        temp.path().join("rl-spool"),
        64 * 1024 * 1024,
        8 * 1024 * 1024,
        broadcast::channel(4).0,
        Arc::new(dashmap::DashMap::new()),
    );
    let last_used = LastUsedBatcher::with_capacity(3000);
    let now = chrono::Utc::now();
    for i in 0..2001 {
        last_used.record(format!("k{i}"), now);
    }
    assert_eq!(
        metering.ship_once(&log_batcher, &last_used).await,
        ShipTick::Success
    );
    assert_eq!(
        seen.load(Ordering::SeqCst),
        monoize::replica::metering::METERING_BATCH_HARD_CAP
    );
}

#[tokio::test]
async fn t3_ingest_rejects_oversized_batch_without_apply() {
    let (_temp, state) = boot().await;
    let mut node = (*state.node).clone();
    node.replica_token = Some("tok".to_string());
    let mut state = state.with_node_role(NodeRole::Primary);
    state.node = std::sync::Arc::new(node);
    state.metering_token_digest = Some(monoize::replica::metering::sha256_hex_lower("tok"));
    let app = monoize::app::build_app(state);
    let replica_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let batch = monoize::replica::metering::MeteringBatch {
        replica: Some(ReplicaHeartbeat {
            id: replica_id.to_string(),
            hostname: "replica-a".to_string(),
            listen: "127.0.0.1:8080".to_string(),
            version: "test".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            uptime_seconds: 0,
            spool_pending_count: 0,
            spool_pending_bytes: 0,
        }),
        request_logs: vec![],
        last_used: (0..2001)
            .map(|i| monoize::replica::metering::LastUsedPair {
                api_key_id: format!("k{i}"),
                last_used_at: chrono::Utc::now().to_rfc3339(),
            })
            .collect(),
        plan_terminals: vec![],
        balance_deltas: vec![],
    };
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(monoize::replica::metering::METERING_INGEST_PATH)
                .header("authorization", "Bearer tok")
                .header("x-monoize-replica-id", replica_id)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&batch).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "metering_batch_too_large");
}

fn metering_terminal(audience: &str) -> PlanTerminalWire {
    let mut input = TerminalApplyInput {
        token_id: "missing-token".to_string(),
        reservation_id: "missing-reservation".to_string(),
        request_id: "missing-request".to_string(),
        audience: audience.to_string(),
        kind: TerminalKind::Release,
        actual_nano_usd: None,
        canonical_digest: String::new(),
        applied_at: chrono::Utc::now(),
    };
    input.canonical_digest = terminal_digest(&input).unwrap();
    PlanTerminalWire {
        version: 1,
        token_id: input.token_id,
        reservation_id: input.reservation_id,
        request_id: input.request_id,
        audience: input.audience,
        kind: input.kind,
        actual_nano_usd: None,
        canonical_digest: input.canonical_digest,
        created_at: input.applied_at,
    }
}

#[tokio::test]
async fn metering_plan_terminal_requires_matching_replica_audience() {
    let (_temp, state) = boot().await;
    let mut node = (*state.node).clone();
    node.replica_token = Some("tok".to_string());
    let mut state = state.with_node_role(NodeRole::Primary);
    state.node = Arc::new(node);
    state.metering_token_digest = Some(monoize::replica::metering::sha256_hex_lower("tok"));
    let app = monoize::app::build_app(state);
    let header_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let terminal_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let batch = MeteringBatch {
        plan_terminals: vec![metering_terminal(terminal_id)],
        ..Default::default()
    };
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(monoize::replica::metering::METERING_INGEST_PATH)
                .header("authorization", "Bearer tok")
                .header("x-monoize-replica-id", header_id)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&batch).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "replica_audience_mismatch");
}

#[tokio::test]
async fn metering_plan_terminal_maps_unknown_token_and_invalid_digest() {
    let (_temp, state) = boot().await;
    let mut node = (*state.node).clone();
    node.replica_token = Some("tok".to_string());
    let mut state = state.with_node_role(NodeRole::Primary);
    state.node = Arc::new(node);
    state.metering_token_digest = Some(monoize::replica::metering::sha256_hex_lower("tok"));
    let app = monoize::app::build_app(state);
    let replica_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let request = |terminal: PlanTerminalWire| {
        Request::builder()
            .method("POST")
            .uri(monoize::replica::metering::METERING_INGEST_PATH)
            .header("authorization", "Bearer tok")
            .header("x-monoize-replica-id", replica_id)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&MeteringBatch {
                    plan_terminals: vec![terminal],
                    ..Default::default()
                })
                .unwrap(),
            ))
            .unwrap()
    };

    let unknown = app
        .clone()
        .oneshot(request(metering_terminal(replica_id)))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let mut invalid = metering_terminal(replica_id);
    invalid.canonical_digest = "0".repeat(64);
    let invalid = app.oneshot(request(invalid)).await.unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = invalid.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "admission_terminal_digest_invalid");
}

#[tokio::test]
async fn t5_epoch_bump_poll_swap_and_failed_poll_keeps_snapshot() {
    let (_temp, state) = boot().await;
    let snapshot = std::sync::Arc::new(tokio::sync::RwLock::new(
        state.monoize_runtime.read().await.clone(),
    ));
    let mut last = 0u64;
    monoize::replica::poll::apply_config_epoch_tick(
        &state.db_pool,
        &state.settings_store,
        &snapshot,
        &mut last,
    )
    .await;
    assert_eq!(last, 0);

    let mut settings = state.settings_store.get_all().await.expect("settings");
    settings.monoize_request_timeout_ms = 12_345;
    state
        .settings_store
        .update_all(&settings)
        .await
        .expect("update");
    assert_eq!(
        monoize::settings::read_config_epoch(&state.db_pool)
            .await
            .expect("epoch"),
        1
    );

    monoize::replica::poll::apply_config_epoch_tick(
        &state.db_pool,
        &state.settings_store,
        &snapshot,
        &mut last,
    )
    .await;
    assert_eq!(last, 1);
    assert_eq!(snapshot.read().await.request_timeout_ms, 12_345);

    let write = state.db_pool.write().await;
    write
        .execute(state.db_pool.stmt(
            "UPDATE state_records SET value = $1 WHERE tenant_id = $2 AND kind = $3 AND id = $4",
            vec![
                "not-a-u64".into(),
                "monoize".into(),
                "config_epoch".into(),
                "global".into(),
            ],
        ))
        .await
        .expect("corrupt epoch");
    drop(write);

    monoize::replica::poll::apply_config_epoch_tick(
        &state.db_pool,
        &state.settings_store,
        &snapshot,
        &mut last,
    )
    .await;
    assert_eq!(last, 1, "failed poll keeps last applied epoch");
    assert_eq!(
        snapshot.read().await.request_timeout_ms,
        12_345,
        "failed poll keeps prior snapshot"
    );
}

#[tokio::test]
async fn t6_replica_surface_disables_dashboard_and_keeps_api() {
    let (_temp, state) = boot().await;
    let app = monoize::app::build_app(state.with_node_role(NodeRole::Replica));

    async fn json_code(app: axum::Router, uri: &str) -> (axum::http::StatusCode, String) {
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
        (
            status,
            json["error"]["code"].as_str().unwrap_or("").to_string(),
        )
    }

    let (status, code) = json_code(app.clone(), "/").await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    assert_eq!(code, "replica_dashboard_disabled");

    let (status, code) = json_code(app.clone(), "/api/dashboard/auth/me").await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    assert_eq!(code, "replica_dashboard_disabled");

    let models = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        models
            .into_body()
            .collect()
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes.to_bytes()).ok())
            .and_then(|json| json["error"]["code"].as_str().map(str::to_string))
            .unwrap_or_default(),
        "replica_dashboard_disabled"
    );

    let metrics = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metrics.status(), axum::http::StatusCode::UNAUTHORIZED);
}

/// T9: the persisted identity survives simulated restarts and DeltaSpool cleanup.
#[test]
fn t9_replica_identity_created_once_and_reused_across_restarts() {
    let temp = TempDir::new().unwrap();
    let spool_dir = temp.path().join("metering");

    let first = resolve_replica_identity(None, &spool_dir).expect("first resolution");
    let parsed = uuid::Uuid::parse_str(&first).expect("identity is a UUID");
    assert_eq!(parsed.get_version_num(), 4, "identity is version 4");

    let identity_path = spool_dir.join(REPLICA_IDENTITY_FILE_NAME);
    let content = std::fs::read_to_string(&identity_path).expect("identity file exists");
    assert_eq!(content, format!("{first}\n"), "M9a file format");

    // Simulated restart: resolution over the same directory returns the same identity.
    let second = resolve_replica_identity(None, &spool_dir).expect("second resolution");
    assert_eq!(first, second);

    // M9b: DeltaSpool startup cleanup removes non-json leftovers but keeps the identity.
    std::fs::write(spool_dir.join(".tmp-leftover"), b"junk").unwrap();
    DeltaSpool::new(spool_dir.clone(), 1024 * 1024).expect("spool");
    assert!(identity_path.exists(), "identity survives spool cleanup");
    assert!(
        !spool_dir.join(".tmp-leftover").exists(),
        "temp leftovers are still cleaned"
    );
    let third = resolve_replica_identity(None, &spool_dir).expect("third resolution");
    assert_eq!(first, third);
}

/// T9: `MONOIZE_REPLICA_ID` overrides the file and is validated as a UUID v4.
#[test]
fn t9_replica_identity_env_override_and_validation() {
    let temp = TempDir::new().unwrap();
    let spool_dir = temp.path().join("metering");

    let configured = uuid::Uuid::new_v4();
    let resolved =
        resolve_replica_identity(Some(&configured.to_string().to_uppercase()), &spool_dir)
            .expect("override resolves");
    assert_eq!(
        resolved,
        configured.hyphenated().to_string(),
        "override canonicalizes to lowercase hyphenated form"
    );
    assert!(
        !spool_dir.join(REPLICA_IDENTITY_FILE_NAME).exists(),
        "override neither reads nor writes the identity file"
    );

    let err = resolve_replica_identity(Some("not-a-uuid"), &spool_dir).unwrap_err();
    assert!(err.starts_with("replica_id_invalid"), "{err}");
    // Nil UUID parses but is not version 4.
    let err = resolve_replica_identity(Some("00000000-0000-0000-0000-000000000000"), &spool_dir)
        .unwrap_err();
    assert!(err.starts_with("replica_id_invalid"), "{err}");
}

#[test]
fn replica_metering_uses_one_explicit_identity_without_touching_identity_file() {
    let temp = TempDir::new().unwrap();
    let spool_dir = temp.path().join("metering-explicit-identity");
    let configured = uuid::Uuid::new_v4().hyphenated().to_string();
    let metering = ReplicaMetering::new(
        spool_dir.clone(),
        1024 * 1024,
        "http://127.0.0.1:1",
        "token",
        10,
        configured.clone(),
    )
    .unwrap();

    assert_eq!(metering.replica_id(), configured);
    assert_eq!(metering.admission_client().replica_id(), configured);
    assert!(!spool_dir.join(REPLICA_IDENTITY_FILE_NAME).exists());
}

/// T9: a corrupt identity file is replaced by a freshly generated identity.
#[test]
fn t9_replica_identity_regenerates_corrupt_file() {
    let temp = TempDir::new().unwrap();
    let spool_dir = temp.path().join("metering");
    std::fs::create_dir_all(&spool_dir).unwrap();
    let identity_path = spool_dir.join(REPLICA_IDENTITY_FILE_NAME);
    std::fs::write(&identity_path, b"garbage\n").unwrap();

    let regenerated = resolve_replica_identity(None, &spool_dir).expect("regenerates");
    let parsed = uuid::Uuid::parse_str(&regenerated).expect("valid UUID");
    assert_eq!(parsed.get_version_num(), 4);
    assert_eq!(
        std::fs::read_to_string(&identity_path).unwrap(),
        format!("{regenerated}\n"),
        "corrupt file is atomically replaced"
    );
    assert_eq!(
        resolve_replica_identity(None, &spool_dir).unwrap(),
        regenerated
    );
}

fn heartbeat_record(id: &str, last_seen_unix_ms: i64) -> ReplicaHeartbeatRecord {
    ReplicaHeartbeatRecord {
        heartbeat: ReplicaHeartbeat {
            id: id.to_string(),
            hostname: "host".to_string(),
            listen: "0.0.0.0:9000".to_string(),
            version: "1.0.0".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            uptime_seconds: 0,
            spool_pending_count: 0,
            spool_pending_bytes: 0,
        },
        last_seen_unix_ms,
    }
}

/// T10: overview reads evict entries older than 360 ship intervals and keep the rest.
#[test]
fn t10_heartbeat_eviction_removes_only_expired_entries() {
    let ship_interval = std::time::Duration::from_secs(10);
    let evict_after_ms = ship_interval.as_millis() as i64 * HEARTBEAT_EVICT_INTERVALS as i64;
    let now_ms = chrono::Utc::now().timestamp_millis();

    let map = dashmap::DashMap::new();
    map.insert(
        "expired".to_string(),
        heartbeat_record("expired", now_ms - evict_after_ms - 1),
    );
    map.insert(
        "stale-but-kept".to_string(),
        heartbeat_record("stale-but-kept", now_ms - evict_after_ms),
    );
    map.insert("live".to_string(), heartbeat_record("live", now_ms));

    evict_expired_heartbeats(&map, now_ms, ship_interval);

    assert!(!map.contains_key("expired"), "expired entry is removed");
    assert!(
        map.contains_key("stale-but-kept"),
        "boundary entry is retained"
    );
    assert!(map.contains_key("live"), "live entry is retained");
}

#[tokio::test]
async fn t8_postgres_ingest_parity_when_configured() {
    let Some(dsn) = std::env::var("MONOIZE_TEST_POSTGRES_DSN")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    let temp = TempDir::new().unwrap();
    let mut runtime = test_runtime(dsn);
    runtime.node.metering_spool_dir = temp.path().join("metering");
    let state = monoize::app::load_state_with_runtime(runtime)
        .await
        .expect("postgres state loads");

    let column = state
        .db_pool
        .read()
        .query_one(state.db_pool.stmt(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_name = 'billing_ledger' AND column_name = 'idempotency_key'",
            vec![],
        ))
        .await
        .expect("schema query");
    assert!(column.is_some(), "SC1 idempotency_key exists on postgres");

    let user = state
        .user_store
        .create_user(
            &format!("pg_{}", uuid::Uuid::new_v4().simple()),
            "pw",
            monoize::users::UserRole::User,
            None,
        )
        .await
        .expect("user");
    state
        .user_store
        .update_user(
            &user.id,
            None,
            None,
            None,
            None,
            Some("5000000000"),
            None,
            None,
            None,
        )
        .await
        .expect("seed");
    let batch = monoize::replica::metering::MeteringBatch {
        replica: None,
        request_logs: vec![],
        last_used: vec![],
        plan_terminals: vec![],
        balance_deltas: vec![delta("request_charge", &user.id, None, 1_000_000_000)],
    };
    let ack1 = apply_metering_batch(&state.db_pool, &batch)
        .await
        .expect("apply");
    assert_eq!(ack1.applied_balance_deltas, 1);
    let ack2 = apply_metering_batch(&state.db_pool, &batch)
        .await
        .expect("replay");
    assert_eq!(ack2.applied_balance_deltas, 0);
    let balance = state
        .user_store
        .get_user_balance_uncached(&user.id)
        .await
        .expect("balance")
        .expect("row");
    assert_eq!(balance.balance_nano_usd, 4_000_000_000);
}

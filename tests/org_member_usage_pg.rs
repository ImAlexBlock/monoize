use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use monoize::app::{build_app, load_state_with_runtime, RuntimeConfig};
use monoize::users::UserRole;
use sea_orm::ConnectionTrait;
use serde_json::Value;
use tower::ServiceExt;

async fn exercise(dsn: String) {
    if dsn.starts_with("postgres") {
        let db = sea_orm::Database::connect(&dsn).await.unwrap();
        let row = db.query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema='public'",
        )).await.unwrap().unwrap();
        assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0, "refuse populated database");
        db.close().await.unwrap();
    }
    let state = load_state_with_runtime(RuntimeConfig::with_defaults(
        "127.0.0.1:0", "/metrics", dsn,
    )).await.unwrap();
    let owner = state.user_store.create_user("usage_owner", "test-password", UserRole::User, None).await.unwrap();
    let member = state.user_store.create_user("usage_member", "test-password", UserRole::User, None).await.unwrap();
    let session = state.user_store.create_session(&owner.id, 1).await.unwrap();
    let db = state.db_pool.clone();
    let now = chrono::Utc::now();
    let stamp = now.to_rfc3339();
    for (statement, values) in [
        ("INSERT INTO orgs(id,owner_user_id,display_name,avatar_emoji,avatar_color,invite_token,invite_created_at,created_at,updated_at)
          VALUES('org-test',$1,'Test','X','#000000','test-invite',$2,$2,$2)",
         vec![owner.id.clone().into(),stamp.clone().into()]),
        ("INSERT INTO users(id,username,password_hash,role,created_at,updated_at,enabled,is_org)
          VALUES('org-test','_monoize_org_test','unused','user',$1,$1,1,1)",
         vec![stamp.clone().into()]),
        ("INSERT INTO org_members(org_id,user_id,role,joined_at) VALUES('org-test',$1,'owner',$3),('org-test',$2,'member',$3)",
         vec![owner.id.clone().into(),member.id.clone().into(),stamp.clone().into()]),
        ("INSERT INTO api_keys(id,user_id,name,key_prefix,key,created_at,enabled,sub_account_enabled,sub_account_balance_nano,
          model_limits_enabled,model_limits,ip_whitelist,group_ids,channel_bindings,model_bindings,transforms,
          model_redirects,reasoning_envelope_enabled,request_capture_enabled,org_id,created_by)
          VALUES('org-key','org-test','test','sk-test','sk-test-unused',$1,1,0,'0',0,'[]','[]','[]','[]','[]','[]','[]',0,0,'org-test',$2)",
         vec![stamp.clone().into(),member.id.clone().into()]),
    ] {
        db.write().await.execute(db.stmt(statement,values)).await.unwrap();
    }
    for (id,status,input,output,cache,charge) in [
        ("ok","success",Some(3_000_000_030i64),Some(7i64),Some(13i64),"700"),
        ("nulls","success",None,None,None,"0"),
        ("failed","error",Some(900),Some(900),Some(900),"900"),
    ] {
        db.write().await.execute(db.stmt(
            "INSERT INTO request_logs(id,user_id,api_key_id,model,is_stream,status,input_tokens,output_tokens,cache_read_tokens,
             charge_nano_usd,created_at,created_at_unix_ms) VALUES($1,'org-test','org-key','test-model',0,$2,$3,$4,$5,$6,$7,$8)",
            vec![id.into(),status.into(),input.into(),output.into(),cache.into(),charge.into(),stamp.clone().into(),now.timestamp_millis().into()],
        )).await.unwrap();
    }
    let router = build_app(state);
    let read = || async {
        let response = router.clone().oneshot(Request::builder()
            .uri("/api/dashboard/orgs/org-test/member-usage?range_hours=24&buckets=4")
            .header("Authorization",format!("Bearer {}",session.token))
            .body(Body::empty()).unwrap()).await.unwrap();
        let status=response.status();
        let bytes=response.into_body().collect().await.unwrap().to_bytes();
        let body: Value=serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status,StatusCode::OK,"{body}");
        body
    };
    let body=read().await;
    assert_eq!(body["members"].as_array().unwrap().len(),1,"{body}");
    let usage=&body["members"][0];
    assert_eq!(usage["user_id"],member.id);
    assert_eq!(usage["input_tokens"],3_000_000_030i64);
    assert_eq!(usage["output_tokens"],7);
    assert_eq!(usage["cache_read_tokens"],13);
    assert_eq!(usage["total_charge_nano_usd"],"700");
    assert_eq!(usage["calls"],3);
    assert!(body["removed_members"].as_array().unwrap().is_empty());
    db.write().await.execute(db.stmt("DELETE FROM org_members WHERE org_id='org-test' AND user_id=$1",
        vec![member.id.into()])).await.unwrap();
    let removed=read().await;
    assert!(removed["members"].as_array().unwrap().is_empty());
    assert_eq!(removed["removed_members"][0]["input_tokens"],3_000_000_030i64);
}

#[tokio::test]
async fn sqlite_member_usage_endpoint_preserves_totals() {
    exercise("sqlite::memory:".into()).await;
}

#[tokio::test]
async fn postgres_member_usage_endpoint_preserves_totals() {
    exercise(std::env::var("MONOIZE_TEST_POSTGRES_DSN")
        .expect("explicit disposable PostgreSQL DSN required")).await;
}

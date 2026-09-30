//! Gateway performance budget benchmark (gateway-performance-budget.spec.md).
//!
//! Run explicitly (it is long):
//!   cargo test --release --test gateway_benchmark -- --ignored --nocapture
//! Step duration and rates are env-tunable:
//!   GATEWAY_BENCH_STEP_SECONDS (default 60), GATEWAY_BENCH_SEED_ROWS (default 500000).

use sea_orm::ConnectionTrait;
use std::time::Instant;

include!("api/support.rs");

/// GPB2: upstream TTFB 300 ms, 20 SSE chunks at 50 ms intervals, total ~1.3 s.
const UPSTREAM_TTFB_MS: u64 = 300;
const UPSTREAM_CHUNKS: u64 = 20;
const UPSTREAM_CHUNK_INTERVAL_MS: u64 = 50;
const UPSTREAM_TOTAL_MS: u64 = UPSTREAM_TTFB_MS + UPSTREAM_CHUNKS * UPSTREAM_CHUNK_INTERVAL_MS;

fn bench_step_seconds() -> u64 {
    std::env::var("GATEWAY_BENCH_STEP_SECONDS")
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(60)
}

fn bench_seed_rows() -> usize {
    std::env::var("GATEWAY_BENCH_SEED_ROWS")
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(500_000)
}

/// Fixed-timing SSE chat upstream (GPB2): the upstream is never the bottleneck.
async fn start_fixed_upstream() -> SocketAddr {
    async fn chat_completions() -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
        eprintln!("UPSTREAM HIT");
        let stream = futures_util::stream::unfold(
            (0u64, tokio::time::Instant::now()),
            |(index, started)| async move {
                if index == 0 {
                    tokio::time::sleep_until(started + Duration::from_millis(UPSTREAM_TTFB_MS))
                        .await;
                } else if index <= UPSTREAM_CHUNKS {
                    tokio::time::sleep_until(
                        started
                            + Duration::from_millis(
                                UPSTREAM_TTFB_MS + index * UPSTREAM_CHUNK_INTERVAL_MS,
                            ),
                    )
                    .await;
                } else {
                    return None;
                }
                let data = if index < UPSTREAM_CHUNKS {
                    json!({
                        "id": "chatcmpl_bench",
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": "gpt-5-mini-chat",
                        "choices": [{
                            "index": 0,
                            "delta": { "role": "assistant", "content": "x" },
                            "finish_reason": Value::Null
                        }]
                    })
                } else if index == UPSTREAM_CHUNKS {
                    json!({
                        "id": "chatcmpl_bench",
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": "gpt-5-mini-chat",
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "stop"
                        }],
                        "usage": { "prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30 }
                    })
                } else {
                    Value::String("[DONE]".to_string())
                };
                Some((Ok(Event::default().data(data.to_string())), (index + 1, started)))
            },
        );
        Sse::new(stream)
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixed upstream");
    let address = listener.local_addr().expect("fixed upstream address");
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(chat_completions)),
        )
        .await
        .expect("serve fixed upstream");
    });
    address
}

/// GPB9: seed request_logs history so spend-window aggregates exercise a
/// realistic depth. The leading 90% of rows go to the concentrated key.
async fn seed_request_logs(ctx: &TestContext, total_rows: usize) {
    let concentrated = total_rows * 9 / 10;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let chunk = 1_000usize;
    let mut row = 0usize;
    while row < total_rows {
        let end = (row + chunk).min(total_rows);
        let tx = ctx.state.db_pool.begin_write().await.expect("begin seed tx");
        for index in row..end {
            let api_key_id = if index < concentrated {
                ctx.api_key_id.as_str()
            } else {
                "seed-other-key"
            };
            let created_ms = now_ms - ((index as i64 + 1) * 60);
            tx.execute(ctx.state.db_pool.stmt(
                "INSERT OR IGNORE INTO request_logs
                    (id, user_id, api_key_id, model, status, is_stream, input_tokens, output_tokens,
                     charge_nano_usd, duration_ms, ttfb_ms, created_at, created_at_unix_ms)
                 VALUES ($1, 'seed-user', $2, 'gpt-5-mini-chat', 'success', 1, 100, 50,
                     '1000', 1200, 400, $3, $4)",
                vec![
                    format!("seed-{index}").into(),
                    api_key_id.into(),
                    chrono::DateTime::from_timestamp_millis(created_ms)
                        .expect("seed created_at")
                        .to_rfc3339()
                        .into(),
                    created_ms.into(),
                ],
            ))
            .await
            .expect("seed insert");
        }
        tx.commit().await.expect("commit seed tx");
        row = end;
        if row % 50_000 == 0 {
            eprintln!("seeded {row}/{total_rows} rows");
        }
    }
    eprintln!("seed complete: {total_rows} rows");
}

#[derive(Default)]
struct StepStats {
    samples: Vec<(f64, f64)>, // (ttfb_ms, total_ms)
    http_5xx: u64,
    gateway_saturated: u64,
    other_errors: u64,
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64) * fraction).ceil() as usize - 1;
    sorted[index.min(sorted.len() - 1)]
}

async fn run_step(
    client: &reqwest::Client,
    base_url: &str,
    auth_header: &str,
    rpm: u32,
    seconds: u64,
    stats: &Arc<tokio::sync::Mutex<StepStats>>,
) {
    let interval = Duration::from_nanos(60_000_000_000u64 / rpm as u64);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume the immediate tick
    let mut tasks = tokio::task::JoinSet::new();
    while Instant::now() < deadline {
        ticker.tick().await;
        let client = client.clone();
        let url = format!("{base_url}/v1/chat/completions");
        let auth = auth_header.to_string();
        let stats = stats.clone();
        tasks.spawn(async move {
            use futures_util::StreamExt;
            let started = Instant::now();
            let mut ttfb: Option<Duration> = None;
            let outcome = async {
                let response = client
                    .post(&url)
                    .header("authorization", &auth)
                    .json(&json!({
                        "model": "gpt-5-mini-chat",
                        "stream": true,
                        "messages": [{ "role": "user", "content": "bench" }]
                    }))
                    .send()
                    .await
                    .map_err(|error| error.to_string())?;
                let status = response.status();
                let mut stream = response.bytes_stream();
                let mut body = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|error| error.to_string())?;
                    if ttfb.is_none() {
                        ttfb = Some(started.elapsed());
                    }
                    body.extend_from_slice(&chunk);
                }
                if !status.is_success() {
                    return Err(format!(
                        "http {status}: {}",
                        String::from_utf8_lossy(&body[..body.len().min(500)])
                    ));
                }
                Ok::<(), String>(())
            }
            .await;
            let total = started.elapsed();
            let mut stats = stats.lock().await;
            match outcome {
                Ok(()) => stats.samples.push((
                    ttfb.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0),
                    total.as_secs_f64() * 1000.0,
                )),
                Err(message) => {
                    if !message.contains("http 4") && stats.other_errors < 3 {
                        eprintln!("REQUEST FAILED: {message}");
                    }
                    if message.contains("http 503") {
                        stats.gateway_saturated += 1;
                    } else if message.contains("http 5") {
                        stats.http_5xx += 1;
                    } else {
                        stats.other_errors += 1;
                    }
                }
            }
        });
    }
    while tasks.join_next().await.is_some() {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "long-running gateway benchmark; run explicitly with --ignored --nocapture"]
async fn gateway_benchmark_ramp() {
    monoize::monoize_routing::set_allow_private_upstream_override(true);
    let ctx = setup().await;

    // Point the chat channel at the fixed-timing upstream (direct column update
    // plus a generation bump keeps this benchmark self-contained).
    let upstream_base = format!("http://{}", start_fixed_upstream().await);
    let providers = ctx.state.monoize_store.list_providers().await.expect("list providers");
    let chat_provider = providers
        .iter()
        .find(|provider| provider.name == "up-chat")
        .expect("up-chat provider registered");
    ctx.state
        .db_pool
        .write()
        .await
        .execute(ctx.state.db_pool.stmt(
            "UPDATE monoize_providers SET channel_base_url = $2 WHERE id = $1",
            vec![chat_provider.id.clone().into(), upstream_base.into()],
        ))
        .await
        .expect("retarget chat channel base_url");
    monoize::monoize_routing::bump_registry_generation();

    seed_request_logs(&ctx, bench_seed_rows()).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gateway");
    let gateway_addr = listener.local_addr().expect("gateway address");
    let router = ctx.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve gateway");
    });

    let client = reqwest::Client::builder()
        .no_proxy()
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("bench client");
    let base_url = format!("http://{gateway_addr}");

    let step_seconds = bench_step_seconds();
    let mut summary = serde_json::Map::new();
    summary.insert(
        "config".to_string(),
        json!({
            "step_seconds": step_seconds,
            "seed_rows": bench_seed_rows(),
            "upstream_total_ms": UPSTREAM_TOTAL_MS,
            "upstream_ttfb_ms": UPSTREAM_TTFB_MS,
        }),
    );
    let mut steps = Vec::new();
    for rpm in [100u32, 500, 1000, 2000] {
        let stats = Arc::new(tokio::sync::Mutex::new(StepStats::default()));
        run_step(&client, &base_url, &ctx.auth_header, rpm, step_seconds, &stats).await;
        let stats = stats.lock().await;
        let mut ttfb: Vec<f64> = stats.samples.iter().map(|s| s.0).collect();
        ttfb.sort_by(|a, b| a.partial_cmp(b).expect("ttfb ordering"));
        let mut overhead: Vec<f64> = stats
            .samples
            .iter()
            .map(|s| (s.1 - UPSTREAM_TOTAL_MS as f64).max(0.0))
            .collect();
        overhead.sort_by(|a, b| a.partial_cmp(b).expect("overhead ordering"));
        let served = stats.samples.len() as u64;
        let offered = served + stats.gateway_saturated + stats.http_5xx + stats.other_errors;
        let step = json!({
            "rpm": rpm,
            "offered": offered,
            "served": served,
            "gateway_saturated": stats.gateway_saturated,
            "http_5xx": stats.http_5xx,
            "other_errors": stats.other_errors,
            "saturated_ratio": if offered > 0 {
                stats.gateway_saturated as f64 / offered as f64
            } else { 0.0 },
            "ttfb_ms": {
                "p50": percentile(&ttfb, 0.50),
                "p90": percentile(&ttfb, 0.90),
                "p99": percentile(&ttfb, 0.99),
                "max": ttfb.last().copied().unwrap_or(0.0),
            },
            "gateway_overhead_ms": {
                "p50": percentile(&overhead, 0.50),
                "p90": percentile(&overhead, 0.90),
                "p99": percentile(&overhead, 0.99),
                "max": overhead.last().copied().unwrap_or(0.0),
            },
        });
        println!("{}", serde_json::to_string_pretty(&step).expect("step json"));
        steps.push(step);
    }
    summary.insert("steps".to_string(), Value::Array(steps));
    println!("=== GATEWAY BENCHMARK SUMMARY ===");
    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Object(summary)).expect("summary json")
    );
}

#[tokio::test]
async fn bench_debug_single_request() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("monoize=debug,warn"))
        .with_writer(std::io::stderr)
        .try_init();
    monoize::monoize_routing::set_allow_private_upstream_override(true);
    let ctx = setup().await;
    let upstream_base = format!("http://{}", start_fixed_upstream().await);
    let providers = ctx.state.monoize_store.list_providers().await.expect("list providers");
    let chat_provider = providers
        .iter()
        .find(|provider| provider.name == "up-chat")
        .expect("up-chat provider registered");
    ctx.state
        .db_pool
        .write()
        .await
        .execute(ctx.state.db_pool.stmt(
            "UPDATE monoize_providers SET channel_base_url = $2 WHERE id = $1",
            vec![chat_provider.id.clone().into(), upstream_base.into()],
        ))
        .await
        .expect("retarget chat channel base_url");
    monoize::monoize_routing::bump_registry_generation();

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, ctx.auth_header.clone())
        .body(Body::from(
            json!({
                "model": "gpt-5-mini-chat",
                "messages": [{ "role": "user", "content": "bench" }],
                "stream": true
            })
            .to_string(),
        ))
        .unwrap();
    let resp = ctx.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    println!("ONESHOT STATUS {status}");
    println!("ONESHOT BODY {text}");

    // Same request through a real server + reqwest client.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = listener.local_addr().expect("gateway address");
    let router = ctx.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve gateway");
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{gateway_addr}/v1/chat/completions");
    let resp = client
        .post(&url)
        .header("authorization", &ctx.auth_header)
        .json(&json!({
            "model": "gpt-5-mini-chat",
            "messages": [{ "role": "user", "content": "bench" }],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    println!("HTTP STATUS {}", resp.status());
    for (name, value) in resp.headers().iter() {
        println!("HTTP HDR {name}: {}", value.to_str().unwrap_or("<bin>"));
    }
    let body = resp.text().await.unwrap();
    println!("HTTP BODY {}", &body[..body.len().min(300)]);
}

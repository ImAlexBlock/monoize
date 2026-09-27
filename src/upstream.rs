use crate::config::{ProviderAuthConfig, ProviderAuthType, ProviderConfig};
use crate::error::AppError;
use axum::http::StatusCode;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum UpstreamErrorKind {
    Network,
    Http,
}

/// SAN-D3 (`spec/upstream-error-sanitization.spec.md`): classifies where the
/// error `message` text came from, which decides how much of it may be shown
/// to downstream clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamErrorSource {
    /// The request could not be sent or its response body could not be read.
    Transport,
    /// Non-2xx upstream response with a parseable `error.message`.
    StructuredBody,
    /// Non-2xx upstream response whose non-empty body had no parseable
    /// `error.message`; `message` carries the raw body for server logs only.
    UnparsedBody,
    /// Non-2xx upstream response with an empty body.
    EmptyBody,
    /// Monoize-generated diagnostic (config, encoding, 2xx decode failures).
    Internal,
}

#[derive(Debug, Clone)]
pub struct UpstreamCallError {
    pub kind: UpstreamErrorKind,
    pub status: Option<StatusCode>,
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub param: Option<String>,
    pub message: String,
    pub source: UpstreamErrorSource,
}

impl UpstreamCallError {
    pub fn new(kind: UpstreamErrorKind, status: Option<StatusCode>, message: String) -> Self {
        // SAN-D3 defaults: network-kind messages are transport diagnostics
        // (reqwest text may embed the upstream URL); HTTP-kind messages built
        // by constructors other than the non-2xx response path are
        // Monoize-generated diagnostics.
        let source = match kind {
            UpstreamErrorKind::Network => UpstreamErrorSource::Transport,
            UpstreamErrorKind::Http => UpstreamErrorSource::Internal,
        };
        Self {
            kind,
            status,
            code: None,
            error_type: None,
            param: None,
            message,
            source,
        }
    }

    pub fn with_error_info(mut self, info: UpstreamErrorInfo) -> Self {
        self.code = info.code;
        self.error_type = info.error_type;
        self.param = info.param;
        self
    }

    pub fn with_source(mut self, source: UpstreamErrorSource) -> Self {
        self.source = source;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct UpstreamErrorInfo {
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub param: Option<String>,
    pub message: Option<String>,
}

/// CP-INV-16a: create-time validation cannot bind a hostname to the address it
/// will reach later, so a Channel whose DNS record changes after creation could
/// still direct Monoize at a private address. Every dispatch re-resolves its
/// target host and rejects a private, loopback, link-local, or reserved result.
/// The decision is cached per origin for a short window, which keeps a hot
/// Channel from paying a resolver round trip on every request while bounding how
/// long a stale decision can outlive a DNS change.
const ADDRESS_GUARD_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);
const ADDRESS_GUARD_CACHE_MAX_ENTRIES: usize = 4096;

static ADDRESS_GUARD_CACHE: std::sync::OnceLock<
    dashmap::DashMap<String, (std::time::Instant, Result<(), String>)>,
> = std::sync::OnceLock::new();

fn address_guard_cache()
-> &'static dashmap::DashMap<String, (std::time::Instant, Result<(), String>)> {
    ADDRESS_GUARD_CACHE.get_or_init(dashmap::DashMap::new)
}

/// Guard one dispatch target. A literal IP is classified directly; a hostname is
/// resolved on the blocking pool because `ToSocketAddrs` blocks. Unresolvable
/// names pass here and fail later as ordinary upstream network errors, which
/// keeps DNS outages from being reported as a policy rejection.
async fn guard_upstream_address(url: &str) -> Result<(), String> {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return Ok(());
    };
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Ok(());
    }
    let Some(host) = parsed.host_str().map(str::to_string) else {
        return Ok(());
    };
    if crate::monoize_routing::private_upstream_addresses_allowed() {
        return Ok(());
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return if crate::monoize_routing::is_private_or_local_ip(ip) {
            Err(private_address_message(ip))
        } else {
            Ok(())
        };
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let key = format!("{host}:{port}");
    if let Some(entry) = address_guard_cache().get(&key)
        && entry.value().0.elapsed() < ADDRESS_GUARD_CACHE_TTL
    {
        return entry.value().1.clone();
    }
    let decision = tokio::task::spawn_blocking(move || resolve_and_classify(&host, port))
        .await
        .unwrap_or(Ok(()));
    let cache = address_guard_cache();
    if cache.len() >= ADDRESS_GUARD_CACHE_MAX_ENTRIES {
        cache.clear();
    }
    cache.insert(key, (std::time::Instant::now(), decision.clone()));
    decision
}

fn resolve_and_classify(host: &str, port: u16) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return Ok(());
    };
    for address in addresses.take(8) {
        if crate::monoize_routing::is_private_or_local_ip(address.ip()) {
            return Err(private_address_message(address.ip()));
        }
    }
    Ok(())
}

fn private_address_message(address: std::net::IpAddr) -> String {
    format!(
        "upstream host resolves to a loopback, link-local, private, or reserved address ({address})"
    )
}

pub async fn call_upstream(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
) -> Result<Value, UpstreamCallError> {
    let resp =
        call_upstream_raw_with_timeout(client, provider, auth_value, path, body, 30_000).await?;
    read_json_response_capped(resp).await
}

// RRB-R1: non-stream upstream bodies are read incrementally with a hard byte cap
// instead of `Response::text()`, so a hostile or broken upstream cannot balloon RAM.
fn upstream_response_max_bytes() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("MONOIZE_UPSTREAM_RESPONSE_MAX_BYTES")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(67_108_864)
    })
}

async fn read_json_response_capped(resp: reqwest::Response) -> Result<Value, UpstreamCallError> {
    let status = resp.status();
    let bytes =
        crate::bounded_response::read_response_body_with_limit(resp, upstream_response_max_bytes())
            .await
            .map_err(|err| {
                UpstreamCallError::new(UpstreamErrorKind::Network, Some(status), err.to_string())
            })?;
    let text = String::from_utf8_lossy(&bytes);
    let value: Value = serde_json::from_str(&text).map_err(|err| {
        UpstreamCallError::new(UpstreamErrorKind::Http, Some(status), err.to_string())
    })?;
    Ok(value)
}

pub async fn call_upstream_raw(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
) -> Result<reqwest::Response, UpstreamCallError> {
    call_upstream_raw_with_timeout(client, provider, auth_value, path, body, 30_000).await
}

pub async fn call_upstream_with_timeout(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
    timeout_ms: u64,
) -> Result<Value, UpstreamCallError> {
    call_upstream_with_timeout_and_headers(
        client,
        provider,
        auth_value,
        path,
        body,
        timeout_ms,
        &[],
    )
    .await
}

pub async fn call_upstream_with_timeout_and_headers(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
    timeout_ms: u64,
    extra_headers: &[(String, String)],
) -> Result<Value, UpstreamCallError> {
    let resp = call_upstream_raw_with_timeout_and_headers(
        client,
        provider,
        auth_value,
        path,
        body,
        timeout_ms,
        extra_headers,
    )
    .await?;
    read_json_response_capped(resp).await
}

pub async fn call_upstream_raw_with_timeout(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
    timeout_ms: u64,
) -> Result<reqwest::Response, UpstreamCallError> {
    call_upstream_raw_with_timeout_and_headers(
        client,
        provider,
        auth_value,
        path,
        body,
        timeout_ms,
        &[],
    )
    .await
}

pub async fn call_upstream_raw_with_timeout_and_headers(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
    timeout_ms: u64,
    extra_headers: &[(String, String)],
) -> Result<reqwest::Response, UpstreamCallError> {
    send_json_with_breakpoint_fallback(provider, body, |body| async move {
        let req =
            prepare_upstream_request(client, provider, auth_value, path, &body, extra_headers)
                .await?;
        send_upstream_request(req.timeout(std::time::Duration::from_millis(timeout_ms))).await
    })
    .await
}

pub async fn call_upstream_stream_with_timeout_and_headers(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
    timeout_ms: u64,
    extra_headers: &[(String, String)],
) -> Result<reqwest::Response, UpstreamCallError> {
    send_json_with_breakpoint_fallback(provider, body, |body| async move {
        let inspect = strip_prompt_cache_breakpoints(&body).is_some();
        let resp = send_upstream_stream_request(
            client,
            provider,
            auth_value,
            path,
            &body,
            timeout_ms,
            extra_headers,
        )
        .await?;
        if inspect {
            prefetch_stream_for_breakpoint_rejection(resp).await
        } else {
            Ok(resp)
        }
    })
    .await
}

const STREAM_PREFETCH_MAX_BYTES: usize = 65_536;
const STREAM_PREFETCH_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

enum PrefetchVerdict {
    Undecided,
    Proceed,
    Rejected(UpstreamCallError),
}

/// ACOTU-24 case analysis over the complete SSE events in `buffer`.
fn classify_stream_prefix(buffer: &[u8]) -> PrefetchVerdict {
    let text = String::from_utf8_lossy(buffer).replace("\r\n", "\n");
    let Some(complete) = text.rfind("\n\n") else {
        return PrefetchVerdict::Undecided;
    };
    for block in text[..complete].split("\n\n") {
        let mut event_name = None;
        let mut data = String::new();
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                event_name = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
        if event_name.is_none() && data.is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        let data_type = parsed.get("type").and_then(Value::as_str);
        let is_error_event = matches!(event_name.as_deref(), Some("error" | "response.failed"))
            || matches!(data_type, Some("error" | "response.failed"))
            || parsed.get("error").is_some_and(Value::is_object);
        if !is_error_event {
            return PrefetchVerdict::Proceed;
        }
        let error = parsed
            .get("error")
            .filter(|value| value.is_object())
            .or_else(|| parsed.get("response").and_then(|r| r.get("error")))
            .unwrap_or(&parsed);
        let param = error.get("param").and_then(Value::as_str);
        let message = error.get("message").and_then(Value::as_str).unwrap_or("");
        if param == Some(PROMPT_CACHE_BREAKPOINT_KEY)
            || message.contains(PROMPT_CACHE_BREAKPOINT_KEY)
        {
            let info = UpstreamErrorInfo {
                code: error.get("code").and_then(json_scalar_string),
                error_type: error.get("type").and_then(json_scalar_string),
                param: param.map(str::to_string),
                message: Some(message.to_string()),
            };
            return PrefetchVerdict::Rejected(
                UpstreamCallError::new(
                    UpstreamErrorKind::Http,
                    Some(StatusCode::BAD_REQUEST),
                    message.to_string(),
                )
                .with_error_info(info)
                .with_source(UpstreamErrorSource::StructuredBody),
            );
        }
        return PrefetchVerdict::Proceed;
    }
    PrefetchVerdict::Undecided
}

/// ACOTU-24: some relays answer 200 and put the breakpoint rejection in the
/// first SSE event. Buffer just enough of the stream to see the first event;
/// a rejection becomes an HTTP-400-shaped error so the caller's fallback can
/// resend, and anything else is replayed byte-for-byte ahead of the rest.
async fn prefetch_stream_for_breakpoint_rejection(
    resp: reqwest::Response,
) -> Result<reqwest::Response, UpstreamCallError> {
    use futures_util::StreamExt;
    let status = resp.status();
    let version = resp.version();
    let headers = resp.headers().clone();
    let mut upstream = resp.bytes_stream();
    let mut buffer = Vec::new();
    let deadline = tokio::time::Instant::now() + STREAM_PREFETCH_MAX_WAIT;
    let mut pending_error = None;
    while buffer.len() < STREAM_PREFETCH_MAX_BYTES {
        match classify_stream_prefix(&buffer) {
            PrefetchVerdict::Rejected(err) => return Err(err),
            PrefetchVerdict::Proceed => break,
            PrefetchVerdict::Undecided => {}
        }
        match tokio::time::timeout_at(deadline, upstream.next()).await {
            Ok(Some(Ok(chunk))) => buffer.extend_from_slice(&chunk),
            Ok(Some(Err(err))) => {
                pending_error = Some(err);
                break;
            }
            Ok(None) | Err(_) => break,
        }
    }
    if let PrefetchVerdict::Rejected(err) = classify_stream_prefix(&buffer) {
        return Err(err);
    }
    let head = futures_util::stream::once(async move {
        Ok::<bytes::Bytes, reqwest::Error>(bytes::Bytes::from(buffer))
    });
    let tail = futures_util::stream::iter(pending_error.map(Err)).chain(upstream);
    let mut replay = axum::http::Response::new(reqwest::Body::wrap_stream(head.chain(tail)));
    *replay.status_mut() = status;
    *replay.version_mut() = version;
    *replay.headers_mut() = headers;
    Ok(reqwest::Response::from(replay))
}

async fn send_upstream_stream_request(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
    timeout_ms: u64,
    extra_headers: &[(String, String)],
) -> Result<reqwest::Response, UpstreamCallError> {
    let req =
        prepare_upstream_request(client, provider, auth_value, path, body, extra_headers).await?;
    // A reqwest request timeout survives successful headers and would truncate an
    // active stream. The decoder owns the idle deadline once this future returns.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let resp = tokio::time::timeout_at(deadline, req.send())
        .await
        .map_err(|_| {
            UpstreamCallError::new(
                UpstreamErrorKind::Network,
                None,
                "upstream response headers timed out".to_string(),
            )
        })?
        .map_err(|err| UpstreamCallError::new(UpstreamErrorKind::Network, None, err.to_string()))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let error = tokio::time::timeout_at(deadline, non_success_upstream_error(resp, status))
        .await
        .unwrap_or_else(|_| {
            UpstreamCallError::new(
                UpstreamErrorKind::Http,
                Some(status),
                "upstream returned an empty error body".to_string(),
            )
            .with_source(UpstreamErrorSource::EmptyBody)
        });
    Err(error)
}

const PROMPT_CACHE_BREAKPOINT_KEY: &str = "prompt_cache_breakpoint";
const BREAKPOINT_UNSUPPORTED_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// ACOTU-22: (base_url, model) pairs whose upstream rejected explicit cache
/// breakpoints, mapped to the instant the record expires.
fn breakpoint_unsupported_pairs()
-> &'static std::sync::Mutex<std::collections::HashMap<(String, String), std::time::Instant>> {
    static PAIRS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<(String, String), std::time::Instant>>,
    > = std::sync::OnceLock::new();
    PAIRS.get_or_init(Default::default)
}

fn breakpoint_pair(provider: &ProviderConfig, body: &Value) -> (String, String) {
    (
        provider.base_url.clone().unwrap_or_default(),
        body.get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

fn breakpoint_pair_is_unsupported(pair: &(String, String)) -> bool {
    let mut pairs = breakpoint_unsupported_pairs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match pairs.get(pair) {
        Some(expires_at) if *expires_at > std::time::Instant::now() => true,
        Some(_) => {
            pairs.remove(pair);
            false
        }
        None => false,
    }
}

fn record_breakpoint_unsupported(pair: (String, String)) {
    let now = std::time::Instant::now();
    let mut pairs = breakpoint_unsupported_pairs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pairs.retain(|_, expires_at| *expires_at > now);
    pairs.insert(pair, now + BREAKPOINT_UNSUPPORTED_TTL);
}

/// ACOTU-19.
fn is_explicit_breakpoint_rejection(err: &UpstreamCallError) -> bool {
    err.status == Some(StatusCode::BAD_REQUEST)
        && (err.param.as_deref() == Some(PROMPT_CACHE_BREAKPOINT_KEY)
            || (err.source == UpstreamErrorSource::StructuredBody
                && err.message.contains(PROMPT_CACHE_BREAKPOINT_KEY)))
}

/// ACOTU-20: returns the stripped body, or `None` when no key was present.
fn strip_prompt_cache_breakpoints(body: &Value) -> Option<Value> {
    fn strip_items(items: Option<&mut Value>, nested: &[&str]) -> usize {
        let Some(Value::Array(items)) = items else {
            return 0;
        };
        let mut removed = 0;
        for item in items {
            let Value::Object(object) = item else {
                continue;
            };
            removed += usize::from(object.remove(PROMPT_CACHE_BREAKPOINT_KEY).is_some());
            for key in nested {
                removed += strip_items(object.get_mut(*key), &[]);
            }
        }
        removed
    }
    let mut stripped = body.clone();
    let Value::Object(root) = &mut stripped else {
        return None;
    };
    let removed = strip_items(root.get_mut("input"), &["content", "output"])
        + strip_items(root.get_mut("messages"), &["content"]);
    (removed > 0).then_some(stripped)
}

/// ACOTU-21..23: sends `body`, and when the upstream rejects explicit cache
/// breakpoints, resends it once without them. The resend happens before any
/// response byte is returned, so it is invisible to routing and to the client.
async fn send_json_with_breakpoint_fallback<F, Fut>(
    provider: &ProviderConfig,
    body: &Value,
    send: F,
) -> Result<reqwest::Response, UpstreamCallError>
where
    F: Fn(Value) -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, UpstreamCallError>>,
{
    let pair = breakpoint_pair(provider, body);
    if breakpoint_pair_is_unsupported(&pair) {
        let body = strip_prompt_cache_breakpoints(body).unwrap_or_else(|| body.clone());
        return send(body).await;
    }
    match send(body.clone()).await {
        Err(err) if is_explicit_breakpoint_rejection(&err) => {
            let Some(stripped) = strip_prompt_cache_breakpoints(body) else {
                return Err(err);
            };
            tracing::warn!(
                base_url = %pair.0,
                model = %pair.1,
                "upstream rejected prompt_cache_breakpoint; resending without it"
            );
            record_breakpoint_unsupported(pair);
            send(stripped).await
        }
        result => result,
    }
}

async fn prepare_upstream_request(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    body: &Value,
    extra_headers: &[(String, String)],
) -> Result<reqwest::RequestBuilder, UpstreamCallError> {
    let base = provider.base_url.as_ref().ok_or_else(|| {
        UpstreamCallError::new(
            UpstreamErrorKind::Http,
            None,
            "missing base_url".to_string(),
        )
    })?;
    let url = join_url(base, path);
    guard_upstream_address(&url)
        .await
        .map_err(|message| UpstreamCallError::new(UpstreamErrorKind::Http, None, message))?;
    let mut req = client.post(url).json(body);
    let auth = provider.auth.as_ref().ok_or_else(|| {
        UpstreamCallError::new(UpstreamErrorKind::Http, None, "missing auth".to_string())
    })?;
    req = apply_auth(req, auth, auth_value)
        .map_err(|err| UpstreamCallError::new(UpstreamErrorKind::Http, None, err.message))?;
    for (k, v) in extra_headers {
        req = req.header(k, v);
    }
    Ok(req)
}

async fn send_upstream_request(
    req: reqwest::RequestBuilder,
) -> Result<reqwest::Response, UpstreamCallError> {
    let resp = req
        .send()
        .await
        .map_err(|err| UpstreamCallError::new(UpstreamErrorKind::Network, None, err.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(non_success_upstream_error(resp, status).await);
    }
    Ok(resp)
}

/// SAN-D3: classify a non-2xx upstream response. `message` keeps the raw body
/// only when no structured `error.message` exists, so that the routing layer
/// can log it server-side without ever exposing it downstream.
async fn non_success_upstream_error(
    resp: reqwest::Response,
    status: StatusCode,
) -> UpstreamCallError {
    let text = resp.text().await.unwrap_or_default();
    let info = extract_error_info(&text);
    let (message, source) = match info.message.clone() {
        Some(message) => (message, UpstreamErrorSource::StructuredBody),
        None if text.is_empty() => (
            "upstream returned an empty error body".to_string(),
            UpstreamErrorSource::EmptyBody,
        ),
        None => (text, UpstreamErrorSource::UnparsedBody),
    };
    UpstreamCallError::new(UpstreamErrorKind::Http, Some(status), message)
        .with_error_info(info)
        .with_source(source)
}

pub async fn call_upstream_multipart_with_timeout_and_headers(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    path: &str,
    form: reqwest::multipart::Form,
    timeout_ms: u64,
    extra_headers: &[(String, String)],
) -> Result<reqwest::Response, UpstreamCallError> {
    let base = provider.base_url.as_ref().ok_or_else(|| {
        UpstreamCallError::new(
            UpstreamErrorKind::Http,
            None,
            "missing base_url".to_string(),
        )
    })?;
    let url = join_url(base, path);
    guard_upstream_address(&url)
        .await
        .map_err(|message| UpstreamCallError::new(UpstreamErrorKind::Http, None, message))?;
    let mut req = client
        .post(url)
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .multipart(form);
    let auth = provider.auth.as_ref().ok_or_else(|| {
        UpstreamCallError::new(UpstreamErrorKind::Http, None, "missing auth".to_string())
    })?;
    req = apply_auth(req, auth, auth_value)
        .map_err(|err| UpstreamCallError::new(UpstreamErrorKind::Http, None, err.message))?;
    for (k, v) in extra_headers {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .map_err(|err| UpstreamCallError::new(UpstreamErrorKind::Network, None, err.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(non_success_upstream_error(resp, status).await);
    }
    Ok(resp)
}

pub async fn call_responses(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    body: &Value,
) -> Result<Value, UpstreamCallError> {
    call_upstream(client, provider, auth_value, "/v1/responses", body).await
}

pub async fn call_chat_completions(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    body: &Value,
) -> Result<Value, UpstreamCallError> {
    call_upstream(client, provider, auth_value, "/v1/chat/completions", body).await
}

pub async fn call_messages(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    auth_value: &str,
    body: &Value,
) -> Result<Value, UpstreamCallError> {
    call_upstream(client, provider, auth_value, "/v1/messages", body).await
}

#[allow(clippy::result_large_err)]
fn apply_auth(
    req: reqwest::RequestBuilder,
    auth: &ProviderAuthConfig,
    auth_value: &str,
) -> Result<reqwest::RequestBuilder, AppError> {
    match auth.auth_type {
        ProviderAuthType::Bearer => Ok(req.bearer_auth(auth_value)),
        ProviderAuthType::Header => {
            let header_name = auth
                .header_name
                .clone()
                .unwrap_or_else(|| "x-api-key".to_string());
            let req = req.header(header_name.as_str(), auth_value);
            // Official Anthropic authenticates `x-api-key`. Many Messages-compatible
            // relays authenticate Bearer. Send both when the header is `x-api-key`.
            if header_name.eq_ignore_ascii_case("x-api-key") {
                Ok(req.bearer_auth(auth_value))
            } else {
                Ok(req)
            }
        }
        ProviderAuthType::Query => {
            let query_name = auth
                .query_name
                .clone()
                .unwrap_or_else(|| "api_key".to_string());
            Ok(req.query(&[(query_name, auth_value)]))
        }
    }
}

fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let mut path = path.trim_start_matches('/');
    if base.ends_with("/v1") {
        if path == "v1" {
            path = "";
        } else if let Some(stripped) = path.strip_prefix("v1/") {
            path = stripped;
        }
    }
    if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{path}")
    }
}

fn extract_error_info(text: &str) -> UpstreamErrorInfo {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return UpstreamErrorInfo::default();
    };
    let Some(error) = value.get("error") else {
        return UpstreamErrorInfo::default();
    };
    let metadata = error.get("metadata").and_then(Value::as_object);
    UpstreamErrorInfo {
        code: error.get("code").and_then(json_scalar_string).or_else(|| {
            metadata
                .and_then(|metadata| metadata.get("provider_code"))
                .and_then(json_scalar_string)
        }),
        error_type: error.get("type").and_then(json_scalar_string).or_else(|| {
            metadata
                .and_then(|metadata| metadata.get("error_type"))
                .and_then(json_scalar_string)
        }),
        param: error
            .get("param")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        message: error
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

fn json_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn timeout_test_upstream(
        status: StatusCode,
        stall_headers: bool,
    ) -> (ProviderConfig, tokio::task::JoinHandle<()>) {
        use axum::{Router, body::Body, routing::post};
        use futures_util::stream;
        crate::monoize_routing::test_set_allow_private_upstream(true);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/v1/responses",
            post(move || async move {
                if stall_headers {
                    std::future::pending::<()>().await;
                }
                let body = Body::from_stream(stream::once(async {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    Ok::<_, std::convert::Infallible>("data: [DONE]\n\n")
                }));
                (status, body)
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = serde_json::from_value(serde_json::json!({
            "id": "timeout-test",
            "type": "responses",
            "base_url": format!("http://{address}"),
            "auth": { "type": "bearer", "value": "test-key" }
        }))
        .unwrap();
        (provider, server)
    }

    fn timeout_test_client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn streaming_body_survives_the_response_header_deadline() {
        let (provider, server) = timeout_test_upstream(StatusCode::OK, false).await;
        let response = call_upstream_stream_with_timeout_and_headers(
            &timeout_test_client(),
            &provider,
            "test-key",
            "/v1/responses",
            &serde_json::json!({"stream": true}),
            200,
            &[],
        )
        .await
        .expect("successful response headers");
        let body = tokio::time::timeout(std::time::Duration::from_secs(3), response.text())
            .await
            .expect("body completion")
            .expect("active stream must outlive header deadline");
        assert_eq!(body, "data: [DONE]\n\n");
        server.abort();
    }

    #[tokio::test]
    async fn streaming_header_wait_remains_bounded() {
        let (provider, server) = timeout_test_upstream(StatusCode::OK, true).await;
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            call_upstream_stream_with_timeout_and_headers(
                &timeout_test_client(),
                &provider,
                "test-key",
                "/v1/responses",
                &serde_json::json!({"stream": true}),
                200,
                &[],
            ),
        )
        .await
        .expect("dispatch deadline")
        .unwrap_err();
        assert!(matches!(error.kind, UpstreamErrorKind::Network));
        assert!(error.message.contains("timed out"));
        server.abort();
    }

    #[tokio::test]
    async fn streaming_error_body_deadline_preserves_http_status() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let (provider, server) = timeout_test_upstream(status, false).await;
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                call_upstream_stream_with_timeout_and_headers(
                    &timeout_test_client(),
                    &provider,
                    "test-key",
                    "/v1/responses",
                    &serde_json::json!({"stream": true}),
                    200,
                    &[],
                ),
            )
            .await
            .expect("error body deadline")
            .unwrap_err();
            assert_eq!(
                error.status,
                Some(status),
                "body timeout must retain received status"
            );
            assert!(matches!(error.kind, UpstreamErrorKind::Http));
            assert_eq!(error.source, UpstreamErrorSource::EmptyBody);
            server.abort();
        }
    }

    #[tokio::test]
    async fn nonstreaming_body_keeps_its_total_deadline() {
        let (provider, server) = timeout_test_upstream(StatusCode::OK, false).await;
        let response = call_upstream_raw_with_timeout_and_headers(
            &timeout_test_client(),
            &provider,
            "test-key",
            "/v1/responses",
            &serde_json::json!({"stream": false}),
            200,
            &[],
        )
        .await
        .expect("successful response headers");
        assert!(response.text().await.unwrap_err().is_timeout());
        server.abort();
    }

    #[test]
    fn openrouter_error_info_accepts_numeric_code_and_metadata_fallbacks() {
        let info = extract_error_info(
            r#"{"error":{"code":502,"message":"provider failed","metadata":{"provider_code":"P502","error_type":"provider_error"}}}"#,
        );
        assert_eq!(info.code.as_deref(), Some("502"));
        assert_eq!(info.error_type.as_deref(), Some("provider_error"));

        let fallback = extract_error_info(
            r#"{"error":{"message":"provider failed","metadata":{"provider_code":529,"error_type":"upstream_error"}}}"#,
        );
        assert_eq!(fallback.code.as_deref(), Some("529"));
        assert_eq!(fallback.error_type.as_deref(), Some("upstream_error"));
    }

    #[tokio::test]
    async fn guard_rejects_a_literal_loopback_dispatch_target() {
        let denied = guard_upstream_address("http://127.0.0.1:9999/v1/responses").await;
        assert!(denied.is_err(), "loopback literal must be rejected");

        let reserved = guard_upstream_address("http://10.1.2.3/v1/responses").await;
        assert!(reserved.is_err(), "RFC 1918 literal must be rejected");
    }

    #[tokio::test]
    async fn guard_allows_a_public_literal_dispatch_target() {
        assert!(
            guard_upstream_address("https://203.0.113.1/v1/responses")
                .await
                .is_err(),
            "documentation range is not publicly routable"
        );
        assert!(
            guard_upstream_address("https://1.1.1.1/v1/responses")
                .await
                .is_ok()
        );
    }

    #[test]
    fn unresolvable_host_passes_the_guard() {
        assert!(
            resolve_and_classify("host.invalid", 443).is_ok(),
            "DNS failure must not be reported as a policy rejection"
        );
    }

    /// Upstream that rejects any body containing `prompt_cache_breakpoint`
    /// the way the sub.joinreso.com relay does, and records every body it sees.
    async fn breakpoint_rejecting_upstream() -> (
        ProviderConfig,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{Json, Router, routing::post};
        crate::monoize_routing::test_set_allow_private_upstream(true);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let recorder = seen.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/v1/responses",
            post(move |Json(body): Json<Value>| {
                let recorder = recorder.clone();
                async move {
                    // `always-reject` models the case where stripping cannot help.
                    let rejected = body["model"] == "always-reject"
                        || body
                            .to_string()
                            .contains(&format!("\"{PROMPT_CACHE_BREAKPOINT_KEY}\":"));
                    recorder.lock().unwrap().push(body);
                    if rejected {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": {
                                "message": "prompt_cache_breakpoint is not supported on this model",
                                "type": "invalid_request_error",
                                "param": "prompt_cache_breakpoint",
                                "code": "invalid_parameter"
                            }})),
                        )
                    } else {
                        (StatusCode::OK, Json(serde_json::json!({"ok": true})))
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = serde_json::from_value(serde_json::json!({
            "id": "breakpoint-test",
            "type": "responses",
            "base_url": format!("http://{address}"),
            "auth": { "type": "bearer", "value": "test-key" }
        }))
        .unwrap();
        (provider, seen, server)
    }

    fn body_with_breakpoint(model: &str) -> Value {
        serde_json::json!({
            "model": model,
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": [
                    {"type": "input_text", "text": "keep prompt_cache_breakpoint text",
                     "prompt_cache_breakpoint": {"mode": "explicit"}}
                ]}
            ]
        })
    }

    #[test]
    fn stripping_removes_only_breakpoint_keys() {
        let stripped = strip_prompt_cache_breakpoints(&body_with_breakpoint("gpt-6")).unwrap();
        let block = &stripped["input"][1]["output"][0];
        assert!(block.get(PROMPT_CACHE_BREAKPOINT_KEY).is_none());
        assert_eq!(block["text"], "keep prompt_cache_breakpoint text");
        assert_eq!(block["type"], "input_text");
        assert_eq!(
            stripped["input"][0],
            body_with_breakpoint("gpt-6")["input"][0]
        );

        let chat = serde_json::json!({"messages": [
            {"role": "tool", "prompt_cache_breakpoint": {}, "content": [
                {"type": "text", "text": "x", "prompt_cache_breakpoint": {}}
            ]}
        ]});
        let stripped = strip_prompt_cache_breakpoints(&chat).unwrap();
        assert_eq!(
            stripped,
            serde_json::json!({"messages": [
                {"role": "tool", "content": [{"type": "text", "text": "x"}]}
            ]})
        );

        assert!(strip_prompt_cache_breakpoints(&serde_json::json!({"input": []})).is_none());
    }

    #[tokio::test]
    async fn rejected_breakpoint_is_stripped_resent_and_remembered() {
        let (provider, seen, server) = breakpoint_rejecting_upstream().await;
        let model = "gpt-6-fallback-memo";
        for stream in [false, true] {
            let body = body_with_breakpoint(model);
            let response = if stream {
                call_upstream_stream_with_timeout_and_headers(
                    &timeout_test_client(),
                    &provider,
                    "test-key",
                    "/v1/responses",
                    &body,
                    5_000,
                    &[],
                )
                .await
            } else {
                call_upstream_raw_with_timeout_and_headers(
                    &timeout_test_client(),
                    &provider,
                    "test-key",
                    "/v1/responses",
                    &body,
                    5_000,
                    &[],
                )
                .await
            };
            assert_eq!(response.expect("stripped resend succeeds").status(), 200);
        }
        let seen = seen.lock().unwrap();
        // First call: rejected send + stripped resend. Second call: the pair is
        // remembered, so exactly one pre-stripped send.
        assert_eq!(seen.len(), 3);
        let has_key = |body: &Value| body.to_string().contains("\"prompt_cache_breakpoint\":");
        assert!(has_key(&seen[0]));
        assert!(!has_key(&seen[1]));
        assert!(!has_key(&seen[2]));
        assert_eq!(
            seen[1]["input"][1]["output"][0]["text"],
            "keep prompt_cache_breakpoint text"
        );
        server.abort();
    }

    #[tokio::test]
    async fn unrelated_400_is_returned_without_resend() {
        let (provider, seen, server) = breakpoint_rejecting_upstream().await;
        let body = serde_json::json!({"model": "always-reject", "input": [
            {"type": "message", "role": "user", "content": "prompt_cache_breakpoint"}
        ]});
        let error = call_upstream_raw_with_timeout_and_headers(
            &timeout_test_client(),
            &provider,
            "test-key",
            "/v1/responses",
            &body,
            5_000,
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, Some(StatusCode::BAD_REQUEST));
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "no key to strip means no resend"
        );
        server.abort();
    }

    /// Relay that answers 200 and reports the breakpoint rejection as the
    /// first SSE event, and a normal stream once the key is gone.
    async fn sse_rejecting_upstream() -> (
        ProviderConfig,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{Json, Router, routing::post};
        crate::monoize_routing::test_set_allow_private_upstream(true);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let recorder = seen.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/v1/responses",
            post(move |Json(body): Json<Value>| {
                let recorder = recorder.clone();
                async move {
                    let has_key = body
                        .to_string()
                        .contains(&format!("\"{PROMPT_CACHE_BREAKPOINT_KEY}\":"));
                    recorder.lock().unwrap().push(body);
                    if has_key {
                        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"code\":\"invalid_parameter\",\"message\":\"prompt_cache_breakpoint is not supported on this model\",\"param\":\"prompt_cache_breakpoint\"}}\n\n"
                    } else {
                        "event: response.created\ndata: {\"type\":\"response.created\"}\n\ndata: [DONE]\n\n"
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = serde_json::from_value(serde_json::json!({
            "id": "sse-breakpoint-test",
            "type": "responses",
            "base_url": format!("http://{address}"),
            "auth": { "type": "bearer", "value": "test-key" }
        }))
        .unwrap();
        (provider, seen, server)
    }

    #[tokio::test]
    async fn in_stream_breakpoint_rejection_is_stripped_and_resent() {
        let (provider, seen, server) = sse_rejecting_upstream().await;
        let response = call_upstream_stream_with_timeout_and_headers(
            &timeout_test_client(),
            &provider,
            "test-key",
            "/v1/responses",
            &body_with_breakpoint("gpt-6-sse-memo"),
            5_000,
            &[],
        )
        .await
        .expect("stripped resend succeeds");
        let body = response.text().await.unwrap();
        assert!(body.starts_with("event: response.created\n"), "{body}");
        assert!(!body.contains("not supported"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(!seen[1].to_string().contains("\"prompt_cache_breakpoint\":"));
        server.abort();
    }

    #[tokio::test]
    async fn prefetched_stream_replays_every_byte() {
        let (provider, server) = timeout_test_upstream(StatusCode::OK, false).await;
        let response = call_upstream_stream_with_timeout_and_headers(
            &timeout_test_client(),
            &provider,
            "test-key",
            "/v1/responses",
            &body_with_breakpoint("gpt-6-replay"),
            5_000,
            &[],
        )
        .await
        .expect("successful stream");
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "data: [DONE]\n\n");
        server.abort();
    }

    #[test]
    fn stream_prefix_classification() {
        assert!(matches!(
            classify_stream_prefix(
                b"event: response.created\ndata: {\"type\":\"response.created\"}"
            ),
            PrefetchVerdict::Undecided
        ));
        assert!(matches!(
            classify_stream_prefix(
                b": ping\n\nevent: response.created\ndata: {\"type\":\"response.created\"}\n\n"
            ),
            PrefetchVerdict::Proceed
        ));
        assert!(matches!(
            classify_stream_prefix(b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"prompt_cache_breakpoint is not supported on this model\"}}}\r\n\r\n"),
            PrefetchVerdict::Rejected(_)
        ));
        assert!(matches!(
            classify_stream_prefix(
                b"event: error\ndata: {\"error\":{\"message\":\"rate limited\"}}\n\n"
            ),
            PrefetchVerdict::Proceed
        ));
    }
}

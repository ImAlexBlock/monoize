use crate::app::AppState;
use crate::dashboard_handlers::session_helpers::get_current_user;
use crate::error::{AppError, AppResult};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Sse};
use chrono::Utc;
use dashmap::DashMap;
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

#[derive(Debug, Deserialize)]
pub struct RequestLogsQuery {
    #[serde(default = "default_logs_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub api_key_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub time_from: Option<String>,
    #[serde(default)]
    pub time_to: Option<String>,
}

fn default_logs_limit() -> i64 {
    50
}

pub(crate) fn validate_request_log_model_filter(query: &RequestLogsQuery) -> AppResult<()> {
    crate::users::UserStore::validate_request_log_model_filter(query.model.as_deref()).map_err(
        |message| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                "request_log_model_filter_too_many_terms",
                message,
            )
            .with_param("model")
        },
    )
}

pub(crate) fn validate_request_log_time_filters(query: &RequestLogsQuery) -> AppResult<()> {
    let parse = |name: &'static str, value: Option<&str>| {
        value
            .map(|value| {
                chrono::DateTime::parse_from_rfc3339(value).map_err(|_| {
                    AppError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_time_filter",
                        format!("{name} must be an RFC 3339 timestamp"),
                    )
                    .with_param(name)
                })
            })
            .transpose()
    };
    let time_from = parse("time_from", query.time_from.as_deref())?;
    let time_to = parse("time_to", query.time_to.as_deref())?;
    if time_from.zip(time_to).is_some_and(|(from, to)| from >= to) {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_time_filter",
            "time_from must be earlier than time_to",
        )
        .with_param("time_from"));
    }
    Ok(())
}

#[cfg(test)]
mod request_log_query_tests {
    use super::{
        RequestLogsQuery, SseConnectionGuard, validate_request_log_model_filter,
        validate_request_log_time_filters,
    };
    use dashmap::DashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn query(time_from: Option<&str>, time_to: Option<&str>) -> RequestLogsQuery {
        RequestLogsQuery {
            limit: 50,
            offset: 0,
            model: None,
            status: None,
            api_key_id: None,
            username: None,
            search: None,
            time_from: time_from.map(str::to_string),
            time_to: time_to.map(str::to_string),
        }
    }

    #[test]
    fn malformed_and_reversed_time_filters_are_bad_requests() {
        let malformed = validate_request_log_time_filters(&query(Some("bad"), None)).unwrap_err();
        assert_eq!(malformed.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(malformed.code, "invalid_time_filter");
        assert_eq!(malformed.param.as_deref(), Some("time_from"));

        let reversed = validate_request_log_time_filters(&query(
            Some("2024-01-02T00:00:00Z"),
            Some("2024-01-01T00:00:00Z"),
        ))
        .unwrap_err();
        assert_eq!(reversed.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(reversed.param.as_deref(), Some("time_from"));
    }

    #[test]
    fn over_limit_model_filter_is_a_bad_request() {
        let mut query = query(None, None);
        query.model = Some(
            (0..33)
                .map(|term| format!("model-{term}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        let error = validate_request_log_model_filter(&query).unwrap_err();
        assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "request_log_model_filter_too_many_terms");
        assert_eq!(error.param.as_deref(), Some("model"));
    }

    #[test]
    fn sse_counter_entry_is_removed_without_decrementing_a_replacement() {
        let connections = Arc::new(DashMap::new());
        let counter = Arc::new(AtomicUsize::new(1));
        connections.insert("user-1".to_string(), counter.clone());
        let replacement = Arc::new(AtomicUsize::new(1));
        connections.insert("user-1".to_string(), replacement.clone());
        drop(SseConnectionGuard {
            user_id: "user-1".to_string(),
            connections: connections.clone(),
            counter,
        });
        let current = connections.get("user-1").expect("replacement remains");
        assert!(Arc::ptr_eq(current.value(), &replacement));
        assert_eq!(current.value().load(Ordering::Relaxed), 1);
    }
}

pub async fn list_my_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<RequestLogsQuery>,
) -> AppResult<impl IntoResponse> {
    validate_request_log_model_filter(&query)?;
    let user = get_current_user(&headers, &state).await?;
    validate_request_log_time_filters(&query)?;
    let is_admin = user.role.can_manage_users();
    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);
    let (mut logs, total, total_charge_nano_usd) = if is_admin {
        state
            .user_store
            .list_all_request_logs(
                limit,
                offset,
                query.model.as_deref(),
                query.status.as_deref(),
                query.api_key_id.as_deref(),
                query.username.as_deref(),
                query.search.as_deref(),
                query.time_from.as_deref(),
                query.time_to.as_deref(),
            )
            .await
    } else {
        state
            .user_store
            .list_request_logs_by_user(
                &user.id,
                limit,
                offset,
                query.model.as_deref(),
                query.status.as_deref(),
                query.api_key_id.as_deref(),
                query.search.as_deref(),
                query.time_from.as_deref(),
                query.time_to.as_deref(),
            )
            .await
    }
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;

    // RL-API16 hides the response model independently of configurable error masking.
    if !is_admin {
        let mask_errors = state.monoize_runtime.read().await.mask_sensitive_info;
        for log in &mut logs {
            log.hide_upstream_response_model();
            if mask_errors {
                log.mask_error_detail_for_non_admin();
            }
        }
    }

    Ok(Json(json!({
        "data": logs,
        "total": total,
        "total_charge_nano_usd": total_charge_nano_usd,
        "limit": limit,
        "offset": offset,
    })))
}

/// `GET /api/dashboard/me/live-usage` (`user-live-usage.spec.md` LU-1..LU-7).
///
/// Always scoped to the authenticated user's own request-log rows; an admin
/// caller receives the same own-rows-only aggregate (LU-3). No query
/// parameters are accepted.
pub async fn get_my_live_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let usage = state
        .user_store
        .get_user_live_usage(&user.id)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;
    let tpm = usage
        .input_tokens
        .checked_add(usage.output_tokens)
        .ok_or_else(|| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "live usage token aggregate overflow",
            )
        })?;
    // LU-6: ratio (not percentage), null when the window has no input tokens.
    let cache_hit_rate = (usage.input_tokens > 0)
        .then(|| usage.cache_read_tokens as f64 / usage.input_tokens as f64);
    Ok(Json(json!({
        "window_seconds": crate::users::LIVE_USAGE_WINDOW_SECONDS,
        "rpm": usage.rpm,
        "tpm": tpm,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_tokens,
        "cache_hit_rate": cache_hit_rate,
    })))
}

#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    #[serde(default = "default_analytics_buckets")]
    pub buckets: i64,
    #[serde(default = "default_analytics_range_hours")]
    pub range_hours: i64,
    pub scope: Option<String>,
}

fn default_analytics_buckets() -> i64 {
    8
}

fn default_analytics_range_hours() -> i64 {
    24
}

/// The resolved row-visibility rule for the analytics endpoint (DH-16..DH-18).
enum AnalyticsUserScope {
    SelfUser(String),
    Group(String),
    All,
}

fn analytics_user_scope(
    can_manage_users: bool,
    user_id: &str,
    group_id: &str,
    scope: Option<&str>,
) -> Result<AnalyticsUserScope, &'static str> {
    match scope {
        Some("self") => Ok(AnalyticsUserScope::SelfUser(user_id.to_string())),
        Some("group") if can_manage_users => Ok(AnalyticsUserScope::Group(group_id.to_string())),
        Some(_) => Err("scope must equal self or group"),
        None if can_manage_users => Ok(AnalyticsUserScope::All),
        None => Ok(AnalyticsUserScope::SelfUser(user_id.to_string())),
    }
}

fn exact_integer_json(value: i128) -> Value {
    Value::String(value.to_string())
}

#[cfg(test)]
mod dashboard_analytics_tests {
    use super::{AnalyticsUserScope, analytics_user_scope, exact_integer_json};
    use serde_json::Value;

    fn scope(
        can_manage_users: bool,
        user_id: &str,
        group_id: &str,
        raw: Option<&str>,
    ) -> Result<(Option<String>, Option<String>), &'static str> {
        Ok(
            match analytics_user_scope(can_manage_users, user_id, group_id, raw)? {
                AnalyticsUserScope::SelfUser(id) => (Some(id), None),
                AnalyticsUserScope::Group(id) => (None, Some(id)),
                AnalyticsUserScope::All => (None, None),
            },
        )
    }

    #[test]
    fn dashboard_analytics_self_scope_filters_admin_to_current_user() {
        assert_eq!(
            scope(true, "admin-user", "g1", Some("self")).unwrap(),
            (Some("admin-user".to_string()), None)
        );
        assert_eq!(scope(true, "admin-user", "g1", None).unwrap(), (None, None));
        assert_eq!(
            scope(false, "member-user", "g1", None).unwrap(),
            (Some("member-user".to_string()), None)
        );
        assert_eq!(
            scope(false, "member-user", "g1", Some("all")).unwrap_err(),
            "scope must equal self or group"
        );
        // scope=group is reserved for Admin roles (DH-16).
        assert_eq!(
            scope(false, "member-user", "g1", Some("group")).unwrap_err(),
            "scope must equal self or group"
        );
        assert_eq!(
            scope(true, "admin-user", "g7", Some("group")).unwrap(),
            (None, Some("g7".to_string()))
        );
    }

    #[test]
    fn dashboard_analytics_serializes_exact_token_totals_as_strings() {
        let value = exact_integer_json(9_007_199_254_740_993_i128);
        assert_eq!(value, Value::String("9007199254740993".to_string()));
    }
}

pub async fn get_dashboard_analytics(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AnalyticsQuery>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let buckets = query.buckets.clamp(1, 48);
    let range_hours = query.range_hours.clamp(1, 720);

    let now = Utc::now();
    let time_to = now.to_rfc3339();
    let time_from = (now - chrono::Duration::hours(range_hours)).to_rfc3339();
    let today_start = crate::beijing_time::beijing_today_start_utc(now).to_rfc3339();

    let (user_id_filter, group_id_filter) = {
        let resolved = analytics_user_scope(
            user.role.can_manage_users(),
            &user.id,
            &user.group_id,
            query.scope.as_deref(),
        )
        .map_err(|message| AppError::new(StatusCode::BAD_REQUEST, "invalid_request", message))?;
        match resolved {
            AnalyticsUserScope::SelfUser(id) => (Some(id), None),
            AnalyticsUserScope::Group(id) => (None, Some(id)),
            AnalyticsUserScope::All => (None, None),
        }
    };

    let raw = state
        .user_store
        .get_dashboard_analytics(
            user_id_filter.as_deref(),
            None,
            None,
            group_id_filter.as_deref(),
            &time_from,
            &time_to,
            &today_start,
            buckets,
        )
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;

    Ok(Json(render_analytics_json(
        &raw,
        buckets,
        range_hours,
        now,
        &time_from,
        &time_to,
    )?))
}

#[derive(Debug, Deserialize)]
pub struct CacheUsersQuery {
    #[serde(default = "default_cache_users_range_hours")]
    pub range_hours: i64,
}

fn default_cache_users_range_hours() -> i64 {
    168
}

/// `GET /api/dashboard/usage/cache/users` (dashboard-usage-analysis.spec.md UA-42):
/// per-user input and cache-read token aggregates for the super_admin cache table.
pub async fn get_cache_hit_rate_by_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<CacheUsersQuery>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    if user.role != crate::users::UserRole::SuperAdmin {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cache hit rate per user requires the super_admin role",
        ));
    }
    let range_hours = query.range_hours.clamp(1, 720);
    let time_from_unix_ms = (Utc::now() - chrono::Duration::hours(range_hours)).timestamp_millis();
    let rows = state
        .user_store
        .get_cache_hit_rate_by_users(time_from_unix_ms)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;
    Ok(Json(json!({
        "range_hours": range_hours,
        "users": rows
            .iter()
            .map(|row| {
                json!({
                    "user_id": row.user_id,
                    "username": row.username,
                    "calls": row.call_count.to_string(),
                    "input_tokens": row.input_tokens.to_string(),
                    "cache_read_tokens": row.cache_read_tokens.to_string(),
                })
            })
            .collect::<Vec<_>>(),
    })))
}

/// Shared response shape for every analytics consumer (self, admin, org space).
/// Token and cost totals are exact integers serialized as strings.
pub(crate) fn render_analytics_json(
    raw: &crate::users::DashboardAnalyticsRaw,
    buckets: i64,
    range_hours: i64,
    now: chrono::DateTime<Utc>,
    time_from: &str,
    time_to: &str,
) -> Result<Value, AppError> {
    let range_ms = (range_hours as f64) * 3600.0 * 1000.0;
    let time_from_ms = now.timestamp_millis() as f64 - range_ms;
    let bucket_width_ms = range_ms / (buckets as f64);

    let mut bucket_labels: Vec<String> = Vec::with_capacity(buckets as usize);
    for i in 0..buckets {
        let ms = time_from_ms + (i as f64) * bucket_width_ms;
        let secs = (ms / 1000.0) as i64;
        let dt = chrono::DateTime::from_timestamp(secs, 0).unwrap_or(now);
        let label = dt.format("%m-%d %H:00").to_string();
        bucket_labels.push(label);
    }

    let mut cost_by_model_buckets: Vec<BTreeMap<String, i128>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut calls_by_model_buckets: Vec<BTreeMap<String, i64>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut calls_by_provider_buckets: Vec<BTreeMap<String, i64>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut input_tokens_by_model_buckets: Vec<BTreeMap<String, i128>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut cache_read_tokens_by_model_buckets: Vec<BTreeMap<String, i128>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut output_tokens_by_model_buckets: Vec<BTreeMap<String, i128>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    // UA-27a: Group-dimension twins of the by-model maps.
    let mut calls_by_model_and_group_buckets: Vec<BTreeMap<String, i64>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut input_tokens_by_model_and_group_buckets: Vec<BTreeMap<String, i128>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut cache_read_tokens_by_model_and_group_buckets: Vec<BTreeMap<String, i128>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();
    let mut output_tokens_by_model_and_group_buckets: Vec<BTreeMap<String, i128>> =
        (0..buckets).map(|_| BTreeMap::new()).collect();

    for row in &raw.model_buckets {
        let idx = row.bucket_idx.clamp(0, buckets - 1) as usize;
        let cost = cost_by_model_buckets[idx]
            .entry(row.model.clone())
            .or_insert(0);
        *cost = cost.checked_add(row.cost_nano).ok_or_else(|| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "analytics cost aggregate overflow",
            )
        })?;
        let calls = calls_by_model_buckets[idx]
            .entry(row.model.clone())
            .or_insert(0);
        *calls = calls.checked_add(row.call_count).ok_or_else(|| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "analytics call count overflow",
            )
        })?;
        // UA-27a: the same aggregates keyed by Group + U+2063 + model.
        let grouped_key = format!("{}\u{2063}{}", row.group_name, row.model);
        let grouped_calls = calls_by_model_and_group_buckets[idx]
            .entry(grouped_key.clone())
            .or_insert(0);
        *grouped_calls = grouped_calls.checked_add(row.call_count).ok_or_else(|| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "analytics call count overflow",
            )
        })?;
        for (target, value, label) in [
            (
                &mut input_tokens_by_model_buckets,
                row.input_tokens,
                "input",
            ),
            (
                &mut cache_read_tokens_by_model_buckets,
                row.cache_read_tokens,
                "cache-read",
            ),
            (
                &mut output_tokens_by_model_buckets,
                row.output_tokens,
                "output",
            ),
        ] {
            let total = target[idx].entry(row.model.clone()).or_insert(0);
            *total = total.checked_add(value).ok_or_else(|| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    format!("analytics {label} token aggregate overflow"),
                )
            })?;
        }
        for (target, value, label) in [
            (
                &mut input_tokens_by_model_and_group_buckets,
                row.input_tokens,
                "input",
            ),
            (
                &mut cache_read_tokens_by_model_and_group_buckets,
                row.cache_read_tokens,
                "cache-read",
            ),
            (
                &mut output_tokens_by_model_and_group_buckets,
                row.output_tokens,
                "output",
            ),
        ] {
            let total = target[idx].entry(grouped_key.clone()).or_insert(0);
            *total = total.checked_add(value).ok_or_else(|| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    format!("analytics {label} token aggregate overflow"),
                )
            })?;
        }
    }

    for row in &raw.provider_buckets {
        let idx = row.bucket_idx.clamp(0, buckets - 1) as usize;
        let calls = calls_by_provider_buckets[idx]
            .entry(row.provider_label.clone())
            .or_insert(0);
        *calls = calls.checked_add(row.call_count).ok_or_else(|| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "analytics call count overflow",
            )
        })?;
    }

    let response_buckets: Vec<Value> = (0..buckets as usize)
        .map(|i| {
            json!({
                "label": bucket_labels[i],
                "cost_by_model": cost_by_model_buckets[i]
                    .iter()
                    .map(|(model, cost)| (model.clone(), Value::String(cost.to_string())))
                    .collect::<serde_json::Map<String, Value>>(),
                "calls_by_model": calls_by_model_buckets[i],
                "calls_by_provider": calls_by_provider_buckets[i],
                "input_tokens_by_model": input_tokens_by_model_buckets[i]
                    .iter()
                    .map(|(model, value)| (model.clone(), exact_integer_json(*value)))
                    .collect::<serde_json::Map<String, Value>>(),
                "cache_read_tokens_by_model": cache_read_tokens_by_model_buckets[i]
                    .iter()
                    .map(|(model, value)| (model.clone(), exact_integer_json(*value)))
                    .collect::<serde_json::Map<String, Value>>(),
                "output_tokens_by_model": output_tokens_by_model_buckets[i]
                    .iter()
                    .map(|(model, value)| (model.clone(), exact_integer_json(*value)))
                    .collect::<serde_json::Map<String, Value>>(),
                "calls_by_model_and_group": calls_by_model_and_group_buckets[i],
                "input_tokens_by_model_and_group": input_tokens_by_model_and_group_buckets[i]
                    .iter()
                    .map(|(key, value)| (key.clone(), exact_integer_json(*value)))
                    .collect::<serde_json::Map<String, Value>>(),
                "cache_read_tokens_by_model_and_group":
                    cache_read_tokens_by_model_and_group_buckets[i]
                        .iter()
                        .map(|(key, value)| (key.clone(), exact_integer_json(*value)))
                        .collect::<serde_json::Map<String, Value>>(),
                "output_tokens_by_model_and_group": output_tokens_by_model_and_group_buckets[i]
                    .iter()
                    .map(|(key, value)| (key.clone(), exact_integer_json(*value)))
                    .collect::<serde_json::Map<String, Value>>(),
            })
        })
        .collect();

    Ok(json!({
        "buckets": response_buckets,
        "time_from": time_from,
        "time_to": time_to,
        "total_cost_nano_usd": raw.total_cost_nano_usd.to_string(),
        "total_calls": raw.total_calls,
        "today_cost_nano_usd": raw.today_cost_nano_usd.to_string(),
        "today_calls": raw.today_calls,
        "total_input_tokens": exact_integer_json(raw.total_input_tokens),
        "total_cache_read_tokens": exact_integer_json(raw.total_cache_read_tokens),
        "total_output_tokens": exact_integer_json(raw.total_output_tokens),
        "total_tokens": exact_integer_json(raw.total_tokens),
    }))
}

/// Guard that decrements the per-user SSE connection counter on drop,
/// ensuring no counter leaks even if the stream is abruptly cancelled.
struct SseConnectionGuard {
    user_id: String,
    connections: Arc<DashMap<String, Arc<AtomicUsize>>>,
    counter: Arc<AtomicUsize>,
}

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        if self.counter.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.connections.remove_if(&self.user_id, |_, current| {
                Arc::ptr_eq(current, &self.counter) && current.load(Ordering::Acquire) == 0
            });
        }
    }
}

fn max_sse_connections_per_user() -> usize {
    std::env::var("MONOIZE_REQUEST_LOG_SSE_MAX_CONNECTIONS_PER_USER")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5)
}

// RRB-R1: process-wide ceiling across every dashboard log-stream subscriber.
fn max_sse_connections_total() -> usize {
    std::env::var("MONOIZE_SSE_MAX_CONNECTIONS_TOTAL")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(64)
}

pub async fn stream_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let is_admin = user.role.can_manage_users();
    let user_id = user.id;

    // Enforce per-user SSE connection limit
    let entry = state
        .sse_connections
        .entry(user_id.clone())
        .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
    let counter = entry.value().clone();
    let current = counter.fetch_add(1, Ordering::AcqRel);
    drop(entry);
    if current >= max_sse_connections_per_user() {
        if counter.fetch_sub(1, Ordering::AcqRel) == 1 {
            state.sse_connections.remove_if(&user_id, |_, current| {
                Arc::ptr_eq(current, &counter) && current.load(Ordering::Acquire) == 0
            });
        }
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_sse_connections",
            "Too many concurrent SSE connections",
        ));
    }
    let global_total: usize = state
        .sse_connections
        .iter()
        .map(|entry| entry.value().load(Ordering::Acquire))
        .sum();
    if global_total > max_sse_connections_total() {
        if counter.fetch_sub(1, Ordering::AcqRel) == 1 {
            state.sse_connections.remove_if(&user_id, |_, current| {
                Arc::ptr_eq(current, &counter) && current.load(Ordering::Acquire) == 0
            });
        }
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_sse_connections",
            "Server SSE connection budget exhausted",
        ));
    }

    let guard = SseConnectionGuard {
        user_id: user_id.clone(),
        connections: state.sse_connections.clone(),
        counter,
    };

    let receiver = state.log_broadcast.subscribe();
    let runtime = state.monoize_runtime.clone();

    let mut initial_pending: Vec<_> = state
        .pending_request_logs
        .iter()
        // RL-API15: probe rows are synthetic monitoring traffic and MUST NOT
        // reach the dashboard SSE stream at any role.
        .filter(|entry| {
            entry.value().request_kind.as_deref()
                != Some(crate::app::ACTIVE_PROBE_CONNECTIVITY_KIND)
        })
        .filter(|entry| is_admin || entry.value().user_id == user_id)
        .map(|entry| entry.value().clone())
        .collect();
    initial_pending.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let initial_event = if initial_pending.is_empty() {
        Event::default().comment("ready")
    } else {
        // RL-API14 / SAN-14: non-admin SSE rows carry masked detail unless
        // masking is disabled (SAN-CFG5 item 5).
        let mask_batch = !is_admin && runtime.read().await.mask_sensitive_info;
        let enriched_batch: Vec<_> = initial_pending
            .into_iter()
            .map(|log| {
                let mut row = log.to_request_log_row();
                if !is_admin {
                    row.hide_upstream_response_model();
                }
                if mask_batch {
                    row.mask_error_detail_for_non_admin();
                }
                row
            })
            .collect();
        match serde_json::to_string(&enriched_batch) {
            Ok(payload) => Event::default().event("log_batch").data(payload),
            Err(_) => Event::default().event("resync").data("{}"),
        }
    };

    let live_stream = stream::unfold(
        (receiver, is_admin, user_id, guard, runtime),
        |(mut receiver, is_admin, user_id, guard, runtime)| async move {
            loop {
                match receiver.recv().await {
                    Ok(batch) => {
                        // RL-API15 applies to live SSE batches as well.
                        let filtered: Vec<_> = if is_admin {
                            batch
                                .into_iter()
                                .filter(|log| {
                                    log.request_kind.as_deref()
                                        != Some(crate::app::ACTIVE_PROBE_CONNECTIVITY_KIND)
                                })
                                .collect()
                        } else {
                            batch
                                .into_iter()
                                .filter(|log| {
                                    log.user_id == user_id
                                        && log.request_kind.as_deref()
                                            != Some(crate::app::ACTIVE_PROBE_CONNECTIVITY_KIND)
                                })
                                .collect()
                        };
                        if filtered.is_empty() {
                            continue;
                        }
                        // RL-API14 / SAN-14 with the SAN-CFG5 item 5 gate,
                        // re-read per batch so a settings change applies to
                        // subsequent frames of an open connection.
                        let mask_batch = !is_admin && runtime.read().await.mask_sensitive_info;
                        let enriched_batch: Vec<_> = filtered
                            .into_iter()
                            .map(|log| {
                                let mut row = log.to_request_log_row();
                                if !is_admin {
                                    row.hide_upstream_response_model();
                                }
                                if mask_batch {
                                    row.mask_error_detail_for_non_admin();
                                }
                                row
                            })
                            .collect();
                        let event = match serde_json::to_string(&enriched_batch) {
                            Ok(payload) => Event::default().event("log_batch").data(payload),
                            Err(_) => Event::default().event("resync").data("{}"),
                        };
                        return Some((
                            Ok::<Event, Infallible>(event),
                            (receiver, is_admin, user_id, guard, runtime),
                        ));
                    }
                    Err(RecvError::Lagged(_)) => {
                        let event = Event::default().event("resync").data("{}");
                        return Some((
                            Ok::<Event, Infallible>(event),
                            (receiver, is_admin, user_id, guard, runtime),
                        ));
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        },
    );
    let stream =
        stream::once(async move { Ok::<Event, Infallible>(initial_event) }).chain(live_stream);

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

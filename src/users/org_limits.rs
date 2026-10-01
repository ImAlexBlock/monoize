//! Organization usage limits (org-usage-limits.spec.md). Limit windows are computed
//! from the durable `request_logs` attribution, so history never moves when members
//! or keys are removed. All queries run on the read pool (ORGL-17).

use super::UserStore;
use sea_orm::ConnectionTrait;
use sea_orm::sea_query::Value as SeaValue;

/// One window result for one level. `spent` is the summed nano-USD over the window.
#[derive(Debug, Clone, Default)]
pub struct OrgSpendWindow {
    pub limit_nano_usd: Option<i128>,
    pub spent_nano_usd: i128,
}

#[derive(Debug, Clone)]
pub struct OrgLimitLevels {
    pub space: OrgSpendWindows,
    pub member: Option<OrgSpendWindows>,
    pub key: Option<OrgSpendWindows>,
}

#[derive(Debug, Clone, Default)]
pub struct OrgSpendWindows {
    pub total: OrgSpendWindow,
    pub hourly: OrgSpendWindow,
    pub daily: OrgSpendWindow,
}

#[derive(Debug, Clone)]
pub struct OrgLimitBreach {
    pub level: &'static str,
    pub window: &'static str,
}

fn parse_limit(raw: Option<String>) -> Result<Option<i128>, String> {
    // Rows written by older builds may carry '' (cleared before NULL-clearing existed);
    // treat the empty string exactly like NULL: unlimited.
    raw.filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<i128>()
                .map_err(|e| format!("invalid persisted org spend limit {value:?}: {e}"))
        })
        .transpose()
}

/// Charges are persisted as decimal strings. The request-log sums use the canonical
/// non-negative digit string (request-log charges from org keys are never negative);
/// the limb-split trick below keeps SQLite exact past 2^53 without importing the
/// heavier 5-limb ranking aggregate — 39 digits cover i128 twice over.
fn sum_charge_expr(is_postgres: bool, alias: &str) -> String {
    if is_postgres {
        return format!(
            "COALESCE(SUM(CASE WHEN {alias}.status = 'success' THEN CAST({alias}.charge_nano_usd AS NUMERIC) ELSE 0 END), 0)::TEXT"
        );
    }
    // SQLite: three 9-digit limbs is exact for any value below 10^27 nano-USD.
    let digits = format!(
        "(CASE WHEN SUBSTR({alias}.charge_nano_usd, 1, 1) = '-' THEN SUBSTR({alias}.charge_nano_usd, 2) ELSE {alias}.charge_nano_usd END)"
    );
    let padded = format!("('000000000000000000000000000000000000000000000' || {digits})");
    let sign =
        format!("(CASE WHEN SUBSTR({alias}.charge_nano_usd, 1, 1) = '-' THEN -1 ELSE 1 END)");
    let mut parts = Vec::new();
    for limb in 0..3 {
        let start = -9 * (limb + 1);
        parts.push(format!(
            "COALESCE(SUM(CASE WHEN {alias}.status = 'success' THEN {sign} * CAST(SUBSTR({padded}, {start}, 9) AS INTEGER) ELSE 0 END), 0)"
        ));
    }
    // Reassemble as a decimal string via printf-free arithmetic: limb2*10^18 + limb1*10^9 + limb0.
    // SQLite integers are 64-bit, so compose with CAST to keep magnitude exact.
    format!(
        "CAST({p0} + {p1} * 1000000000 + {p2} * 1000000000000000000 AS TEXT)",
        p0 = parts[0],
        p1 = parts[1],
        p2 = parts[2]
    )
}

fn limb_sum_to_i128(text: &str) -> Result<i128, String> {
    // The composite expression above can still overflow to float on absurd values;
    // parse leniently (scientific notation included) and saturate.
    let t = text.trim();
    if let Ok(v) = t.parse::<i128>() {
        return Ok(v);
    }
    if let Ok(f) = t.parse::<f64>() {
        return Ok(if f >= i128::MAX as f64 {
            i128::MAX
        } else if f <= i128::MIN as f64 {
            i128::MIN
        } else {
            f as i128
        });
    }
    Err(format!("unparseable org spend aggregate {text:?}"))
}

/// ORGL-1..4: load the configured limits and live consumption for the space, the
/// requesting member, and the key. `member_id` is the key's `created_by`; keys with
/// no creator attribute to the owner (ORGL-13's rule).
pub async fn load_limit_levels(
    store: &UserStore,
    org_id: &str,
    member_id: &str,
    api_key_id: &str,
) -> Result<OrgLimitLevels, String> {
    let is_postgres = store.db.is_postgres();
    let now_unix_ms = chrono::Utc::now().timestamp_millis();
    let hour_ago_unix_ms = now_unix_ms - 3_600_000;
    let day_start = chrono::Utc::now()
        .date_naive()
        .and_time(chrono::NaiveTime::MIN)
        .and_utc();
    let day_start_unix_ms = day_start.timestamp_millis();

    // Space limits + windows in one pass over idx_request_logs_org.
    let space_row = store
        .db
        .read()
        .query_one(store.db.stmt(
            &("SELECT o.spend_limit_total_nano_usd, o.spend_limit_hourly_nano_usd, o.spend_limit_daily_nano_usd,
                    (SELECT {sum} FROM request_logs rl WHERE rl.user_id = $1) AS spent_total,
                    (SELECT {sum} FROM request_logs rl WHERE rl.user_id = $1 AND rl.created_at_unix_ms >= $2) AS spent_hourly,
                    (SELECT {sum} FROM request_logs rl WHERE rl.user_id = $1 AND rl.created_at_unix_ms >= $3) AS spent_daily
             FROM orgs o WHERE o.id = $1"
                .replace("{sum}", &sum_charge_expr(is_postgres, "rl"))),
            vec![
                org_id.into(),
                hour_ago_unix_ms.into(),
                day_start_unix_ms.into(),
            ],
        ))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "org not found".to_string())?;
    let space = OrgSpendWindows {
        total: OrgSpendWindow {
            limit_nano_usd: parse_limit(
                space_row
                    .try_get("", "spend_limit_total_nano_usd")
                    .map_err(|e| e.to_string())?,
            )?,
            spent_nano_usd: limb_sum_to_i128(
                &space_row
                    .try_get::<String>("", "spent_total")
                    .map_err(|e| e.to_string())?,
            )?,
        },
        hourly: OrgSpendWindow {
            limit_nano_usd: parse_limit(
                space_row
                    .try_get("", "spend_limit_hourly_nano_usd")
                    .map_err(|e| e.to_string())?,
            )?,
            spent_nano_usd: limb_sum_to_i128(
                &space_row
                    .try_get::<String>("", "spent_hourly")
                    .map_err(|e| e.to_string())?,
            )?,
        },
        daily: OrgSpendWindow {
            limit_nano_usd: parse_limit(
                space_row
                    .try_get("", "spend_limit_daily_nano_usd")
                    .map_err(|e| e.to_string())?,
            )?,
            spent_nano_usd: limb_sum_to_i128(
                &space_row
                    .try_get::<String>("", "spent_daily")
                    .map_err(|e| e.to_string())?,
            )?,
        },
    };

    // Member limits: window rows attributed to keys of this org created by member_id.
    let member_row = store
        .db
        .read()
        .query_one(store.db.stmt(
            &("SELECT m.spend_limit_total_nano_usd, m.spend_limit_hourly_nano_usd, m.spend_limit_daily_nano_usd,
                    (SELECT {sum} FROM request_logs rl JOIN api_keys k ON k.id = rl.api_key_id
                     WHERE rl.user_id = $1 AND k.org_id = $1 AND k.created_by = $2) AS spent_total,
                    (SELECT {sum} FROM request_logs rl JOIN api_keys k ON k.id = rl.api_key_id
                     WHERE rl.user_id = $1 AND k.org_id = $1 AND k.created_by = $2 AND rl.created_at_unix_ms >= $3) AS spent_hourly,
                    (SELECT {sum} FROM request_logs rl JOIN api_keys k ON k.id = rl.api_key_id
                     WHERE rl.user_id = $1 AND k.org_id = $1 AND k.created_by = $2 AND rl.created_at_unix_ms >= $4) AS spent_daily
             FROM org_members m WHERE m.org_id = $1 AND m.user_id = $2")
                .replace("{sum}", &sum_charge_expr(is_postgres, "rl")),
            vec![
                org_id.into(),
                member_id.into(),
                hour_ago_unix_ms.into(),
                day_start_unix_ms.into(),
            ],
        ))
        .await
        .map_err(|e| e.to_string())?;
    let member = member_row
        .map(|row| {
            let limits = [
                row.try_get::<Option<String>>("", "spend_limit_total_nano_usd")
                    .map_err(|e| e.to_string())?,
                row.try_get::<Option<String>>("", "spend_limit_hourly_nano_usd")
                    .map_err(|e| e.to_string())?,
                row.try_get::<Option<String>>("", "spend_limit_daily_nano_usd")
                    .map_err(|e| e.to_string())?,
            ];
            let spent = [
                limb_sum_to_i128(
                    &row.try_get::<String>("", "spent_total")
                        .map_err(|e| e.to_string())?,
                )?,
                limb_sum_to_i128(
                    &row.try_get::<String>("", "spent_hourly")
                        .map_err(|e| e.to_string())?,
                )?,
                limb_sum_to_i128(
                    &row.try_get::<String>("", "spent_daily")
                        .map_err(|e| e.to_string())?,
                )?,
            ];
            Ok::<OrgSpendWindows, String>(OrgSpendWindows {
                total: OrgSpendWindow {
                    limit_nano_usd: parse_limit(limits[0].clone())?,
                    spent_nano_usd: spent[0],
                },
                hourly: OrgSpendWindow {
                    limit_nano_usd: parse_limit(limits[1].clone())?,
                    spent_nano_usd: spent[1],
                },
                daily: OrgSpendWindow {
                    limit_nano_usd: parse_limit(limits[2].clone())?,
                    spent_nano_usd: spent[2],
                },
            })
        })
        .transpose()?;

    // Key limits: idx_request_logs_api_key_created. Key-level windows apply to
    // every key (ORGL-3); org keys additionally sit under the levels above.
    let key = load_key_windows(store, api_key_id).await?;

    Ok(OrgLimitLevels { space, member, key })
}

/// ORGL-3: the key-level limit windows for one key, personal or org. Returns
/// None when the key row does not exist. The window bounds reuse ORGL-6:
/// hourly = rolling 3600 seconds, daily = current UTC calendar day.
pub async fn load_key_windows(
    store: &UserStore,
    api_key_id: &str,
) -> Result<Option<OrgSpendWindows>, String> {
    let is_postgres = store.db.is_postgres();
    let now_unix_ms = chrono::Utc::now().timestamp_millis();
    let hour_ago_unix_ms = now_unix_ms - 3_600_000;
    let day_start_unix_ms = chrono::Utc::now()
        .date_naive()
        .and_time(chrono::NaiveTime::MIN)
        .and_utc()
        .timestamp_millis();
    let row = store
        .db
        .read()
        .query_one(store.db.stmt(
            &("SELECT k.spend_limit_total_nano_usd, k.spend_limit_hourly_nano_usd, k.spend_limit_daily_nano_usd,
                    (SELECT {sum} FROM request_logs rl WHERE rl.api_key_id = $1) AS spent_total,
                    (SELECT {sum} FROM request_logs rl WHERE rl.api_key_id = $1 AND rl.created_at_unix_ms >= $2) AS spent_hourly,
                    (SELECT {sum} FROM request_logs rl WHERE rl.api_key_id = $1 AND rl.created_at_unix_ms >= $3) AS spent_daily
             FROM api_keys k WHERE k.id = $1")
                .replace("{sum}", &sum_charge_expr(is_postgres, "rl")),
            vec![
                api_key_id.into(),
                hour_ago_unix_ms.into(),
                day_start_unix_ms.into(),
            ],
        ))
        .await
        .map_err(|e| e.to_string())?;
    Ok(row
        .map(|row| {
            let limits = [
                row.try_get::<Option<String>>("", "spend_limit_total_nano_usd")
                    .map_err(|e| e.to_string())?,
                row.try_get::<Option<String>>("", "spend_limit_hourly_nano_usd")
                    .map_err(|e| e.to_string())?,
                row.try_get::<Option<String>>("", "spend_limit_daily_nano_usd")
                    .map_err(|e| e.to_string())?,
            ];
            let spent = [
                limb_sum_to_i128(
                    &row.try_get::<String>("", "spent_total")
                        .map_err(|e| e.to_string())?,
                )?,
                limb_sum_to_i128(
                    &row.try_get::<String>("", "spent_hourly")
                        .map_err(|e| e.to_string())?,
                )?,
                limb_sum_to_i128(
                    &row.try_get::<String>("", "spent_daily")
                        .map_err(|e| e.to_string())?,
                )?,
            ];
            Ok::<OrgSpendWindows, String>(OrgSpendWindows {
                total: OrgSpendWindow {
                    limit_nano_usd: parse_limit(limits[0].clone())?,
                    spent_nano_usd: spent[0],
                },
                hourly: OrgSpendWindow {
                    limit_nano_usd: parse_limit(limits[1].clone())?,
                    spent_nano_usd: spent[1],
                },
                daily: OrgSpendWindow {
                    limit_nano_usd: parse_limit(limits[2].clone())?,
                    spent_nano_usd: spent[2],
                },
            })
        })
        .transpose()?)
}

/// DPT-SW5: cached variant for forwarding admission preflight only. Settlement,
/// balance mutation, and admin views call `load_key_windows` directly.
pub async fn load_key_windows_cached(
    store: &UserStore,
    api_key_id: &str,
) -> Result<Option<OrgSpendWindows>, String> {
    let cache = store.spend_key_window_cache.clone();
    let key_id = api_key_id.to_string();
    cache
        .get_or_load(&format!("key:{api_key_id}"), || {
            load_key_windows(store, &key_id)
        })
        .await
}

/// DPT-SW5: cached variant for forwarding admission preflight only.
pub async fn load_limit_levels_cached(
    store: &UserStore,
    org_id: &str,
    member_id: &str,
    api_key_id: &str,
) -> Result<OrgLimitLevels, String> {
    let cache = store.spend_org_level_cache.clone();
    let (org, member, key) = (org_id.to_string(), member_id.to_string(), api_key_id.to_string());
    cache
        .get_or_load(&format!("org:{org_id}:{member_id}:{api_key_id}"), || {
            load_limit_levels(store, &org, &member, &key)
        })
        .await
}

/// DPT-SW8: best-effort convergence after settlement attributes spend to the key.
pub fn invalidate_spend_windows_for_key(store: &UserStore, api_key_id: &str) {
    store.spend_key_window_cache.invalidate_for_api_key(api_key_id);
    store.spend_org_level_cache.invalidate_for_api_key(api_key_id);
}

/// DPT-SW9: after an org's spend limits change, drop its org-level entries.
pub fn invalidate_spend_windows_for_org(store: &UserStore, org_id: &str) {
    store.spend_org_level_cache.invalidate_org(org_id);
}

/// ORGL-19: key-level evaluation for a personal key. Delegates to
/// `evaluate_limits` with empty space/member levels, so the breach ordering
/// and the `key` level name stay identical to the org path.
pub fn evaluate_key_windows(windows: &OrgSpendWindows) -> Result<(), OrgLimitBreach> {
    evaluate_limits(&OrgLimitLevels {
        space: OrgSpendWindows::default(),
        member: None,
        key: Some(windows.clone()),
    })
}

/// ORGL-5: evaluate every configured window. The first breach wins; levels are
/// ordered space → member → key and windows total → hourly → daily for a stable
/// error message.
pub fn evaluate_limits(levels: &OrgLimitLevels) -> Result<(), OrgLimitBreach> {
    fn check(level: &'static str, w: &OrgSpendWindows) -> Option<OrgLimitBreach> {
        if let Some(limit) = w.total.limit_nano_usd
            && w.total.spent_nano_usd >= limit
        {
            return Some(OrgLimitBreach {
                level,
                window: "total",
            });
        }
        if let Some(limit) = w.hourly.limit_nano_usd
            && w.hourly.spent_nano_usd >= limit
        {
            return Some(OrgLimitBreach {
                level,
                window: "hourly",
            });
        }
        if let Some(limit) = w.daily.limit_nano_usd
            && w.daily.spent_nano_usd >= limit
        {
            return Some(OrgLimitBreach {
                level,
                window: "daily",
            });
        }
        None
    }
    check("space", &levels.space)
        .or_else(|| levels.member.as_ref().and_then(|m| check("member", m)))
        .or_else(|| levels.key.as_ref().and_then(|k| check("key", k)))
        .map_or(Ok(()), Err)
}

/// ORGL-13: per-member usage analysis over the org's durable attribution.
pub async fn member_usage(
    store: &UserStore,
    org_id: &str,
    range_hours: i64,
    buckets: i64,
) -> Result<serde_json::Value, String> {
    let is_postgres = store.db.is_postgres();
    let now = chrono::Utc::now();
    let time_from = now - chrono::Duration::hours(range_hours.max(1));
    let from_unix_ms = time_from.timestamp_millis();
    let bucket_ms = (range_hours.max(1) * 3_600_000) / buckets.max(1);

    let sum = sum_charge_expr(is_postgres, "rl");
    // is_member is an aggregate over the org_members LEFT JOIN so every selected
    // expression is either grouped (member_id, u.username, bucket_index) or
    // aggregated; a correlated subquery over k.created_by would be rejected by
    // PostgreSQL under this GROUP BY.
    let sql = format!(
        "SELECT COALESCE(k.created_by, o.owner_user_id) AS member_id,
                u.username AS member_username,
                MAX(om.alias) AS member_alias,
                CAST(CASE WHEN COUNT(om.user_id) > 0 THEN 1 ELSE 0 END AS BIGINT) AS is_member,
                {sum} AS total_charge,
                COUNT(*) AS calls,
                CAST(COALESCE(SUM(CASE WHEN rl.status = 'success' THEN rl.input_tokens ELSE 0 END), 0) AS BIGINT) AS input_tokens,
                CAST(COALESCE(SUM(CASE WHEN rl.status = 'success' THEN rl.output_tokens ELSE 0 END), 0) AS BIGINT) AS output_tokens,
                CAST(COALESCE(SUM(CASE WHEN rl.status = 'success' THEN rl.cache_read_tokens ELSE 0 END), 0) AS BIGINT) AS cache_read_tokens,
                (rl.created_at_unix_ms - $2) / $3 AS bucket_index
         FROM request_logs rl
         JOIN api_keys k ON k.id = rl.api_key_id
         JOIN orgs o ON o.id = $1
         LEFT JOIN users u ON u.id = COALESCE(k.created_by, o.owner_user_id)
         LEFT JOIN org_members om ON om.org_id = $1
                                 AND om.user_id = COALESCE(k.created_by, o.owner_user_id)
         WHERE rl.user_id = $1 AND rl.created_at_unix_ms >= $2
         GROUP BY member_id, u.username, bucket_index"
    );
    let rows = store
        .db
        .read()
        .query_all(store.db.stmt(
            &sql,
            vec![org_id.into(), from_unix_ms.into(), bucket_ms.into()],
        ))
        .await
        .map_err(|e| e.to_string())?;

    #[derive(Default)]
    struct MemberAgg {
        username: String,
        alias: Option<String>,
        is_member: bool,
        charge: i128,
        calls: i64,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_tokens: i64,
        by_model: std::collections::BTreeMap<String, (i128, i64)>,
        series: std::collections::BTreeMap<i64, (i128, i64)>,
    }
    let mut members: std::collections::BTreeMap<String, MemberAgg> =
        std::collections::BTreeMap::new();
    for row in &rows {
        let member_id: String = row.try_get("", "member_id").map_err(|e| e.to_string())?;
        let bucket_index: i64 = row.try_get("", "bucket_index").map_err(|e| e.to_string())?;
        let charge = limb_sum_to_i128(
            &row.try_get::<String>("", "total_charge")
                .map_err(|e| e.to_string())?,
        )?;
        let calls: i64 = row.try_get("", "calls").map_err(|e| e.to_string())?;
        let entry = members.entry(member_id).or_default();
        entry.username = row
            .try_get::<Option<String>>("", "member_username")
            .ok()
            .flatten()
            .unwrap_or_default();
        if entry.username.is_empty() {
            entry.username = "(removed member)".to_string();
        }
        // bucket rows repeat the alias; keep the first non-null value seen
        if entry.alias.is_none() {
            entry.alias = row
                .try_get::<Option<String>>("", "member_alias")
                .ok()
                .flatten();
        }
        entry.is_member = row
            .try_get::<i64>("", "is_member")
            .map_err(|e| e.to_string())? == 1;
        entry.charge = entry.charge.saturating_add(charge);
        entry.calls += calls;
        entry.input_tokens += row
            .try_get::<i64>("", "input_tokens")
            .map_err(|e| e.to_string())?;
        entry.output_tokens += row
            .try_get::<i64>("", "output_tokens")
            .map_err(|e| e.to_string())?;
        entry.cache_read_tokens += row
            .try_get::<i64>("", "cache_read_tokens")
            .map_err(|e| e.to_string())?;
        let series = entry.series.entry(bucket_index).or_default();
        series.0 = series.0.saturating_add(charge);
        series.1 += calls;
    }

    // by_model needs a second aggregation at model granularity.
    let model_sql = format!(
        "SELECT COALESCE(k.created_by, o.owner_user_id) AS member_id, rl.model AS model,
                {sum} AS total_charge, COUNT(*) AS calls,
                CAST(COALESCE(SUM(CASE WHEN rl.status = 'success' THEN rl.input_tokens ELSE 0 END), 0) AS BIGINT) AS input_tokens,
                CAST(COALESCE(SUM(CASE WHEN rl.status = 'success' THEN rl.output_tokens ELSE 0 END), 0) AS BIGINT) AS output_tokens
         FROM request_logs rl
         JOIN api_keys k ON k.id = rl.api_key_id
         JOIN orgs o ON o.id = $1
         WHERE rl.user_id = $1 AND rl.created_at_unix_ms >= $2
         GROUP BY member_id, rl.model"
    );
    let model_rows = store
        .db
        .read()
        .query_all(
            store
                .db
                .stmt(&model_sql, vec![org_id.into(), from_unix_ms.into()]),
        )
        .await
        .map_err(|e| e.to_string())?;
    for row in &model_rows {
        let member_id: String = row.try_get("", "member_id").map_err(|e| e.to_string())?;
        let model: String = row.try_get("", "model").map_err(|e| e.to_string())?;
        let charge = limb_sum_to_i128(
            &row.try_get::<String>("", "total_charge")
                .map_err(|e| e.to_string())?,
        )?;
        let calls: i64 = row.try_get("", "calls").map_err(|e| e.to_string())?;
        if let Some(entry) = members.get_mut(&member_id) {
            entry.by_model.insert(model, (charge, calls));
        }
    }

    use serde_json::json;
    let mut member_json = Vec::new();
    let mut removed_json = Vec::new();
    for (member_id, agg) in members {
        let series: Vec<serde_json::Value> = agg
            .series
            .iter()
            .map(|(bucket_index, (charge, calls))| {
                let bucket_start_ms = from_unix_ms + bucket_index * bucket_ms;
                json!({
                    "bucket_start": chrono::DateTime::from_timestamp_millis(bucket_start_ms)
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_default(),
                    "charge_nano_usd": charge.to_string(),
                    "calls": calls,
                })
            })
            .collect();
        let by_model: Vec<serde_json::Value> = agg
            .by_model
            .iter()
            .map(|(model, (charge, calls))| {
                json!({
                    "model": model,
                    "charge_nano_usd": charge.to_string(),
                    "calls": calls,
                })
            })
            .collect();
        let entry = json!({
            "user_id": member_id,
            "username": agg.username,
            "alias": agg.alias,
            "total_charge_nano_usd": agg.charge.to_string(),
            "calls": agg.calls,
            "input_tokens": agg.input_tokens,
            "output_tokens": agg.output_tokens,
            "cache_read_tokens": agg.cache_read_tokens,
            "by_model": by_model,
            "series": series,
        });
        if agg.is_member {
            member_json.push(entry);
        } else {
            removed_json.push(entry);
        }
    }
    Ok(json!({
        "range_hours": range_hours,
        "buckets": buckets,
        "members": member_json,
        "removed_members": removed_json,
    }))
}

/// ORGL-10/11 helper: normalize a limit-set patch value. `None` = leave unchanged
/// (members map), explicit null = clear.
pub fn parse_limit_patch(value: &serde_json::Value) -> Result<Option<Option<i128>>, String> {
    match value {
        serde_json::Value::Null => Ok(Some(None)),
        serde_json::Value::Number(n) => n
            .as_i64()
            .filter(|v| *v >= 0)
            .map(|v| Some(Some(v as i128)))
            .ok_or_else(|| "limit must be a non-negative integer".to_string()),
        serde_json::Value::String(s) => s
            .parse::<i128>()
            .ok()
            .filter(|v| *v >= 0)
            .map(|v| Some(Some(v)))
            .ok_or_else(|| format!("invalid limit value {s:?}")),
        _ => Err("limit must be a non-negative integer or null".to_string()),
    }
}

/// ORGL-10: apply the space + member limit patch. Absent member entries are left
/// unchanged; an explicit null inside a provided object clears the limit.
pub async fn apply_org_limit_patch(
    store: &UserStore,
    org_id: &str,
    space: &serde_json::Value,
    members: &serde_json::Value,
) -> Result<(), String> {
    if let Some(space_obj) = space.as_object() {
        let mut sets = Vec::new();
        let mut params: Vec<SeaValue> = vec![org_id.into()];
        let mut idx = 2usize;
        for (json_key, column) in [
            ("total_nano_usd", "spend_limit_total_nano_usd"),
            ("hourly_nano_usd", "spend_limit_hourly_nano_usd"),
            ("daily_nano_usd", "spend_limit_daily_nano_usd"),
        ] {
            if let Some(value) = space_obj.get(json_key) {
                if let Some(limit) = parse_limit_patch(value)? {
                    sets.push(format!("{column} = ${idx}"));
                    params.push(limit.map(|v| v.to_string()).into());
                    idx += 1;
                }
            }
        }
        if !sets.is_empty() {
            let sql = format!("UPDATE orgs SET {} WHERE id = $1", sets.join(", "));
            let tx = store.db.begin_write().await.map_err(|e| e.to_string())?;
            tx.execute(store.db.stmt(&sql, params))
                .await
                .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            invalidate_spend_windows_for_org(store, org_id);
        }
    }

    if let Some(members_obj) = members.as_object() {
        for (member_id, patch) in members_obj {
            let Some(patch_obj) = patch.as_object() else {
                return Err("each member entry must be an object".to_string());
            };
            let mut sets = Vec::new();
            let mut params: Vec<SeaValue> = vec![org_id.into(), member_id.clone().into()];
            let mut idx = 3usize;
            for (json_key, column) in [
                ("total_nano_usd", "spend_limit_total_nano_usd"),
                ("hourly_nano_usd", "spend_limit_hourly_nano_usd"),
                ("daily_nano_usd", "spend_limit_daily_nano_usd"),
            ] {
                if let Some(value) = patch_obj.get(json_key) {
                    if let Some(limit) = parse_limit_patch(value)? {
                        sets.push(format!("{column} = ${idx}"));
                        params.push(limit.map(|v| v.to_string()).into());
                        idx += 1;
                    }
                }
            }
            if !sets.is_empty() {
                // Setting limits on the owner member row is rejected (ORGL-10).
                let owner = store
                    .db
                    .read()
                    .query_one(store.db.stmt(
                        "SELECT owner_user_id FROM orgs WHERE id = $1",
                        vec![org_id.into()],
                    ))
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "org not found".to_string())?;
                let owner_id: String = owner
                    .try_get("", "owner_user_id")
                    .map_err(|e| e.to_string())?;
                if member_id == &owner_id {
                    return Err("owner member row carries no member limits".to_string());
                }
                let sql = format!(
                    "UPDATE org_members SET {} WHERE org_id = $1 AND user_id = $2",
                    sets.join(", ")
                );
                let changed = {
                    let tx = store.db.begin_write().await.map_err(|e| e.to_string())?;
                    let result = tx
                        .execute(store.db.stmt(&sql, params))
                        .await
                        .map_err(|e| e.to_string())?;
                    tx.commit().await.map_err(|e| e.to_string())?;
                    result.rows_affected()
                };
                if changed == 0 {
                    return Err(format!("member {member_id} not in org {org_id}"));
                }
            }
        }
    }
    Ok(())
}

/// ORGL-11: apply the key-level limit patch.
pub async fn apply_org_key_limit_patch(
    store: &UserStore,
    org_id: &str,
    api_key_id: &str,
    patch: &serde_json::Value,
) -> Result<(), String> {
    let Some(patch_obj) = patch.as_object() else {
        return Err("key limits must be an object".to_string());
    };
    let mut sets = Vec::new();
    let mut params: Vec<SeaValue> = vec![api_key_id.into(), org_id.into()];
    let mut idx = 3usize;
    for (json_key, column) in [
        ("total_nano_usd", "spend_limit_total_nano_usd"),
        ("hourly_nano_usd", "spend_limit_hourly_nano_usd"),
        ("daily_nano_usd", "spend_limit_daily_nano_usd"),
    ] {
        if let Some(value) = patch_obj.get(json_key) {
            if let Some(limit) = parse_limit_patch(value)? {
                sets.push(format!("{column} = ${idx}"));
                params.push(limit.map(|v| v.to_string()).into());
                idx += 1;
            }
        }
    }
    if !sets.is_empty() {
        let sql = format!(
            "UPDATE api_keys SET {} WHERE id = $1 AND org_id = $2",
            sets.join(", ")
        );
        let changed = {
            let tx = store.db.begin_write().await.map_err(|e| e.to_string())?;
            let result = tx
                .execute(store.db.stmt(&sql, params))
                .await
                .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            result.rows_affected()
        };
        if changed == 0 {
            return Err("key not found in this org".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;
    use crate::migration::Migrator;
    use crate::users::{UserRole, UserStore};
    use sea_orm_migration::MigratorTrait;

    async fn insert_org_wallet_user(store: &UserStore, org_id: &str) {
        let tx = store.db.begin_write().await.expect("wallet tx");
        tx.execute(store.db.stmt(
        "INSERT INTO users (id, username, password_hash, role, created_at, updated_at, enabled, is_org) VALUES ($1, $2, 'x', 'user', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1, 1)",
        vec![org_id.into(), format!("_monoize_org_{org_id}").into()],
    ))
    .await
    .expect("wallet user");
        tx.commit().await.expect("commit");
    }

    async fn test_store() -> UserStore {
        let db = DbPool::connect("sqlite::memory:")
            .await
            .expect("db connects");
        {
            let write = db.write().await;
            Migrator::up(&*write, None).await.expect("migrates");
        }
        let (log_broadcast, _) = tokio::sync::broadcast::channel(4);
        UserStore::new(db, log_broadcast).await.expect("store")
    }

    #[tokio::test]
    async fn limits_default_to_unlimited_and_pass() {
        let store = test_store().await;
        let owner = store
            .create_user("org-owner", "pw", UserRole::User, None)
            .await
            .expect("owner");
        let org = store
            .db
            .read()
            .query_one(store.db.stmt(
                "INSERT INTO orgs (id, owner_user_id, display_name, avatar_emoji, avatar_color, invite_token, invite_created_at, created_at, updated_at)
                 VALUES ($1, $2, 't', 'X', '#000000', 'tok-1234567890abcdef', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z') RETURNING id",
                vec!["test-org".into(), owner.id.clone().into()],
            ))
            .await
            .expect("org insert")
            .expect("row");
        let _ = org;
        insert_org_wallet_user(&store, "test-org").await;
        let member = store
            .create_user("org-member", "pw", UserRole::User, None)
            .await
            .unwrap_or_else(|e| panic!("member create failed: {e}"));
        {
            let tx = store.db.begin_write().await.expect("tx");
            tx.execute(store.db.stmt(
                "INSERT INTO org_members (org_id, user_id, role, joined_at) VALUES ($1, $2, 'member', '2026-01-01T00:00:00Z')",
                vec!["test-org".into(), member.id.clone().into()],
            ))
            .await
            .expect("member insert");
            tx.commit().await.expect("commit");
        }
        let key_id = "key-1";
        {
            let tx = store.db.begin_write().await.expect("tx");
            tx.execute(store.db.stmt(
                "INSERT INTO api_keys (id, user_id, name, key_prefix, key, created_at, enabled, sub_account_enabled, sub_account_balance_nano, model_limits_enabled, model_limits, ip_whitelist, group_ids, channel_bindings, model_bindings, transforms, model_redirects, reasoning_envelope_enabled, request_capture_enabled, org_id, created_by)
                 VALUES ($1, $2, 'k', 'sk-prefix000', 'sk-test-key-0000000000000001', '2026-01-01T00:00:00Z', 1, 0, '0', 0, '[]', '[]', '[]', '[]', '[]', '[]', '[]', 1, 0, $3, $4)",
                vec![
                    key_id.into(),
                    "test-org".into(),
                    "test-org".into(),
                    member.id.clone().into(),
                ],
            ))
            .await
            .expect("key insert");
            tx.commit().await.expect("commit");
        }
        // Record one billed request attributed to the org.
        {
            let tx = store.db.begin_write().await.expect("tx");
            tx.execute(store.db.stmt(
                "INSERT INTO request_logs (id, request_id, user_id, api_key_id, model, is_stream, status, charge_nano_usd, created_at, created_at_unix_ms, input_tokens, output_tokens)
                 VALUES ('log-1', 'r1', $1, $2, 'm', 0, 'success', '1000', '2026-01-01T00:00:00Z', 0, 10, 5)",
                vec!["test-org".into(), key_id.into()],
            ))
            .await
            .expect("log insert");
            tx.commit().await.expect("commit");
        }

        let levels = load_limit_levels(&store, "test-org", &member.id, key_id)
            .await
            .expect("levels");
        assert!(levels.space.total.limit_nano_usd.is_none());
        assert_eq!(levels.space.total.spent_nano_usd, 1000);
        let member_windows = levels
            .member
            .as_ref()
            .unwrap_or_else(|| panic!("member windows missing: {:?}", levels))
            .total
            .spent_nano_usd;
        assert_eq!(member_windows, 1000);
        assert_eq!(levels.key.as_ref().expect("key").total.spent_nano_usd, 1000);
        assert!(
            evaluate_limits(&levels).is_ok(),
            "no limits configured => pass"
        );
    }

    #[tokio::test]
    async fn total_limit_breaches_at_settlement_threshold() {
        let windows = OrgSpendWindows {
            total: OrgSpendWindow {
                limit_nano_usd: Some(1000),
                spent_nano_usd: 1000,
            },
            hourly: OrgSpendWindow::default(),
            daily: OrgSpendWindow::default(),
        };
        let breach = evaluate_limits(&OrgLimitLevels {
            space: windows,
            member: None,
            key: None,
        })
        .expect_err("spent == limit => breach");
        assert_eq!(breach.level, "space");
        assert_eq!(breach.window, "total");
    }

    #[tokio::test]
    async fn hourly_member_limit_reports_member_level() {
        let member = OrgSpendWindows {
            hourly: OrgSpendWindow {
                limit_nano_usd: Some(500),
                spent_nano_usd: 600,
            },
            ..OrgSpendWindows::default()
        };
        let breach = evaluate_limits(&OrgLimitLevels {
            space: OrgSpendWindows::default(),
            member: Some(member),
            key: None,
        })
        .expect_err("breach");
        assert_eq!(breach.level, "member");
        assert_eq!(breach.window, "hourly");
    }

    #[tokio::test]
    async fn key_daily_limit_reports_key_level_and_ordering() {
        let key = OrgSpendWindows {
            daily: OrgSpendWindow {
                limit_nano_usd: Some(1),
                spent_nano_usd: 2,
            },
            ..OrgSpendWindows::default()
        };
        let space = OrgSpendWindows {
            total: OrgSpendWindow {
                limit_nano_usd: Some(10),
                spent_nano_usd: 10,
            },
            ..OrgSpendWindows::default()
        };
        let breach = evaluate_limits(&OrgLimitLevels {
            space,
            member: None,
            key: Some(key),
        })
        .expect_err("breach");
        assert_eq!(breach.level, "space", "space total is evaluated first");
    }

    #[test]
    fn limit_patch_parses_values_and_nulls() {
        assert_eq!(
            parse_limit_patch(&serde_json::json!("1500")).expect("string"),
            Some(Some(1500))
        );
        assert_eq!(
            parse_limit_patch(&serde_json::json!(1500)).expect("number"),
            Some(Some(1500))
        );
        assert_eq!(
            parse_limit_patch(&serde_json::json!(null)).expect("null clears"),
            Some(None)
        );
        assert!(parse_limit_patch(&serde_json::json!(-1)).is_err());
        assert!(parse_limit_patch(&serde_json::json!("abc")).is_err());
        assert!(parse_limit_patch(&serde_json::json!(true)).is_err());
    }

    #[tokio::test]
    async fn limit_patch_updates_space_and_rejects_owner_member_row() {
        let store = test_store().await;
        let owner = store
            .create_user("patch-owner", "pw", UserRole::User, None)
            .await
            .expect("owner");
        {
            let tx = store.db.begin_write().await.expect("tx");
            tx.execute(store.db.stmt(
                "INSERT INTO orgs (id, owner_user_id, display_name, avatar_emoji, avatar_color, invite_token, invite_created_at, created_at, updated_at)
                 VALUES ('patch-org', $1, 't', 'X', '#000000', 'tok-2234567890abcdef', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                vec![owner.id.clone().into()],
            ))
            .await
            .expect("org insert");
            tx.commit().await.expect("commit");
        }
        apply_org_limit_patch(
            &store,
            "patch-org",
            &serde_json::json!({"hourly_nano_usd": "5000"}),
            &serde_json::json!({}),
        )
        .await
        .expect("patch applies");

        let row = store
            .db
            .read()
            .query_one(store.db.stmt(
                "SELECT spend_limit_hourly_nano_usd FROM orgs WHERE id = $1",
                vec!["patch-org".into()],
            ))
            .await
            .expect("read")
            .expect("row");
        let limit: String = row.try_get("", "spend_limit_hourly_nano_usd").expect("v");
        assert_eq!(limit, "5000");

        // ORGL-10: patching the owner member row is rejected.
        let mut members_patch = serde_json::Map::new();
        members_patch.insert(
            owner.id.clone(),
            serde_json::json!({"daily_nano_usd": "100"}),
        );
        let err = apply_org_limit_patch(
            &store,
            "patch-org",
            &serde_json::json!({}),
            &serde_json::Value::Object(members_patch),
        )
        .await
        .expect_err("owner member rejected");
        assert!(err.contains("owner"));
    }

    #[tokio::test]
    async fn member_usage_groups_by_creator_and_flags_removed() {
        let store = test_store().await;
        assert_member_usage(store).await;
    }

    #[tokio::test]
    #[ignore = "requires an explicitly disposable PostgreSQL database"]
    async fn postgres_member_usage_preserves_tokens_and_membership() {
        let dsn = std::env::var("MONOIZE_TEST_POSTGRES_DSN").expect("disposable PG DSN");
        let db = DbPool::connect(&dsn).await.expect("connect");
        let row = db.read().query_one(db.stmt(
            "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema='public'",
            vec![],
        )).await.unwrap().unwrap();
        assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0, "refuse populated database");
        Migrator::up(&*db.write().await, None).await.expect("migrate");
        let (sender, _) = tokio::sync::broadcast::channel(4);
        let store = UserStore::new(db, sender).await.expect("store");
        assert_member_usage(store).await;
    }

    async fn assert_member_usage(store: UserStore) {
        let owner = store
            .create_user("mu-owner", "pw", UserRole::User, None)
            .await
            .expect("owner");
        let member = store
            .create_user("mu-member", "pw", UserRole::User, None)
            .await
            .expect("member");
        {
            let tx = store.db.begin_write().await.expect("tx");
            tx.execute(store.db.stmt(
                "INSERT INTO orgs (id, owner_user_id, display_name, avatar_emoji, avatar_color, invite_token, invite_created_at, created_at, updated_at)
                 VALUES ('mu-org', $1, 't', 'X', '#000000', 'tok-3234567890abcdef', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                vec![owner.id.clone().into()],
            ))
            .await
            .expect("org");
            tx.execute(store.db.stmt(
                "INSERT INTO users (id, username, password_hash, role, created_at, updated_at, enabled, is_org) VALUES ('mu-org', '_monoize_org_mu', 'x', 'user', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1, 1)",
                vec![],
            ))
            .await
            .expect("wallet user");
            tx.execute(store.db.stmt(
                "INSERT INTO org_members (org_id, user_id, role, joined_at) VALUES ('mu-org', $1, 'member', '2026-01-01T00:00:00Z')",
                vec![member.id.clone().into()],
            ))
            .await
            .expect("member");
            tx.execute(store.db.stmt(
                "INSERT INTO api_keys (id, user_id, name, key_prefix, key, created_at, enabled, sub_account_enabled, sub_account_balance_nano, model_limits_enabled, model_limits, ip_whitelist, group_ids, channel_bindings, model_bindings, transforms, model_redirects, reasoning_envelope_enabled, request_capture_enabled, org_id, created_by)
                 VALUES ('mu-key', 'mu-org', 'k', 'sk-prefix001', 'sk-test-key-0000000000000002', '2026-01-01T00:00:00Z', 1, 0, '0', 0, '[]', '[]', '[]', '[]', '[]', '[]', '[]', 1, 0, 'mu-org', $1)",
                vec![member.id.clone().into()],
            ))
            .await
            .expect("key");
            tx.commit().await.expect("commit");
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        {
            let tx = store.db.begin_write().await.expect("tx");
            tx.execute(store.db.stmt(
                "INSERT INTO request_logs (id, request_id, user_id, api_key_id, model, is_stream, status, charge_nano_usd, created_at, created_at_unix_ms, input_tokens, output_tokens)
                 VALUES ('log-mu1', 'mu-r1', 'mu-org', 'mu-key', 'model-a', 0, 'success', '700', $1, $2, 3000000030, 7)",
                vec![chrono::Utc::now().to_rfc3339().into(), now_ms.into()],
            ))
            .await
            .expect("log");
            tx.commit().await.expect("commit");
        }

        let value = member_usage(&store, "mu-org", 24, 4).await.expect("usage");
        let members = value
            .get("members")
            .and_then(|v| v.as_array())
            .expect("members");
        assert_eq!(members.len(), 1);
        let m = &members[0];
        assert_eq!(m["user_id"].as_str().expect("id"), member.id);
        assert_eq!(m["total_charge_nano_usd"].as_str().expect("charge"), "700");
        assert_eq!(m["calls"].as_i64().expect("calls"), 1);
        assert_eq!(m["input_tokens"].as_i64(), Some(3_000_000_030));
        assert_eq!(m["output_tokens"].as_i64(), Some(7));
        assert_eq!(m["cache_read_tokens"].as_i64(), Some(0));
        assert!(value["removed_members"].as_array().unwrap().is_empty());
        let by_model = m["by_model"].as_array().expect("by_model");
        assert_eq!(by_model[0]["model"].as_str().expect("model"), "model-a");
        store.db.write().await.execute(store.db.stmt(
            "DELETE FROM org_members WHERE org_id='mu-org'", vec![],
        )).await.unwrap();
        let removed = member_usage(&store, "mu-org", 24, 4).await.expect("removed usage");
        assert!(removed["members"].as_array().unwrap().is_empty());
        assert_eq!(removed["removed_members"][0]["input_tokens"].as_i64(), Some(3_000_000_030));
    }
}

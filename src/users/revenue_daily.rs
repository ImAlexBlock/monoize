use crate::beijing_time::{beijing_day_end_utc, beijing_day_id, beijing_day_start_utc};
use crate::db::DbPool;
use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Asia::Shanghai;
use sea_orm::ConnectionTrait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;

/// `request-logs.spec.md` RL-S9: the retention limit constrains how far back a
/// persisted day can be recomputed from source rows.
pub const REVENUE_RECOMPUTE_LOOKBACK_DAYS: i64 = 365;

/// One per-model aggregate of a single Beijing day (AR-10).
#[derive(Debug, Clone)]
pub struct RevenueModelRow {
    pub model: String,
    pub charge_nano_usd: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// One per-user aggregate of a single Beijing day (AR-10), with that user's
/// per-model breakdown.
#[derive(Debug, Clone)]
pub struct RevenueUserRow {
    pub user_id: String,
    pub username: Option<String>,
    pub charge_nano_usd: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub models: Vec<RevenueModelRow>,
}

/// One day of the admin revenue report (AR-10).
#[derive(Debug, Clone)]
pub struct RevenueDayRow {
    pub day: String,
    pub total_charge_nano_usd: String,
    pub total_calls: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub models: Vec<RevenueModelRow>,
    pub users: Vec<RevenueUserRow>,
}

/// An exclusion-list entry (AR-11).
#[derive(Debug, Clone)]
pub struct RevenueExclusionRow {
    pub user_id: String,
    pub username: String,
    pub created_at: String,
}

/// The Beijing-day SQL expression over the request-log table alias `rl`.
/// SQLite has no timezone tables, so the fixed +8 hours modifier reproduces the
/// Asia/Shanghai local date exactly (the zone has no DST). Legacy rows with a
/// null `created_at_unix_ms` fall back to the RFC3339 `created_at` column.
fn beijing_day_expr(is_sqlite: bool) -> &'static str {
    if is_sqlite {
        "COALESCE(strftime('%Y-%m-%d', rl.created_at_unix_ms / 1000, 'unixepoch', '+8 hours'), \
         substr(rl.created_at, 1, 10))"
    } else {
        "COALESCE(to_char(to_timestamp(rl.created_at_unix_ms / 1000.0) AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD'), \
         to_char(rl.created_at::timestamp AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD'))"
    }
}

/// AR-2 revenue predicate: only canonical in-range charges, no probe rows, no
/// excluded users.
fn revenue_predicate(is_sqlite: bool) -> String {
    let digits = "(CASE WHEN SUBSTR(rl.charge_nano_usd, 1, 1) = '-' THEN SUBSTR(rl.charge_nano_usd, 2) ELSE rl.charge_nano_usd END)";
    let canonical = if is_sqlite {
        "(rl.charge_nano_usd = '0' OR (SUBSTR(rl.charge_nano_usd, 1, 1) BETWEEN '1' AND '9' AND rl.charge_nano_usd NOT GLOB '*[^0-9]*') OR (SUBSTR(rl.charge_nano_usd, 1, 1) = '-' AND SUBSTR(rl.charge_nano_usd, 2, 1) BETWEEN '1' AND '9' AND {digits} NOT GLOB '*[^0-9]*'))".replace(
            "{digits}", digits,
        )
    } else {
        "rl.charge_nano_usd ~ '^-?(0|[1-9][0-9]*)$'".to_string()
    };
    format!(
        "rl.charge_nano_usd IS NOT NULL AND {canonical} \
         AND (rl.request_kind IS NULL OR rl.request_kind <> '{probe_kind}')",
        probe_kind = crate::app::ACTIVE_PROBE_CONNECTIVITY_KIND
    )
}

fn excluded_users_predicate(excluded: &[String]) -> String {
    if excluded.is_empty() {
        String::new()
    } else {
        // The ids come from the admin_revenue_exclusions table, never from
        // user input, so quoting is bounded by the stored user ids themselves.
        let quoted = excluded
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" AND rl.user_id NOT IN ({quoted})")
    }
}

/// Decode the per-model charge aggregate of one group row.
fn decode_charge_total(row: &sea_orm::QueryResult, is_postgres: bool) -> Result<i128, String> {
    if is_postgres {
        let total: String = row
            .try_get("", "total_charge_nano_usd")
            .map_err(|e| e.to_string())?;
        return total
            .parse::<i128>()
            .map_err(|_| "revenue charge aggregate overflow".to_string());
    }
    // SQLite: SUM over INTEGER can overflow 64 bits for extreme volumes, so the
    // sum is taken over five 9-digit limbs exactly like the analytics
    // `charge_aggregate_columns` helper.
    let mut total = 0i128;
    let mut scale = 1i128;
    for limb in 0..5 {
        let value: i64 = row
            .try_get("", &format!("charge_limb_{limb}"))
            .map_err(|e| e.to_string())?;
        total = total
            .checked_add(
                i128::from(value)
                    .checked_mul(scale)
                    .ok_or_else(|| "revenue charge aggregate overflow".to_string())?,
            )
            .ok_or_else(|| "revenue charge aggregate overflow".to_string())?;
        if limb < 4 {
            scale = scale
                .checked_mul(1_000_000_000)
                .ok_or_else(|| "revenue charge aggregate overflow".to_string())?;
        }
    }
    Ok(total)
}

/// Aggregate the revenue of one Beijing day live from `request_logs`.
/// Returns `None` when the day has no matching rows (AR-10: zero-revenue days
/// are omitted).
pub async fn aggregate_revenue_day(
    db: &DbPool,
    day: &str,
    excluded_user_ids: &[String],
) -> Result<Option<RevenueDayRow>, String> {
    let is_sqlite = db.is_sqlite();
    let start = beijing_day_start_utc(day).ok_or_else(|| format!("invalid day id: {day}"))?;
    let end = beijing_day_end_utc(day).ok_or_else(|| format!("invalid day id: {day}"))?;
    let start_ms = start.timestamp_millis();
    let end_ms = end.timestamp_millis();

    let day_expr = beijing_day_expr(is_sqlite);
    let predicate = revenue_predicate(is_sqlite);
    let exclusion = excluded_users_predicate(excluded_user_ids);

    // SQLite SUM over INTEGER can overflow 64 bits for extreme volumes, so the
    // charge is summed through five 9-digit limbs exactly like the analytics
    // `charge_aggregate_columns` helper; PostgreSQL sums a NUMERIC cast.
    let charge_group_select = if db.is_postgres() {
        "COALESCE(SUM(CAST(rl.charge_nano_usd AS NUMERIC)), 0)::TEXT AS total_charge_nano_usd"
            .to_string()
    } else {
        let padded = format!(
            "('000000000000000000000000000000000000000000000' || (CASE WHEN SUBSTR(rl.charge_nano_usd, 1, 1) = '-' THEN SUBSTR(rl.charge_nano_usd, 2) ELSE rl.charge_nano_usd END))"
        );
        let sign = "(CASE WHEN SUBSTR(rl.charge_nano_usd, 1, 1) = '-' THEN -1 ELSE 1 END)";
        let mut limbs = Vec::new();
        for limb in 0..5 {
            let start = -9 * (limb + 1);
            limbs.push(format!(
                "COALESCE(SUM({sign} * CAST(SUBSTR({padded}, {start}, 9) AS INTEGER)), 0) AS charge_limb_{limb}"
            ));
        }
        limbs.join(", ")
    };

    // One scan answers the day total, the per-model rows, and the per-user rows;
    // the users join resolves the username snapshot per AR-3.
    let sql = format!(
        "SELECT {day_expr} AS day_id, rl.model AS model, rl.user_id AS user_id, \
         u.username AS username, \
         {charge_group_select}, \
         COUNT(*) AS calls, \
         CAST(COALESCE(SUM(COALESCE(rl.input_tokens, 0)), 0) AS BIGINT) AS input_tokens, \
         CAST(COALESCE(SUM(COALESCE(rl.output_tokens, 0)), 0) AS BIGINT) AS output_tokens \
         FROM request_logs rl \
         LEFT JOIN users u ON u.id = rl.user_id \
         WHERE {predicate}{exclusion} \
         AND ((rl.created_at_unix_ms IS NOT NULL AND rl.created_at_unix_ms >= $1 AND rl.created_at_unix_ms < $2) \
              OR (rl.created_at_unix_ms IS NULL AND rl.created_at >= $3 AND rl.created_at < $4)) \
         GROUP BY {day_expr}, rl.model, rl.user_id, u.username",
        day_expr = day_expr,
        charge_group_select = charge_group_select,
        predicate = predicate,
        exclusion = exclusion,
    );
    let rows = db
        .read()
        .query_all(db.stmt(
            &sql,
            vec![
                start_ms.into(),
                end_ms.into(),
                start.to_rfc3339().into(),
                end.to_rfc3339().into(),
            ],
        ))
        .await
        .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        return Ok(None);
    }
    let is_postgres = db.is_postgres();
    // The single scan groups by (day, model, user), so one model can appear in
    // many rows — one per consuming user. Both the model and the user detail
    // merge those per-user groups back into one row per model / per user.
    let mut models_by_name: std::collections::HashMap<String, RevenueModelRow> =
        std::collections::HashMap::new();
    let mut users_by_id: std::collections::HashMap<String, RevenueUserRow> =
        std::collections::HashMap::new();
    let mut total_calls = 0i64;
    let mut total_input = 0i64;
    let mut total_output = 0i64;
    for row in &rows {
        let charge = decode_charge_total(row, is_postgres)?;
        let calls: i64 = row.try_get("", "calls").map_err(|e| e.to_string())?;
        let input: i64 = row.try_get("", "input_tokens").map_err(|e| e.to_string())?;
        let output: i64 = row
            .try_get("", "output_tokens")
            .map_err(|e| e.to_string())?;
        let model: String = row.try_get("", "model").map_err(|e| e.to_string())?;
        total_calls = total_calls.saturating_add(calls);
        total_input = total_input.saturating_add(input);
        total_output = total_output.saturating_add(output);
        let model_entry = models_by_name
            .entry(model.clone())
            .or_insert_with(|| RevenueModelRow {
                model: String::new(),
                charge_nano_usd: "0".to_string(),
                calls: 0,
                input_tokens: 0,
                output_tokens: 0,
            });
        model_entry.calls = model_entry.calls.saturating_add(calls);
        model_entry.input_tokens = model_entry.input_tokens.saturating_add(input);
        model_entry.output_tokens = model_entry.output_tokens.saturating_add(output);
        let mut model_total = model_entry
            .charge_nano_usd
            .parse::<i128>()
            .map_err(|_| "revenue charge aggregate overflow".to_string())?;
        model_total = model_total
            .checked_add(charge)
            .ok_or_else(|| "revenue charge aggregate overflow".to_string())?;
        model_entry.charge_nano_usd = model_total.to_string();
        let user_id: String = row.try_get("", "user_id").map_err(|e| e.to_string())?;
        let username: Option<String> = row.try_get("", "username").ok();
        let entry = users_by_id
            .entry(user_id)
            .or_insert_with(|| RevenueUserRow {
                user_id: String::new(),
                username: username.clone(),
                charge_nano_usd: "0".to_string(),
                calls: 0,
                input_tokens: 0,
                output_tokens: 0,
                models: Vec::new(),
            });
        entry.calls = entry.calls.saturating_add(calls);
        entry.input_tokens = entry.input_tokens.saturating_add(input);
        entry.output_tokens = entry.output_tokens.saturating_add(output);
        if entry.username.is_none() {
            entry.username = username;
        }
        // The raw row IS this user's (model) group, so it feeds the per-user
        // model breakdown directly.
        entry.models.push(RevenueModelRow {
            model: model.clone(),
            charge_nano_usd: charge.to_string(),
            calls,
            input_tokens: input,
            output_tokens: output,
        });
        let mut total = entry
            .charge_nano_usd
            .parse::<i128>()
            .map_err(|_| "revenue charge aggregate overflow".to_string())?;
        total = total
            .checked_add(charge)
            .ok_or_else(|| "revenue charge aggregate overflow".to_string())?;
        entry.charge_nano_usd = total.to_string();
    }
    // Fill the name keys into their own rows now that the maps are complete.
    let mut models = Vec::new();
    for (name, mut row) in models_by_name {
        row.model = name;
        models.push(row);
    }
    for (id, row) in users_by_id.iter_mut() {
        row.user_id = id.clone();
        sort_revenue_models(&mut row.models);
    }
    let total_charge = models
        .iter()
        .map(|row| {
            row.charge_nano_usd
                .parse::<i128>()
                .map_err(|_| "revenue charge aggregate overflow".to_string())
        })
        .collect::<Result<Vec<i128>, String>>()?
        .into_iter()
        .fold(0i128, |acc, value| acc.saturating_add(value));
    sort_revenue_models(&mut models);
    let mut users = users_by_id.into_values().collect::<Vec<_>>();
    sort_revenue_users(&mut users);
    Ok(Some(RevenueDayRow {
        day: day.to_string(),
        total_charge_nano_usd: total_charge.to_string(),
        total_calls,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        models,
        users,
    }))
}

/// AR-10: models ordered by charge descending, then model ascending in UTF-8
/// byte order.
fn sort_revenue_models(models: &mut [RevenueModelRow]) {
    models.sort_by(|left, right| {
        let left_charge = left.charge_nano_usd.parse::<i128>().unwrap_or(0);
        let right_charge = right.charge_nano_usd.parse::<i128>().unwrap_or(0);
        right_charge
            .cmp(&left_charge)
            .then_with(|| left.model.as_bytes().cmp(right.model.as_bytes()))
    });
}

/// AR-10: users ordered by charge descending, then user id ascending in UTF-8
/// byte order.
fn sort_revenue_users(users: &mut [RevenueUserRow]) {
    users.sort_by(|left, right| {
        let left_charge = left.charge_nano_usd.parse::<i128>().unwrap_or(0);
        let right_charge = right.charge_nano_usd.parse::<i128>().unwrap_or(0);
        right_charge
            .cmp(&left_charge)
            .then_with(|| left.user_id.as_bytes().cmp(right.user_id.as_bytes()))
    });
}

pub async fn list_revenue_exclusions(db: &DbPool) -> Result<Vec<RevenueExclusionRow>, String> {
    let rows = db
        .read()
        .query_all(db.stmt(
            "SELECT user_id, username, created_at FROM admin_revenue_exclusions ORDER BY created_at ASC",
            vec![],
        ))
        .await
        .map_err(|e| e.to_string())?;
    rows.into_iter()
        .map(|row| {
            Ok(RevenueExclusionRow {
                user_id: row.try_get("", "user_id").map_err(|e| e.to_string())?,
                username: row.try_get("", "username").map_err(|e| e.to_string())?,
                created_at: row.try_get("", "created_at").map_err(|e| e.to_string())?,
            })
        })
        .collect()
}

/// AR-4: recompute one fully-elapsed day and persist it atomically. A day with
/// no matching rows keeps no persisted rows at all.
pub async fn persist_revenue_day(db: &DbPool, day: &str) -> Result<(), String> {
    let excluded = list_revenue_exclusions(db)
        .await?
        .into_iter()
        .map(|row| row.user_id)
        .collect::<Vec<_>>();
    let aggregate = aggregate_revenue_day(db, day, &excluded).await?;
    let computed_at = Utc::now().to_rfc3339();

    let txn = db.begin_write().await.map_err(|e| e.to_string())?;
    txn.execute(db.stmt(
        "DELETE FROM admin_revenue_daily_model_rows WHERE day = $1",
        vec![day.into()],
    ))
    .await
    .map_err(|e| e.to_string())?;
    txn.execute(db.stmt(
        "DELETE FROM admin_revenue_daily_user_rows WHERE day = $1",
        vec![day.into()],
    ))
    .await
    .map_err(|e| e.to_string())?;
    txn.execute(db.stmt(
        "DELETE FROM admin_revenue_daily_user_model_rows WHERE day = $1",
        vec![day.into()],
    ))
    .await
    .map_err(|e| e.to_string())?;
    txn.execute(db.stmt(
        "DELETE FROM admin_revenue_daily_summaries WHERE day = $1",
        vec![day.into()],
    ))
    .await
    .map_err(|e| e.to_string())?;
    if let Some(aggregate) = aggregate {
        txn.execute(db.stmt(
            "INSERT INTO admin_revenue_daily_summaries \
             (id, day, total_charge_nano_usd, total_calls, total_input_tokens, total_output_tokens, computed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            vec![
                uuid::Uuid::new_v4().to_string().into(),
                aggregate.day.clone().into(),
                aggregate.total_charge_nano_usd.clone().into(),
                aggregate.total_calls.into(),
                aggregate.total_input_tokens.into(),
                aggregate.total_output_tokens.into(),
                computed_at.clone().into(),
            ],
        ))
        .await
        .map_err(|e| e.to_string())?;
        for row in &aggregate.models {
            txn.execute(db.stmt(
                "INSERT INTO admin_revenue_daily_model_rows \
                 (id, day, model, charge_nano_usd, calls, input_tokens, output_tokens) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                vec![
                    uuid::Uuid::new_v4().to_string().into(),
                    aggregate.day.clone().into(),
                    row.model.clone().into(),
                    row.charge_nano_usd.clone().into(),
                    row.calls.into(),
                    row.input_tokens.into(),
                    row.output_tokens.into(),
                ],
            ))
            .await
            .map_err(|e| e.to_string())?;
        }
        for row in &aggregate.users {
            txn.execute(db.stmt(
                "INSERT INTO admin_revenue_daily_user_rows \
                 (id, day, user_id, username, charge_nano_usd, calls, input_tokens, output_tokens) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                vec![
                    uuid::Uuid::new_v4().to_string().into(),
                    aggregate.day.clone().into(),
                    row.user_id.clone().into(),
                    row.username
                        .clone()
                        .map(|name| sea_orm::Value::String(Some(Box::new(name))))
                        .unwrap_or(sea_orm::Value::String(None)),
                    row.charge_nano_usd.clone().into(),
                    row.calls.into(),
                    row.input_tokens.into(),
                    row.output_tokens.into(),
                ],
            ))
            .await
            .map_err(|e| e.to_string())?;
            for model_row in &row.models {
                txn.execute(db.stmt(
                    "INSERT INTO admin_revenue_daily_user_model_rows \
                     (id, day, user_id, model, charge_nano_usd, calls, input_tokens, output_tokens) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                    vec![
                        uuid::Uuid::new_v4().to_string().into(),
                        aggregate.day.clone().into(),
                        row.user_id.clone().into(),
                        model_row.model.clone().into(),
                        model_row.charge_nano_usd.clone().into(),
                        model_row.calls.into(),
                        model_row.input_tokens.into(),
                        model_row.output_tokens.into(),
                    ],
                ))
                .await
                .map_err(|e| e.to_string())?;
            }
        }
    }
    txn.commit().await.map_err(|e| e.to_string())
}

/// AR-6: settle every fully-elapsed day that is missing from the persisted
/// tables and not older than the retention limit. The earliest recomputable
/// day is derived from the oldest surviving request-log row so a fresh
/// deployment does not iterate 365 empty days.
pub async fn settle_elapsed_days(db: &DbPool, now: DateTime<Utc>) -> Result<u32, String> {
    let today = beijing_day_id(now);
    let Some(yesterday) = crate::beijing_time::beijing_day_shift(&today, -1) else {
        return Ok(0);
    };

    let oldest_day: Option<String> = db
        .read()
        .query_one(db.stmt(
            "SELECT MIN(day) AS oldest FROM admin_revenue_daily_summaries",
            vec![],
        ))
        .await
        .map_err(|e| e.to_string())?
        .and_then(|row| row.try_get::<String>("", "oldest").ok());

    let earliest_source_day = oldest_earliest_recomputable_day(db, now).await?;
    let start_day = match (oldest_day, earliest_source_day) {
        (Some(persisted), Some(source)) => persisted.min(source),
        (Some(persisted), None) => persisted,
        (None, Some(source)) => source,
        (None, None) => return Ok(0),
    };

    let Some(days) = crate::beijing_time::beijing_day_ids(&start_day, &yesterday) else {
        return Ok(0);
    };
    // A day counts as settled only when it has user-model rows: days settled
    // by a pre-000107 build carry a summary and user rows but no per-user
    // model detail, so they must be recomputed once to backfill it.
    let settled_days = db
        .read()
        .query_all(db.stmt(
            "SELECT s.day AS day \
             FROM admin_revenue_daily_summaries s \
             JOIN admin_revenue_daily_user_model_rows um ON um.day = s.day \
             GROUP BY s.day",
            vec![],
        ))
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|row| row.try_get::<String>("", "day").ok())
        .collect::<std::collections::HashSet<_>>();

    let mut settled = 0u32;
    for day in days {
        if settled_days.contains(&day) {
            continue;
        }
        persist_revenue_day(db, &day).await?;
        settled += 1;
    }
    Ok(settled)
}

/// The earliest day whose source rows may still exist: the older of the
/// retention cutoff day and the oldest surviving request-log day.
async fn oldest_earliest_recomputable_day(
    db: &DbPool,
    now: DateTime<Utc>,
) -> Result<Option<String>, String> {
    let cutoff = now - Duration::days(REVENUE_RECOMPUTE_LOOKBACK_DAYS);
    let row = db
        .read()
        .query_one(db.stmt(
            "SELECT MIN(created_at_unix_ms) AS oldest_ms FROM request_logs",
            vec![],
        ))
        .await
        .map_err(|e| e.to_string())?;
    let oldest_ms: Option<i64> = row.and_then(|row| row.try_get::<i64>("", "oldest_ms").ok());
    let Some(oldest_ms) = oldest_ms else {
        return Ok(None);
    };
    let oldest_instant = DateTime::from_timestamp_millis(oldest_ms).ok_or("invalid timestamp")?;
    let earliest = oldest_instant.min(cutoff);
    Ok(Some(beijing_day_id(earliest)))
}

/// AR-10: read the persisted day rows in `from..=to` (descending).
pub async fn list_persisted_revenue_days(
    db: &DbPool,
    from: &str,
    to: &str,
) -> Result<Vec<RevenueDayRow>, String> {
    let summaries = db
        .read()
        .query_all(db.stmt(
            "SELECT day, total_charge_nano_usd, CAST(total_calls AS BIGINT) AS total_calls, total_input_tokens, total_output_tokens \
             FROM admin_revenue_daily_summaries WHERE day >= $1 AND day <= $2 ORDER BY day DESC",
            vec![from.into(), to.into()],
        ))
        .await
        .map_err(|e| e.to_string())?;
    let model_rows = db
        .read()
        .query_all(db.stmt(
            "SELECT day, model, charge_nano_usd, CAST(calls AS BIGINT) AS calls, input_tokens, output_tokens \
             FROM admin_revenue_daily_model_rows WHERE day >= $1 AND day <= $2",
            vec![from.into(), to.into()],
        ))
        .await
        .map_err(|e| e.to_string())?;
    let user_rows = db
        .read()
        .query_all(db.stmt(
            "SELECT day, user_id, username, charge_nano_usd, CAST(calls AS BIGINT) AS calls, input_tokens, output_tokens \
             FROM admin_revenue_daily_user_rows WHERE day >= $1 AND day <= $2",
            vec![from.into(), to.into()],
        ))
        .await
        .map_err(|e| e.to_string())?;
    let user_model_rows = db
        .read()
        .query_all(db.stmt(
            "SELECT day, user_id, model, charge_nano_usd, CAST(calls AS BIGINT) AS calls, input_tokens, output_tokens \
             FROM admin_revenue_daily_user_model_rows WHERE day >= $1 AND day <= $2",
            vec![from.into(), to.into()],
        ))
        .await
        .map_err(|e| e.to_string())?;

    let mut models_by_day: std::collections::HashMap<String, Vec<RevenueModelRow>> =
        std::collections::HashMap::new();
    for row in model_rows {
        let day: String = row.try_get("", "day").map_err(|e| e.to_string())?;
        models_by_day.entry(day).or_default().push(RevenueModelRow {
            model: row.try_get("", "model").map_err(|e| e.to_string())?,
            charge_nano_usd: row
                .try_get("", "charge_nano_usd")
                .map_err(|e| e.to_string())?,
            calls: row.try_get("", "calls").map_err(|e| e.to_string())?,
            input_tokens: row.try_get("", "input_tokens").map_err(|e| e.to_string())?,
            output_tokens: row
                .try_get("", "output_tokens")
                .map_err(|e| e.to_string())?,
        });
    }
    let mut user_models_by_day: std::collections::HashMap<
        String,
        std::collections::HashMap<String, Vec<RevenueModelRow>>,
    > = std::collections::HashMap::new();
    for row in user_model_rows {
        let day: String = row.try_get("", "day").map_err(|e| e.to_string())?;
        let user_id: String = row.try_get("", "user_id").map_err(|e| e.to_string())?;
        user_models_by_day
            .entry(day)
            .or_default()
            .entry(user_id)
            .or_default()
            .push(RevenueModelRow {
                model: row.try_get("", "model").map_err(|e| e.to_string())?,
                charge_nano_usd: row
                    .try_get("", "charge_nano_usd")
                    .map_err(|e| e.to_string())?,
                calls: row.try_get("", "calls").map_err(|e| e.to_string())?,
                input_tokens: row.try_get("", "input_tokens").map_err(|e| e.to_string())?,
                output_tokens: row
                    .try_get("", "output_tokens")
                    .map_err(|e| e.to_string())?,
            });
    }
    let mut users_by_day: std::collections::HashMap<String, Vec<RevenueUserRow>> =
        std::collections::HashMap::new();
    for row in user_rows {
        let day: String = row.try_get("", "day").map_err(|e| e.to_string())?;
        users_by_day.entry(day).or_default().push(RevenueUserRow {
            user_id: row.try_get("", "user_id").map_err(|e| e.to_string())?,
            username: row.try_get("", "username").ok(),
            charge_nano_usd: row
                .try_get("", "charge_nano_usd")
                .map_err(|e| e.to_string())?,
            calls: row.try_get("", "calls").map_err(|e| e.to_string())?,
            input_tokens: row.try_get("", "input_tokens").map_err(|e| e.to_string())?,
            output_tokens: row
                .try_get("", "output_tokens")
                .map_err(|e| e.to_string())?,
            models: Vec::new(),
        });
    }

    let mut days = Vec::new();
    for summary in summaries {
        let day: String = summary.try_get("", "day").map_err(|e| e.to_string())?;
        let mut models = models_by_day.remove(&day).unwrap_or_default();
        sort_revenue_models(&mut models);
        let user_models = user_models_by_day.remove(&day).unwrap_or_default();
        let mut users = users_by_day.remove(&day).unwrap_or_default();
        for user in users.iter_mut() {
            if let Some(model_rows) = user_models.get(&user.user_id) {
                user.models = model_rows.clone();
                sort_revenue_models(&mut user.models);
            }
        }
        sort_revenue_users(&mut users);
        days.push(RevenueDayRow {
            day,
            total_charge_nano_usd: summary
                .try_get("", "total_charge_nano_usd")
                .map_err(|e| e.to_string())?,
            total_calls: summary
                .try_get("", "total_calls")
                .map_err(|e| e.to_string())?,
            total_input_tokens: summary
                .try_get("", "total_input_tokens")
                .map_err(|e| e.to_string())?,
            total_output_tokens: summary
                .try_get("", "total_output_tokens")
                .map_err(|e| e.to_string())?,
            models,
            users,
        });
    }
    Ok(days)
}

/// AR-8: after an exclusion-list change, recompute every persisted day that is
/// still inside the retention window.
pub async fn recompute_persisted_days(db: &DbPool, now: DateTime<Utc>) -> Result<(), String> {
    let cutoff_day = crate::beijing_time::beijing_day_shift(
        &beijing_day_id(now),
        -REVENUE_RECOMPUTE_LOOKBACK_DAYS,
    )
    .ok_or("day arithmetic failed")?;
    let days = db
        .read()
        .query_all(db.stmt(
            "SELECT day FROM admin_revenue_daily_summaries WHERE day >= $1 ORDER BY day ASC",
            vec![cutoff_day.into()],
        ))
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|row| row.try_get::<String>("", "day").ok())
        .collect::<Vec<_>>();
    for day in days {
        persist_revenue_day(db, &day).await?;
    }
    Ok(())
}

/// AR-6: after every Asia/Shanghai midnight, settle the just-elapsed day. The
/// first tick also runs immediately so a restart settles any missed days.
pub fn spawn_revenue_daily_settlement(
    db: DbPool,
    shutdown: std::sync::Arc<AtomicBool>,
    guard: crate::app::PrimaryDutyGuard,
) {
    tokio::spawn(async move {
        let mut first = true;
        loop {
            // 00:05 Asia/Shanghai, five minutes after midnight so that any
            // late-arriving request-log batch flush lands before settlement.
            let next_run = next_settlement_instant(Utc::now());
            let sleep = if first {
                StdDuration::from_secs(0)
            } else {
                (next_run - Utc::now())
                    .to_std()
                    .unwrap_or_else(|_| StdDuration::from_secs(1))
            };
            tokio::time::sleep(sleep).await;
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            if !guard.may_run().await {
                return;
            }
            match settle_elapsed_days(&db, Utc::now()).await {
                Ok(0) => {}
                Ok(count) => {
                    tracing::info!(count, "settled revenue daily summaries");
                }
                Err(error) => {
                    tracing::error!(error, "revenue daily settlement failed; retrying next tick");
                    // Retry sooner than the next midnight so a transient
                    // failure does not lose a day.
                    tokio::time::sleep(StdDuration::from_secs(300)).await;
                    if shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    if !guard.may_run().await {
                        return;
                    }
                    if let Err(retry_error) = settle_elapsed_days(&db, Utc::now()).await {
                        tracing::error!(error = %retry_error, "revenue daily settlement retry failed");
                    }
                }
            }
            first = false;
        }
    });
}

/// The next 00:05 Asia/Shanghai strictly after `now`.
fn next_settlement_instant(now: DateTime<Utc>) -> DateTime<Utc> {
    let local = now.with_timezone(&Shanghai);
    let today_run = local
        .date_naive()
        .and_hms_opt(0, 5, 0)
        .and_then(|time| Shanghai.from_local_datetime(&time).single())
        .expect("00:05 Asia/Shanghai is a valid local time");
    let next_local = if local < today_run {
        today_run
    } else {
        let tomorrow = local.date_naive().succ_opt().expect("date has a successor");
        tomorrow
            .and_hms_opt(0, 5, 0)
            .and_then(|time| Shanghai.from_local_datetime(&time).single())
            .expect("00:05 Asia/Shanghai is a valid local time")
    };
    next_local.with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_targets_five_past_beijing_midnight() {
        // 15:30 UTC is 23:30 Beijing: the same Beijing day's 00:05 has not
        // passed yet, so the target is that instant.
        let now = Utc.with_ymd_and_hms(2026, 9, 15, 15, 30, 0).unwrap();
        let next = next_settlement_instant(now);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 9, 15, 16, 5, 0).unwrap());

        // 16:10 UTC is 00:10 Beijing: today's run has passed, target tomorrow.
        let now = Utc.with_ymd_and_hms(2026, 9, 15, 16, 10, 0).unwrap();
        let next = next_settlement_instant(now);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 9, 16, 16, 5, 0).unwrap());
    }
}

//! Organization spaces (`orgs.spec.md`).
//!
//! An org is a `users` row with `is_org = 1` (the wallet) plus metadata and membership
//! rows. Creating one needs only the enterprise account class (admin-granted); the wallet
//! starts at zero and is funded by owner deposits. Org keys are owned by and bill the
//! org wallet; sharing only controls who may see the key material inside the space.

use crate::app::AppState;
use crate::dashboard_handlers::session_helpers::get_current_user;
use crate::dashboard_handlers::{AnalyticsQuery, RequestLogsQuery};
use crate::error::{AppError, AppResult};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, Statement, TransactionTrait, sea_query::Value as SeaValue};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// ORG-21: compile-time limits for this release.
pub const MAX_ORGS_PER_USER: i64 = 2;
pub const MAX_ORG_MEMBERS: i64 = 15;
/// ORG-2b: inline avatar image cap, a data:image/ URL of at most 300k characters.
const MAX_AVATAR_IMAGE_CHARS: usize = 300_000;

const INVITE_CODE_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// A 6-character display code; uniqueness is enforced by uq_orgs_invite_code.
fn generate_invite_code() -> String {
    let uuid = uuid::Uuid::new_v4();
    (0..6)
        .map(|i| {
            INVITE_CODE_ALPHABET[(uuid.as_bytes()[i] as usize) % INVITE_CODE_ALPHABET.len()] as char
        })
        .collect()
}

/// NULL keeps the compile-time default (ORG-21a); admins override per user/org.
fn limit_or_default(value: Option<i64>, default: i64) -> i64 {
    value.filter(|v| *v >= 0).unwrap_or(default)
}

/// ORG-8a: admins manage spaces but never inhabit them, and sales/agent
/// accounts are commercial identities outside the org model.
async fn membership_allowed(state: &AppState, user: &crate::users::User) -> AppResult<()> {
    if user.role.can_manage_users() {
        return Err(forbidden("admin accounts cannot join organizations"));
    }
    if user.parent_user_id.is_some() {
        return Err(forbidden("sub-accounts cannot join organizations"));
    }
    if user.account_class == crate::users::AccountClass::Agent {
        return Err(forbidden("agent accounts cannot join organizations"));
    }
    let sales = crate::store_billing::sales_store::SalesStore::new(state.db_pool.clone())
        .list_agents()
        .await
        .map_err(storage)?
        .iter()
        .any(|agent| agent.user_id == user.id);
    if sales {
        return Err(forbidden("sales agent accounts cannot join organizations"));
    }
    Ok(())
}

fn storage(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        error.to_string(),
    )
}

fn bad_request(message: &str) -> AppError {
    AppError::new(StatusCode::BAD_REQUEST, "invalid_request", message)
}

fn forbidden(message: &str) -> AppError {
    AppError::new(StatusCode::FORBIDDEN, "org_forbidden", message)
}

fn invite_invalid() -> AppError {
    AppError::new(
        StatusCode::NOT_FOUND,
        "invite_invalid",
        "invite link is invalid",
    )
}

fn parse_expiry(raw: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match raw {
        "24h" => Some(now + Duration::hours(24)),
        "3d" => Some(now + Duration::days(3)),
        "7d" => Some(now + Duration::days(7)),
        "30d" => Some(now + Duration::days(30)),
        "never" => None,
        _ => None,
    }
}

fn expiry_is_valid(raw: &str) -> bool {
    matches!(raw, "24h" | "3d" | "7d" | "30d" | "never")
}

fn valid_share_mode(mode: &str) -> bool {
    matches!(mode, "private" | "public" | "allow" | "deny")
}

fn validate_avatar_image(value: &Option<String>) -> Result<Option<String>, AppError> {
    match value {
        None => Ok(None),
        Some(image) => {
            if !image.starts_with("data:image/") {
                return Err(bad_request("avatar_image must be a data:image/ URL"));
            }
            if image.len() > MAX_AVATAR_IMAGE_CHARS {
                return Err(bad_request("avatar_image is too large"));
            }
            Ok(Some(image.clone()))
        }
    }
}

/// ORG-5: only an enterprise-class main account may create an org.
async fn creation_eligible(state: &AppState, user: &crate::users::User) -> AppResult<()> {
    if user.role.can_manage_users() {
        return Err(forbidden("admin accounts cannot create organizations"));
    }
    if user.parent_user_id.is_some() {
        return Err(forbidden("sub-accounts cannot create organizations"));
    }
    if user.account_class != crate::users::AccountClass::Enterprise {
        return Err(forbidden(
            "only enterprise accounts can create organizations",
        ));
    }
    let sales = crate::store_billing::sales_store::SalesStore::new(state.db_pool.clone())
        .list_agents()
        .await
        .map_err(storage)?
        .iter()
        .any(|agent| agent.user_id == user.id);
    if sales {
        return Err(forbidden(
            "sales agent accounts cannot create organizations",
        ));
    }
    Ok(())
}

/// Membership check; returns the role.
async fn member_role<C: ConnectionTrait>(
    tx: &C,
    backend: sea_orm::DbBackend,
    org_id: &str,
    user_id: &str,
) -> Result<Option<String>, AppError> {
    let row = tx
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2",
            [org_id.into(), user_id.into()],
        ))
        .await
        .map_err(storage)?;
    row.map(|row| row.try_get("", "role").map_err(storage))
        .transpose()
}

async fn org_member_count<C: ConnectionTrait>(
    tx: &C,
    backend: sea_orm::DbBackend,
    org_id: &str,
) -> Result<i64, AppError> {
    let row = tx
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS value FROM org_members WHERE org_id = $1",
            [org_id.into()],
        ))
        .await
        .map_err(storage)?;
    row.ok_or_else(|| storage("count returned no row"))?
        .try_get::<i64>("", "value")
        .map_err(storage)
}

async fn write_ledger_row<C: ConnectionTrait>(
    tx: &C,
    backend: sea_orm::DbBackend,
    entry_id: &str,
    user_id: &str,
    kind: &str,
    delta: i128,
    balance_after: i128,
    meta: serde_json::Value,
    now: &str,
) -> Result<(), AppError> {
    tx.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO billing_ledger
            (id, user_id, kind, delta_nano_usd, balance_after_nano_usd,
             meta_json, created_at, idempotency_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        [
            entry_id.into(),
            user_id.into(),
            kind.into(),
            delta.to_string().into(),
            balance_after.to_string().into(),
            meta.to_string().into(),
            now.into(),
            format!("ledger:{entry_id}").into(),
        ],
    ))
    .await
    .map_err(storage)?;
    Ok(())
}

/// One wallet-to-wallet move in a single transaction; both ledger sides written.
async fn move_wallet_balance(
    state: &AppState,
    from_user_id: &str,
    to_user_id: &str,
    amount_nano: i128,
    kind_out: &str,
    kind_in: &str,
    meta_extra: serde_json::Value,
) -> AppResult<(i128, i128)> {
    if amount_nano <= 0 {
        return Err(bad_request("amount must be positive"));
    }
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let lock_suffix = if state.db_pool.is_postgres() {
        " FOR UPDATE"
    } else {
        ""
    };

    let read_balance = |user_id: &str| {
        Statement::from_string(
            backend,
            format!("SELECT balance_nano_usd FROM users WHERE id = '{user_id}'{lock_suffix}"),
        )
    };
    // Locks are always taken from_user first, so concurrent distributions cannot deadlock.
    let from_row = tx
        .query_one(read_balance(from_user_id))
        .await
        .map_err(storage)?
        .ok_or_else(|| bad_request("source wallet not found"))?;
    let from_balance: i128 = from_row
        .try_get::<String>("", "balance_nano_usd")
        .map_err(storage)?
        .parse()
        .map_err(|_| bad_request("invalid persisted balance"))?;
    let to_row = tx
        .query_one(read_balance(to_user_id))
        .await
        .map_err(storage)?
        .ok_or_else(|| bad_request("target wallet not found"))?;
    let to_balance: i128 = to_row
        .try_get::<String>("", "balance_nano_usd")
        .map_err(storage)?
        .parse()
        .map_err(|_| bad_request("invalid persisted balance"))?;

    let from_after = from_balance.checked_sub(amount_nano).ok_or_else(|| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            "insufficient_balance",
            "wallet balance is insufficient",
        )
    })?;
    if from_after < 0 {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "insufficient_balance",
            "wallet balance is insufficient",
        ));
    }
    let to_after = to_balance
        .checked_add(amount_nano)
        .ok_or_else(|| bad_request("balance overflow"))?;
    let now = Utc::now().to_rfc3339();

    for (user_id, balance) in [(from_user_id, from_after), (to_user_id, to_after)] {
        tx.execute(Statement::from_sql_and_values(
            backend,
            "UPDATE users SET balance_nano_usd = $2, updated_at = $3 WHERE id = $1",
            [
                user_id.into(),
                balance.to_string().into(),
                now.clone().into(),
            ],
        ))
        .await
        .map_err(storage)?;
    }
    write_ledger_row(
        &tx,
        backend,
        &uuid::Uuid::new_v4().to_string(),
        from_user_id,
        kind_out,
        -amount_nano,
        from_after,
        json!({"to_user_id": to_user_id, "extra": meta_extra}),
        &now,
    )
    .await?;
    write_ledger_row(
        &tx,
        backend,
        &uuid::Uuid::new_v4().to_string(),
        to_user_id,
        kind_in,
        amount_nano,
        to_after,
        json!({"from_user_id": from_user_id, "extra": meta_extra}),
        &now,
    )
    .await?;
    tx.commit().await.map_err(storage)?;
    Ok((from_after, to_after))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateOrgRequest {
    pub display_name: String,
    #[serde(default)]
    pub avatar_emoji: Option<String>,
    #[serde(default)]
    pub avatar_color: Option<String>,
    #[serde(default)]
    pub avatar_image: Option<String>,
    pub invite_expiry: String,
}

#[derive(Debug, Serialize)]
pub struct OrgSummary {
    pub id: String,
    pub display_name: String,
    pub avatar_emoji: String,
    pub avatar_color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_image: Option<String>,
    pub role: String,
    pub member_count: i64,
    pub balance_nano_usd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invite_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
    pub max_members: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invite_expires_at: Option<String>,
}

pub async fn create_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateOrgRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    creation_eligible(&state, &user).await?;

    let name = body.display_name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(bad_request("display_name must be 1..64 characters"));
    }
    if !expiry_is_valid(&body.invite_expiry) {
        return Err(bad_request(
            "invite_expiry must be one of 24h, 3d, 7d, 30d, never",
        ));
    }
    let avatar_emoji = body.avatar_emoji.unwrap_or_else(|| "🏢".to_string());
    if avatar_emoji.is_empty() || avatar_emoji.len() > 8 {
        return Err(bad_request("avatar_emoji must be 1..8 bytes"));
    }
    let avatar_color = body.avatar_color.unwrap_or_else(|| "#6366f1".to_string());
    if !avatar_color.starts_with('#') || avatar_color.len() != 7 {
        return Err(bad_request("avatar_color must be #rrggbb"));
    }
    let avatar_image = validate_avatar_image(&body.avatar_image)?;

    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let owned_row = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT (SELECT COUNT(*) FROM orgs WHERE owner_user_id = $1) AS value,
                org_creation_limit FROM users WHERE id = $1",
            [user.id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| storage("creator row missing"))?;
    let owned: i64 = owned_row.try_get("", "value").map_err(storage)?;
    let creation_limit = limit_or_default(
        owned_row
            .try_get::<Option<i64>>("", "org_creation_limit")
            .map_err(storage)?,
        MAX_ORGS_PER_USER,
    );
    if owned >= creation_limit {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "org_limit_reached",
            "org creation limit reached",
        ));
    }

    let org_id = uuid::Uuid::new_v4().to_string();
    let invite_token = format!(
        "{}{}",
        crate::store_billing::sales::generate_code(),
        crate::store_billing::sales::generate_code()
    )
    .to_lowercase();
    let invite_code = generate_invite_code();
    let invite_expires_at = parse_expiry(&body.invite_expiry, Utc::now());
    let now = Utc::now().to_rfc3339();

    // The wallet row is born with the deposit; the creator side is deducted in the same
    // transaction, so a failure leaves neither row.
    // Resolve the group before begin_write: the registry lookup uses the read pool, which
    // on single-connection SQLite would deadlock behind our own write transaction.
    let read = state.db_pool.read();
    let group = read
        .query_one(Statement::from_string(
            backend,
            "SELECT id, account_class FROM monoize_groups WHERE account_class = 'enterprise' AND is_public = 1
             ORDER BY sort_order ASC, created_at ASC LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(storage)?;
    // `read` borrows the read pool; the query above has already awaited and returned an owned
    // row, so no explicit drop is needed before taking the write lock.
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let (group_id, org_class) = match group {
        Some(row) => (
            row.try_get::<String>("", "id").map_err(storage)?,
            crate::users::AccountClass::Enterprise,
        ),
        None => (
            state.user_store.default_group_id().await.map_err(storage)?,
            crate::users::AccountClass::Standard,
        ),
    };

    tx.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO users (id, username, password_hash, role, created_at, updated_at, enabled,
                            balance_nano_usd, balance_unlimited, group_id, account_class, is_org)
         VALUES ($1, $1, '', 'user', $2, $2, 1, '0', 0, $3, $4, 1)",
        [
            org_id.clone().into(),
            now.clone().into(),
            group_id.into(),
            org_class.as_str().into(),
        ],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO orgs (id, owner_user_id, display_name, avatar_emoji, avatar_color, avatar_image,
                           invite_token, invite_code, invite_expires_at, invite_created_at, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $10)",
        [
            org_id.clone().into(),
            user.id.clone().into(),
            name.into(),
            avatar_emoji.clone().into(),
            avatar_color.clone().into(),
            avatar_image.clone().into(),
            invite_token.clone().into(),
            invite_code.clone().into(),
            invite_expires_at.map(|t| t.to_rfc3339()).into(),
            now.clone().into(),
        ],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO org_members (org_id, user_id, role, joined_at) VALUES ($1, $2, 'owner', $3)",
        [
            org_id.clone().into(),
            user.id.clone().into(),
            now.clone().into(),
        ],
    ))
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)?;

    Ok((
        StatusCode::CREATED,
        Json(OrgSummary {
            id: org_id,
            display_name: name.to_string(),
            avatar_emoji,
            avatar_color,
            avatar_image,
            role: "owner".to_string(),
            member_count: 1,
            max_members: MAX_ORG_MEMBERS,
            balance_nano_usd: "0".to_string(),
            invite_token: Some(invite_token.clone()),
            invite_code: Some(invite_code),
            invite_expires_at: invite_expires_at.map(|t| t.to_rfc3339()),
        }),
    ))
}

pub async fn list_my_orgs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let rows = state
        .db_pool
        .read()
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT o.id, o.display_name, o.avatar_emoji, o.avatar_color, o.avatar_image,
                    o.invite_token, o.invite_code, o.invite_expires_at, o.max_members, m.role AS my_role,
                    (SELECT COUNT(*) FROM org_members mm WHERE mm.org_id = o.id) AS member_count,
                    u.balance_nano_usd
             FROM orgs o
             JOIN org_members m ON m.org_id = o.id AND m.user_id = $1
             JOIN users u ON u.id = o.id
             ORDER BY o.created_at ASC",
            [user.id.clone().into()],
        ))
        .await
        .map_err(storage)?;
    let orgs = rows
        .iter()
        .map(|row| {
            let owner = row
                .try_get::<String>("", "my_role")
                .map(|role| role == "owner")
                .unwrap_or(false);
            Ok(OrgSummary {
                id: row.try_get("", "id").map_err(storage)?,
                display_name: row.try_get("", "display_name").map_err(storage)?,
                avatar_emoji: row.try_get("", "avatar_emoji").map_err(storage)?,
                avatar_color: row.try_get("", "avatar_color").map_err(storage)?,
                avatar_image: row
                    .try_get::<Option<String>>("", "avatar_image")
                    .map_err(storage)?,
                role: row.try_get("", "my_role").map_err(storage)?,
                member_count: row.try_get("", "member_count").map_err(storage)?,
                balance_nano_usd: row.try_get("", "balance_nano_usd").map_err(storage)?,
                invite_token: owner
                    .then(|| row.try_get("", "invite_token").map_err(storage))
                    .transpose()?,
                invite_code: owner
                    .then(|| {
                        row.try_get::<Option<String>>("", "invite_code")
                            .map_err(storage)
                    })
                    .transpose()?
                    .flatten(),
                max_members: limit_or_default(
                    row.try_get::<Option<i64>>("", "max_members")
                        .map_err(storage)?,
                    MAX_ORG_MEMBERS,
                ),
                invite_expires_at: owner
                    .then(|| {
                        row.try_get::<Option<String>>("", "invite_expires_at")
                            .map_err(storage)
                    })
                    .transpose()?
                    .flatten(),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let owned: i64 = orgs.iter().filter(|org| org.role == "owner").count() as i64;
    let creation_limit = limit_or_default(
        state
            .db_pool
            .read()
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT org_creation_limit FROM users WHERE id = $1",
                [user.id.clone().into()],
            ))
            .await
            .map_err(storage)?
            .and_then(|row| row.try_get::<Option<i64>>("", "org_creation_limit").ok())
            .flatten(),
        MAX_ORGS_PER_USER,
    );
    // ORG-21a: the quota hint drives the "+" affordance; eligibility still gates
    // the actual create call.
    let can_create = creation_eligible(&state, &user).await.is_ok() && owned < creation_limit;
    Ok(Json(json!({
        "orgs": orgs,
        "creation_limit": creation_limit,
        "creation_used": owned,
        "can_create": can_create,
    })))
}

#[derive(Debug, Serialize)]
pub struct OrgMemberView {
    pub user_id: String,
    pub username: String,
    pub role: String,
    pub joined_at: String,
    pub alias: Option<String>,
}

pub async fn org_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let role = member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let org = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT display_name, avatar_emoji, avatar_color, avatar_image, invite_token,
                    invite_code, invite_expires_at, max_members, owner_user_id FROM orgs WHERE id = $1",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let members = read
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT m.user_id, m.role, m.joined_at, m.alias, u.username
             FROM org_members m JOIN users u ON u.id = m.user_id
             WHERE m.org_id = $1 ORDER BY m.joined_at ASC",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?;
    let wallet = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT balance_nano_usd FROM users WHERE id = $1",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| storage("org wallet row missing"))?;
    let balance: String = wallet.try_get("", "balance_nano_usd").map_err(storage)?;
    // ORG-14a: the key picker is fed from the wallet, never the acting member. The
    // base set mirrors the non-admin accessibility rule (public or granted to the
    // wallet); the picker convention additionally keeps user_selectable rows and
    // the wallet's own group.
    let wallet_groups = read
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT g.id, g.name, g.description, g.is_default, g.user_selectable, g.is_public,
                    g.account_class, g.sort_order, g.created_at, g.updated_at
             FROM monoize_groups g
             JOIN users u ON u.id = $1 AND u.account_class = g.account_class
             WHERE (g.is_public = 1 OR g.id = u.group_id
                    OR EXISTS (SELECT 1 FROM user_group_grants ug
                               WHERE ug.user_id = u.id AND ug.group_id = g.id))
               AND (g.user_selectable = 1 OR g.id = u.group_id)
             ORDER BY g.sort_order ASC, g.created_at ASC, g.id ASC",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .iter()
        .map(crate::users::row_to_group)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| storage(format!("{error:?}")))?;

    let avatar_image: Option<String> = org.try_get("", "avatar_image").map_err(storage)?;
    let is_owner = role == "owner";
    Ok(Json(json!({
        "id": org_id,
        "display_name": org.try_get::<String>("", "display_name").map_err(storage)?,
        "avatar_emoji": org.try_get::<String>("", "avatar_emoji").map_err(storage)?,
        "avatar_color": org.try_get::<String>("", "avatar_color").map_err(storage)?,
        "avatar_image": avatar_image,
        "my_role": role,
        "balance_nano_usd": balance,
        "wallet_groups": wallet_groups,
        "members": members
            .iter()
            .map(|row| OrgMemberView {
                user_id: row.try_get("", "user_id").unwrap_or_default(),
                username: row.try_get("", "username").unwrap_or_default(),
                role: row.try_get("", "role").unwrap_or_default(),
                joined_at: row.try_get("", "joined_at").unwrap_or_default(),
                alias: row.try_get("", "alias").unwrap_or_default(),
            })
            .collect::<Vec<_>>(),
        "max_members": limit_or_default(
            org.try_get::<Option<i64>>("", "max_members").map_err(storage)?,
            MAX_ORG_MEMBERS,
        ),
        "invite": if is_owner {
            json!({
                "token": org.try_get::<String>("", "invite_token").map_err(storage)?,
                "code": org
                    .try_get::<Option<String>>("", "invite_code")
                    .map_err(storage)?
                    .unwrap_or_default(),
                "expires_at": org
                    .try_get::<Option<String>>("", "invite_expires_at")
                    .map_err(storage)?
                    .unwrap_or_default(),
            })
        } else {
            serde_json::Value::Null
        },
    })))
}

/// ORG-9: the landing page reads org info without marking anything.
pub async fn invite_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> AppResult<impl IntoResponse> {
    get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let row = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT o.id, o.display_name, o.avatar_emoji, o.avatar_color, o.avatar_image,
                    o.invite_expires_at, o.max_members, u.username AS owner_username,
                    (SELECT COUNT(*) FROM org_members m WHERE m.org_id = o.id) AS member_count
             FROM orgs o JOIN users u ON u.id = o.owner_user_id
             WHERE o.invite_token = $1 OR o.invite_code = $1",
            [token.into()],
        ))
        .await
        .map_err(storage)?;
    let Some(row) = row else {
        return Err(invite_invalid());
    };
    let expires_at: Option<String> = row.try_get("", "invite_expires_at").map_err(storage)?;
    if let Some(expires) = expires_at.as_deref()
        && DateTime::parse_from_rfc3339(expires)
            .map(|parsed| parsed.with_timezone(&Utc) <= Utc::now())
            .unwrap_or(true)
    {
        return Err(invite_invalid());
    }
    // A full org still previews (with is_full) so the landing page can explain
    // itself; only accepting is blocked.
    let member_count: i64 = row.try_get("", "member_count").map_err(storage)?;
    let max_members = limit_or_default(
        row.try_get::<Option<i64>>("", "max_members")
            .map_err(storage)?,
        MAX_ORG_MEMBERS,
    );
    Ok(Json(json!({
        "org_id": row.try_get::<String>("", "id").map_err(storage)?,
        "display_name": row.try_get::<String>("", "display_name").map_err(storage)?,
        "avatar_emoji": row.try_get::<String>("", "avatar_emoji").map_err(storage)?,
        "avatar_color": row.try_get::<String>("", "avatar_color").map_err(storage)?,
        "avatar_image": row.try_get::<Option<String>>("", "avatar_image").map_err(storage)?,
        "owner_username": row.try_get::<String>("", "owner_username").map_err(storage)?,
        "member_count": member_count,
        "max_members": max_members,
        "is_full": member_count >= max_members,
    })))
}

/// ORG-10: accept an invite.
pub async fn join_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JoinOrgRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    membership_allowed(&state, &user).await?;
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let org = tx
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT id, invite_expires_at, max_members FROM orgs
             WHERE invite_token = $1 OR invite_code = $1",
            [body.token.into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(invite_invalid)?;
    let org_id: String = org.try_get("", "id").map_err(storage)?;
    let expires_at: Option<String> = org.try_get("", "invite_expires_at").map_err(storage)?;
    if let Some(expires) = expires_at.as_deref()
        && DateTime::parse_from_rfc3339(expires)
            .map(|parsed| parsed.with_timezone(&Utc) <= Utc::now())
            .unwrap_or(true)
    {
        return Err(invite_invalid());
    }
    if member_role(&tx, backend, &org_id, &user.id)
        .await?
        .is_some()
    {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "org_already_member",
            "already a member of this organization",
        ));
    }
    let max_members = limit_or_default(
        org.try_get::<Option<i64>>("", "max_members")
            .map_err(storage)?,
        MAX_ORG_MEMBERS,
    );
    if org_member_count(&tx, backend, &org_id).await? >= max_members {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "org_member_limit_reached",
            "organization member limit reached",
        ));
    }
    tx.execute(Statement::from_sql_and_values(
        backend,
        "INSERT INTO org_members (org_id, user_id, role, joined_at) VALUES ($1, $2, 'member', $3)",
        [
            org_id.clone().into(),
            user.id.into(),
            Utc::now().to_rfc3339().into(),
        ],
    ))
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(Json(json!({ "org_id": org_id })))
}

#[derive(Debug, Deserialize)]
pub struct JoinOrgRequest {
    pub token: String,
}

/// ORG-11: regenerate the single invite link (owner only).
pub async fn regenerate_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(body): Json<RegenerateInviteRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    if !expiry_is_valid(&body.invite_expiry) {
        return Err(bad_request(
            "invite_expiry must be one of 24h, 3d, 7d, 30d, never",
        ));
    }
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let role = member_role(&tx, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    if role != "owner" {
        return Err(forbidden("only the owner can regenerate the invite link"));
    }
    let token = format!(
        "{}{}",
        crate::store_billing::sales::generate_code(),
        crate::store_billing::sales::generate_code()
    )
    .to_lowercase();
    let code = generate_invite_code();
    let expires_at = parse_expiry(&body.invite_expiry, Utc::now());
    tx.execute(Statement::from_sql_and_values(
        backend,
        "UPDATE orgs SET invite_token = $2, invite_code = $3, invite_expires_at = $4,
                invite_created_at = $5, updated_at = $5 WHERE id = $1",
        [
            org_id.into(),
            token.clone().into(),
            code.clone().into(),
            expires_at.map(|t| t.to_rfc3339()).into(),
            Utc::now().to_rfc3339().into(),
        ],
    ))
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(Json(json!({
        "token": token,
        "code": code,
        "expires_at": expires_at.map(|t| t.to_rfc3339()),
    })))
}

#[derive(Debug, Deserialize)]
pub struct RegenerateInviteRequest {
    pub invite_expiry: String,
}

#[derive(Debug, Deserialize)]
pub struct WalletMoveRequest {
    pub amount_nano_usd: String,
}

fn parse_amount(raw: &str) -> AppResult<i128> {
    let amount = raw
        .trim()
        .parse::<i128>()
        .map_err(|_| bad_request("amount_nano_usd must be an integer string"))?;
    if amount <= 0 {
        return Err(bad_request("amount must be positive"));
    }
    Ok(amount)
}

/// ORG-12: owner tops up the org wallet from their personal wallet.
pub async fn deposit_to_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(body): Json<WalletMoveRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let amount = parse_amount(&body.amount_nano_usd)?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    // ORG-12: any member may fund the wallet from their personal balance.
    member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let (personal_after, org_after) = move_wallet_balance(
        &state,
        &user.id,
        &org_id,
        amount,
        "org_deposit",
        "org_deposit_receive",
        json!({"org_id": org_id}),
    )
    .await?;
    Ok(Json(json!({
        "personal_balance_nano_usd": personal_after.to_string(),
        "org_balance_nano_usd": org_after.to_string(),
    })))
}

/// ORG-13: owner distributes org wallet funds to a member's personal wallet.
pub async fn distribute_from_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(body): Json<DistributeRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let amount = parse_amount(&body.amount_nano_usd)?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let role = member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    if role != "owner" {
        return Err(forbidden(
            "only the owner can distribute the organization wallet",
        ));
    }
    let member_role_value = member_role(&*read, backend, &org_id, &body.member_user_id)
        .await?
        .ok_or_else(|| bad_request("member_user_id is not a member of this organization"))?;
    let _ = member_role_value;
    let (org_after, member_after) = move_wallet_balance(
        &state,
        &org_id,
        &body.member_user_id,
        amount,
        "org_grant",
        "org_receive",
        json!({"org_id": org_id}),
    )
    .await?;
    Ok(Json(json!({
        "org_balance_nano_usd": org_after.to_string(),
        "member_balance_nano_usd": member_after.to_string(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct DistributeRequest {
    pub member_user_id: String,
    pub amount_nano_usd: String,
}

/// ORG-14: the key is owned by and bills the org wallet; sharing only exposes the material.
pub async fn create_org_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(body): Json<CreateOrgKeyRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let role = member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(bad_request("name must be 1..64 characters"));
    }
    // The owner shares with everyone by default; a member keeps keys private by default.
    let effective_mode = match body.share_mode.as_deref() {
        None => if role == "owner" { "public" } else { "private" }.to_string(),
        Some(mode) if valid_share_mode(mode) => mode.to_string(),
        Some(other) => {
            return Err(bad_request(&format!(
                "share_mode must be private, public, allow, or deny: {other}"
            )));
        }
    };

    let input = crate::users::CreateApiKeyInput {
        name: name.to_string(),
        expires_in_days: body.expires_in_days,
        sub_account_enabled: false,
        sub_account_balance_nano_usd: None,
        model_limits_enabled: body
            .model_limits_enabled
            .unwrap_or(!body.model_limits.is_empty()),
        model_limits: body.model_limits.clone(),
        ip_whitelist: body.ip_whitelist.clone(),
        group_ids: body.group_ids.clone(),
        channel_bindings: Vec::new(),
        model_bindings: Vec::new(),
        max_multiplier: None,
        transforms: Vec::new(),
        model_redirects: Vec::new(),
        reasoning_envelope_enabled: true,
        request_capture_mode: crate::users::RequestCaptureMode::Off,
        spend_limit_total_nano_usd: None,
        spend_limit_hourly_nano_usd: None,
        spend_limit_daily_nano_usd: None,
    };
    let (api_key, plaintext) = state
        .user_store
        .create_api_key_extended(&org_id, input, false)
        .await
        .map_err(|e| bad_request(&e))?;
    state
        .db_pool
        .write()
        .await
        .execute(Statement::from_sql_and_values(
            backend,
            "UPDATE api_keys SET org_id = $2, org_share_mode = $3, created_by = $4 WHERE id = $1",
            [
                api_key.id.clone().into(),
                org_id.into(),
                effective_mode.clone().into(),
                user.id.clone().into(),
            ],
        ))
        .await
        .map_err(storage)?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "id": api_key.id, "name": api_key.name, "key": plaintext,
                     "share_mode": effective_mode, "owner_username": user.username,
                     "model_limits_enabled": !body.model_limits.is_empty() }),
        ),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CreateOrgKeyRequest {
    pub name: String,
    #[serde(default)]
    pub share_mode: Option<String>,
    #[serde(default)]
    pub model_limits_enabled: Option<bool>,
    #[serde(default)]
    pub model_limits: Vec<String>,
    #[serde(default)]
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub expires_in_days: Option<i64>,
    #[serde(default)]
    pub ip_whitelist: Vec<String>,
}

/// ORG-15: my keys plus keys shared to me (with material for copying).
pub async fn list_org_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let caller_role = member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;

    let mine = read
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT k.id, k.name, k.key, k.key_prefix, k.org_share_mode, k.model_limits_enabled,
                    k.model_limits, k.group_ids, k.created_at, u.username AS owner_username,
                    om.alias AS owner_alias
             FROM api_keys k
             LEFT JOIN users u ON u.id = k.created_by
             LEFT JOIN org_members om ON om.org_id = k.org_id AND om.user_id = k.created_by
             WHERE k.org_id = $1 AND k.created_by = $2 ORDER BY k.created_at DESC",
            [org_id.clone().into(), user.id.clone().into()],
        ))
        .await
        .map_err(storage)?;
    // ORG-15: members see keys shared with them; the owner sees EVERY other key of
    // the space regardless of share mode — the keys bill the org wallet the owner
    // funds, and ORG-17 keeps keys of removed members alive and invisible to
    // members, so the owner's view is the only surface that can manage them.
    let shared_sql = if caller_role == "owner" {
        "SELECT k.id, k.name, k.key, k.key_prefix, k.org_share_mode, k.model_limits_enabled,
                k.model_limits, k.group_ids, u.username AS owner_username, k.created_by,
                om.alias AS owner_alias
         FROM api_keys k
         LEFT JOIN users u ON u.id = k.created_by
         LEFT JOIN org_members om ON om.org_id = k.org_id AND om.user_id = k.created_by
         WHERE k.org_id = $1 AND k.created_by != $2
           AND (k.created_by IS NULL OR k.created_by IN
                (SELECT user_id FROM org_members WHERE org_id = $1))
         ORDER BY k.created_at DESC"
    } else {
        "SELECT k.id, k.name, k.key, k.key_prefix, k.org_share_mode, k.model_limits_enabled,
                k.model_limits, k.group_ids, u.username AS owner_username, k.created_by,
                om.alias AS owner_alias
         FROM api_keys k
         LEFT JOIN users u ON u.id = k.created_by
         LEFT JOIN org_members om ON om.org_id = k.org_id AND om.user_id = k.created_by
         WHERE k.org_id = $1 AND k.created_by != $2 AND (
                k.org_share_mode = 'public'
                OR (k.org_share_mode = 'allow'
                    AND EXISTS (SELECT 1 FROM org_key_shares s
                                WHERE s.api_key_id = k.id AND s.member_user_id = $2))
                OR (k.org_share_mode = 'deny'
                    AND NOT EXISTS (SELECT 1 FROM org_key_shares s
                                    WHERE s.api_key_id = k.id AND s.member_user_id = $2)))
         ORDER BY k.created_at DESC"
    };
    let shared = read
        .query_all(Statement::from_sql_and_values(
            backend,
            shared_sql,
            [org_id.clone().into(), user.id.clone().into()],
        ))
        .await
        .map_err(storage)?;

    // ORG-15: the caller's own allow/deny keys carry their current share rows so the
    // sharing editor can present the saved selection.
    let mut shared_by_key: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    if !mine.is_empty() {
        let key_ids: Vec<SeaValue> = mine
            .iter()
            .map(|row| row.try_get::<String>("", "id").unwrap_or_default().into())
            .collect();
        let placeholders = (1..=key_ids.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let share_rows = read
            .query_all(Statement::from_sql_and_values(
                backend,
                format!("SELECT api_key_id, member_user_id FROM org_key_shares WHERE api_key_id IN ({placeholders})"),
                key_ids,
            ))
            .await
            .map_err(storage)?;
        for row in share_rows {
            let key_id = row.try_get::<String>("", "api_key_id").map_err(storage)?;
            let member_id = row
                .try_get::<String>("", "member_user_id")
                .map_err(storage)?;
            shared_by_key.entry(key_id).or_default().push(member_id);
        }
    }

    Ok(Json(json!({
        "mine": mine
            .iter()
            .map(|row| json!({
                "id": row.try_get::<String>("", "id").unwrap_or_default(),
                "name": row.try_get::<String>("", "name").unwrap_or_default(),
                // ORG-15b: a member's own key material stays readable in the space.
                "key": row.try_get::<String>("", "key").unwrap_or_default(),
                "key_prefix": row.try_get::<String>("", "key_prefix").unwrap_or_default(),
                "share_mode": row.try_get::<Option<String>>("", "org_share_mode").unwrap_or_default(),
                "shared_with": shared_by_key
                    .get(&row.try_get::<String>("", "id").unwrap_or_default())
                    .cloned()
                    .unwrap_or_default(),
                "model_limits_enabled": row.try_get::<i32>("", "model_limits_enabled").unwrap_or(0) == 1,
                "model_limits": serde_json::from_str::<Vec<String>>(
                    &row.try_get::<String>("", "model_limits").unwrap_or_else(|_| "[]".to_string()),
                ).unwrap_or_default(),
                "group_ids": serde_json::from_str::<Vec<String>>(
                    &row.try_get::<String>("", "group_ids").unwrap_or_else(|_| "[]".to_string()),
                ).unwrap_or_default(),
                "created_at": row.try_get::<String>("", "created_at").unwrap_or_default(),
                // SA-SCOPE6: label the account so a reader never compares a sub-account
                // balance against a wallet balance.
                "account_scope": row
                    .try_get::<String>("", "account_scope")
                    .unwrap_or_else(|_| "user".to_string()),
            }))
            .collect::<Vec<_>>(),
        "shared": shared
            .iter()
            .map(|row| json!({
                "id": row.try_get::<String>("", "id").unwrap_or_default(),
                "name": row.try_get::<String>("", "name").unwrap_or_default(),
                "key": row.try_get::<String>("", "key").unwrap_or_default(),
                "key_prefix": row.try_get::<String>("", "key_prefix").unwrap_or_default(),
                "share_mode": row.try_get::<Option<String>>("", "org_share_mode").unwrap_or_default(),
                "model_limits_enabled": row.try_get::<i32>("", "model_limits_enabled").unwrap_or(0) == 1,
                "model_limits": serde_json::from_str::<Vec<String>>(
                    &row.try_get::<String>("", "model_limits").unwrap_or_else(|_| "[]".to_string()),
                ).unwrap_or_default(),
                "group_ids": serde_json::from_str::<Vec<String>>(
                    &row.try_get::<String>("", "group_ids").unwrap_or_else(|_| "[]".to_string()),
                ).unwrap_or_default(),
                "owner_username": row.try_get::<String>("", "owner_username").unwrap_or_default(),
                "owner_alias": row.try_get::<Option<String>>("", "owner_alias").unwrap_or_default(),
                "created_by": row.try_get::<Option<String>>("", "created_by").unwrap_or_default(),
            }))
            .collect::<Vec<_>>(),
    })))
}

/// ORG-16: the key owner chooses who may see the key inside the space.
pub async fn update_key_sharing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, key_id)): Path<(String, String)>,
    Json(body): Json<UpdateSharingRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    member_role(&tx, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let key = tx
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT id FROM api_keys WHERE id = $1 AND created_by = $2 AND org_id = $3",
            [
                key_id.clone().into(),
                user.id.clone().into(),
                org_id.clone().into(),
            ],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "key not found"))?;
    let _ = key;

    let mode = match body.mode.as_str() {
        "private" => None,
        "public" | "allow" | "deny" => Some(body.mode.clone()),
        other => {
            return Err(bad_request(&format!(
                "mode must be private, public, allow, or deny: {other}"
            )));
        }
    };
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM org_key_shares WHERE api_key_id = $1",
        [key_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    if mode.as_deref() == Some("allow") || mode.as_deref() == Some("deny") {
        for member_id in &body.member_ids {
            if member_role(&tx, backend, &org_id, member_id)
                .await?
                .is_none()
            {
                return Err(bad_request("member_ids contains a non-member"));
            }
            tx.execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO org_key_shares (api_key_id, member_user_id, created_at)
                 VALUES ($1, $2, $3)",
                [
                    key_id.clone().into(),
                    member_id.clone().into(),
                    Utc::now().to_rfc3339().into(),
                ],
            ))
            .await
            .map_err(storage)?;
        }
    }
    tx.execute(Statement::from_sql_and_values(
        backend,
        "UPDATE api_keys SET org_share_mode = $2 WHERE id = $1",
        [key_id.clone().into(), mode.into()],
    ))
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(Json(json!({ "success": true, "mode": body.mode })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSharingRequest {
    pub mode: String,
    #[serde(default)]
    pub member_ids: Vec<String>,
}

/// ORG-22: the org wallet ledger, newest first.
pub async fn org_ledger(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let rows = read
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT id, kind, delta_nano_usd, balance_after_nano_usd, meta_json, created_at, account_scope
             FROM billing_ledger WHERE user_id = $1
             ORDER BY created_at DESC, id DESC LIMIT 200",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?;
    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let meta: serde_json::Value = row
                .try_get::<Option<String>>("", "meta_json")
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or(serde_json::Value::Null);
            json!({
                "id": row.try_get::<String>("", "id").unwrap_or_default(),
                "kind": row.try_get::<String>("", "kind").unwrap_or_default(),
                "delta_nano_usd": row.try_get::<String>("", "delta_nano_usd").unwrap_or_default(),
                "balance_after_nano_usd": row
                    .try_get::<String>("", "balance_after_nano_usd")
                    .unwrap_or_default(),
                "created_at": row.try_get::<String>("", "created_at").unwrap_or_default(),
                "meta": meta,
            })
        })
        .collect();
    // ORG-22a: resolve the from/to actor ids the move rows carry so the UI can
    // say who recharged and who received a distribution, even for ex-members.
    let mut actor_ids: Vec<String> = Vec::new();
    for entry in &entries {
        for key in ["from_user_id", "to_user_id"] {
            if let Some(id) = entry["meta"][key].as_str() {
                if !actor_ids.iter().any(|existing| existing == id) {
                    actor_ids.push(id.to_string());
                }
            }
        }
    }
    let mut actors = serde_json::Map::new();
    if !actor_ids.is_empty() {
        let placeholders = actor_ids
            .iter()
            .enumerate()
            .map(|(index, _)| format!("${}", index + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT u.id, u.username, om.alias
             FROM users u
             LEFT JOIN org_members om ON om.org_id = $1 AND om.user_id = u.id
             WHERE u.id IN ({placeholders})"
        );
        let params: Vec<sea_orm::Value> = std::iter::once(org_id.into())
            .chain(actor_ids.iter().map(|id| id.clone().into()))
            .collect();
        if let Ok(actor_rows) = read
            .query_all(Statement::from_sql_and_values(backend, &sql, params))
            .await
        {
            for row in &actor_rows {
                actors.insert(
                    row.try_get::<String>("", "id").unwrap_or_default(),
                    json!({
                        "username": row.try_get::<Option<String>>("", "username").ok().flatten(),
                        "alias": row.try_get::<Option<String>>("", "alias").ok().flatten(),
                    }),
                );
            }
        }
    }
    Ok(Json(json!({ "entries": entries, "actors": actors })))
}

/// ORG-17: removal deletes membership and share rows. Org keys stay owned by the org
/// (ORG-4), so nothing is migrated to the removed member and org analytics are unchanged.
#[derive(Debug, Default, serde::Deserialize)]
pub struct RemoveOrgMemberRequest {
    /// ORG-17c: delete the org keys the member created along with the membership.
    /// Defaults to true so a removed member leaves nothing usable behind; the
    /// historical request logs keep their org attribution either way.
    #[serde(default = "default_delete_keys")]
    pub delete_keys: bool,
}

fn default_delete_keys() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct SetOrgMemberAliasRequest {
    pub alias: Option<String>,
}

/// ORG-3a: sets or clears a member's display alias. The owner may set any
/// member's alias; a member may set only their own.
pub async fn set_org_member_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, member_id)): Path<(String, String)>,
    Json(body): Json<SetOrgMemberAliasRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let caller_role = member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    if caller_role != "owner" && member_id != user.id {
        return Err(forbidden("only the owner can set another member's alias"));
    }
    let member = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT m.alias, u.username FROM org_members m JOIN users u ON u.id = m.user_id
             WHERE m.org_id = $1 AND m.user_id = $2",
            [org_id.clone().into(), member_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "member not found"))?;
    let username: String = member.try_get("", "username").map_err(storage)?;
    let alias = body
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.chars().count() > 32 {
                Err(bad_request("alias must be 1..32 characters"))
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()?;
    state
        .db_pool
        .write()
        .await
        .execute(Statement::from_sql_and_values(
            backend,
            "UPDATE org_members SET alias = $3 WHERE org_id = $1 AND user_id = $2",
            [org_id.clone().into(), member_id.clone().into(), alias.clone().into()],
        ))
        .await
        .map_err(storage)?;
    Ok(Json(json!({
        "user_id": member_id,
        "username": username,
        "alias": alias,
    })))
}

pub async fn remove_org_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, member_id)): Path<(String, String)>,
    body: Option<Json<RemoveOrgMemberRequest>>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let delete_keys = body.map(|Json(body)| body.delete_keys).unwrap_or(true);
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let role = member_role(&tx, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    if role != "owner" {
        return Err(forbidden("only the owner can remove members"));
    }
    if member_id == user.id {
        return Err(bad_request("the owner cannot be removed"));
    }
    let target_role = member_role(&tx, backend, &org_id, &member_id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "member not found"))?;
    if target_role == "owner" {
        return Err(bad_request("the owner cannot be removed"));
    }
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM org_members WHERE org_id = $1 AND user_id = $2",
        [org_id.clone().into(), member_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM org_key_shares WHERE member_user_id = $1 AND api_key_id IN
            (SELECT id FROM api_keys WHERE org_id = $2)",
        [member_id.clone().into(), org_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    let mut deleted_keys = 0i64;
    if delete_keys {
        let removed = tx
            .execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM api_keys WHERE org_id = $1 AND created_by = $2",
                [org_id.clone().into(), member_id.clone().into()],
            ))
            .await
            .map_err(storage)?;
        deleted_keys = removed.rows_affected() as i64;
    }
    tx.commit().await.map_err(storage)?;
    Ok(Json(
        json!({ "success": true, "deleted_keys": deleted_keys }),
    ))
}

/// ORG-17a: a non-owner member leaves the space. Same semantics as ORG-17 removal
/// of the caller: membership and share rows go, org keys stay with the org.
pub async fn leave_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let role = member_role(&tx, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    if role == "owner" {
        return Err(bad_request(
            "the owner cannot leave; delete the organization instead",
        ));
    }
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM org_members WHERE org_id = $1 AND user_id = $2",
        [org_id.clone().into(), user.id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM org_key_shares WHERE member_user_id = $1 AND api_key_id IN
            (SELECT id FROM api_keys WHERE org_id = $2)",
        [user.id.clone().into(), org_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    // ORG-17a: leaving ends membership, so the leaver's org keys go with them —
    // a saved key string must stop billing the org wallet. Log history stays.
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM api_keys WHERE org_id = $1 AND created_by = $2",
        [org_id.clone().into(), user.id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(Json(json!({ "success": true })))
}

/// ORG-17b: delete one org key. The creator or the owner may delete; the org's
/// request-log history for the key is preserved.
pub async fn delete_org_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, key_id)): Path<(String, String)>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    // Checks run on the read pool. The mutations below MUST NOT nest: on SQLite every
    // DbPool::write() takes the same process-wide mutex, so calling the store's
    // delete (which takes its own write guard) inside an outer write transaction
    // would deadlock. Share rows go first in their own short transaction; orphaned
    // share rows can never match a live key, so the split is safe.
    let (role, created_by) = {
        let read = state.db_pool.read();
        let role = member_role(&*read, backend, &org_id, &user.id)
            .await?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
        let key = read
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT created_by FROM api_keys WHERE id = $1 AND org_id = $2",
                [key_id.clone().into(), org_id.clone().into()],
            ))
            .await
            .map_err(storage)?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "key not found"))?;
        (
            role,
            key.try_get::<Option<String>>("", "created_by")
                .map_err(storage)?,
        )
    };
    if role != "owner" && created_by.as_deref() != Some(user.id.as_str()) {
        return Err(forbidden(
            "only the key creator or the owner can delete an org key",
        ));
    }
    // The share-row delete gets its own scope so the SQLite write guard drops at
    // the closing brace, BEFORE the store's delete takes the same mutex below.
    {
        let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
        tx.execute(Statement::from_sql_and_values(
            backend,
            "DELETE FROM org_key_shares WHERE api_key_id = $1",
            [key_id.clone().into()],
        ))
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
    }
    state
        .user_store
        .delete_api_key(&key_id)
        .await
        .map_err(|e| bad_request(&e))?;
    Ok(Json(json!({ "success": true })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateOrgKeyRequest {
    pub name: Option<String>,
    pub group_ids: Option<Vec<String>>,
    pub model_limits_enabled: Option<bool>,
    pub model_limits: Option<Vec<String>>,
    pub ip_whitelist: Option<Vec<String>>,
    pub expires_in_days: Option<i64>,
}

/// ORG-17c: edit an org key's name, groups, model restriction, IP whitelist, or
/// expiry. Creator or owner — the same permission set as deletion (ORG-17b).
/// Group selection is validated against the org wallet row (enterprise public
/// groups), the same rule the personal key editor applies to its owner.
pub async fn update_org_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, key_id)): Path<(String, String)>,
    Json(body): Json<UpdateOrgKeyRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    let role = member_role(&*read, backend, &org_id, &user.id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let key = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT created_by FROM api_keys WHERE id = $1 AND org_id = $2",
            [key_id.clone().into(), org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "key not found"))?;
    let created_by: Option<String> = key.try_get("", "created_by").map_err(storage)?;
    if role != "owner" && created_by.as_deref() != Some(user.id.as_str()) {
        return Err(forbidden(
            "only the key creator or the owner can edit an org key",
        ));
    }
    if let Some(name) = body.name.as_deref() {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(bad_request("name must be 1..64 characters"));
        }
    }
    if body.expires_in_days.is_some_and(|days| days < 1) {
        return Err(bad_request("expires_in_days must be positive"));
    }
    let expires_at = body
        .expires_in_days
        .map(|days| (Utc::now() + Duration::days(days)).to_rfc3339());
    let input = crate::users::UpdateApiKeyInput {
        name: body
            .name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
        enabled: None,
        sub_account_enabled: None,
        sub_account_balance_nano_usd: None,
        model_limits_enabled: body.model_limits_enabled,
        model_limits: body.model_limits,
        ip_whitelist: body.ip_whitelist,
        group_ids: body.group_ids,
        channel_bindings: None,
        model_bindings: None,
        max_multiplier: None,
        transforms: None,
        model_redirects: None,
        reasoning_envelope_enabled: None,
        request_capture_mode: None,
        expires_at,
        spend_limit_total_nano_usd: None,
        spend_limit_hourly_nano_usd: None,
        spend_limit_daily_nano_usd: None,
    };
    let updated = state
        .user_store
        .update_api_key(&key_id, input, false)
        .await
        .map_err(|e| bad_request(&e))?;
    Ok(Json(json!({
        "success": true,
        "group_ids": updated.group_ids,
        "name": updated.name,
    })))
}

async fn require_org_member(
    org_id: &str,
    user_id: &str,
    state: &AppState,
) -> Result<String, AppError> {
    let backend = state.db_pool.read().get_database_backend();
    let read = state.db_pool.read();
    member_role(&*read, backend, org_id, user_id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))
}

/// ORG-23: usage analytics over every request made with a key of this org.
/// The response shape equals `GET /api/dashboard/analytics`.
pub async fn org_analytics(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AnalyticsQuery>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    require_org_member(&org_id, &user.id, &state).await?;
    let buckets = query.buckets.clamp(1, 48);
    let range_hours = query.range_hours.clamp(1, 720);
    let now = Utc::now();
    let time_to = now.to_rfc3339();
    let time_from = (now - Duration::hours(range_hours)).to_rfc3339();
    let today_start = now
        .date_naive()
        .and_time(chrono::NaiveTime::MIN)
        .and_utc()
        .to_rfc3339();
    let raw = state
        .user_store
        .get_dashboard_analytics(
            None,
            None,
            Some(&org_id),
            None,
            &time_from,
            &time_to,
            &today_start,
            buckets,
        )
        .await
        .map_err(storage)?;
    Ok(Json(super::analytics_request_logs::render_analytics_json(
        &raw,
        buckets,
        range_hours,
        now,
        &time_from,
        &time_to,
    )?))
}

/// ORG-24: request logs across every key of this org. The response shape equals
/// `GET /api/dashboard/request-logs`; error detail masking follows the same rule.
pub async fn org_request_logs(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<RequestLogsQuery>,
) -> AppResult<impl IntoResponse> {
    super::analytics_request_logs::validate_request_log_model_filter(&query)?;
    let user = get_current_user(&headers, &state).await?;
    super::analytics_request_logs::validate_request_log_time_filters(&query)?;
    require_org_member(&org_id, &user.id, &state).await?;
    let is_admin = user.role.can_manage_users();
    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);
    let (mut logs, total, total_charge_nano_usd) = state
        .user_store
        .list_request_logs_by_org(
            &org_id,
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
        .map_err(storage)?;
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

/// ORG-27: delete a space. Owner or admin. The remaining wallet balance refunds
/// to the owner, org keys are deleted, and every org row is removed in one
/// transaction. Ledger and request-log history stay.
pub async fn delete_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let org = tx
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT owner_user_id FROM orgs WHERE id = $1",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let owner_user_id: String = org.try_get("", "owner_user_id").map_err(storage)?;
    let is_owner = owner_user_id == user.id;
    if !is_owner && !user.role.can_manage_users() {
        return Err(forbidden(
            "only the owner or an admin can delete an organization",
        ));
    }

    let wallet = tx
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT balance_nano_usd FROM users WHERE id = $1",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| storage("org wallet row missing"))?;
    let org_balance: i128 = wallet
        .try_get::<String>("", "balance_nano_usd")
        .map_err(storage)?
        .parse()
        .map_err(|_| bad_request("invalid persisted balance"))?;
    let now = Utc::now().to_rfc3339();

    if org_balance > 0 {
        let owner_row = tx
            .query_one(Statement::from_sql_and_values(
                backend,
                "SELECT balance_nano_usd FROM users WHERE id = $1",
                [owner_user_id.clone().into()],
            ))
            .await
            .map_err(storage)?
            .ok_or_else(|| storage("owner row missing"))?;
        let owner_balance: i128 = owner_row
            .try_get::<String>("", "balance_nano_usd")
            .map_err(storage)?
            .parse()
            .map_err(|_| bad_request("invalid persisted balance"))?;
        let owner_after = owner_balance
            .checked_add(org_balance)
            .ok_or_else(|| bad_request("balance overflow"))?;
        tx.execute(Statement::from_sql_and_values(
            backend,
            "UPDATE users SET balance_nano_usd = $2, updated_at = $3 WHERE id = $1",
            [
                owner_user_id.clone().into(),
                owner_after.to_string().into(),
                now.clone().into(),
            ],
        ))
        .await
        .map_err(storage)?;
        tx.execute(Statement::from_sql_and_values(
            backend,
            "UPDATE users SET balance_nano_usd = '0', updated_at = $2 WHERE id = $1",
            [org_id.clone().into(), now.clone().into()],
        ))
        .await
        .map_err(storage)?;
        write_ledger_row(
            &tx,
            backend,
            &uuid::Uuid::new_v4().to_string(),
            &org_id,
            "org_delete_refund",
            -org_balance,
            0,
            json!({"to_user_id": owner_user_id}),
            &now,
        )
        .await?;
        write_ledger_row(
            &tx,
            backend,
            &uuid::Uuid::new_v4().to_string(),
            &owner_user_id,
            "org_delete_receive",
            org_balance,
            owner_after,
            json!({"from_org_id": org_id}),
            &now,
        )
        .await?;
    }

    // Shares must go before the keys are deleted.
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM org_key_shares WHERE api_key_id IN
            (SELECT id FROM api_keys WHERE org_id = $1)",
        [org_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM api_keys WHERE org_id = $1",
        [org_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM org_members WHERE org_id = $1",
        [org_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM orgs WHERE id = $1",
        [org_id.clone().into()],
    ))
    .await
    .map_err(storage)?;
    tx.execute(Statement::from_sql_and_values(
        backend,
        "DELETE FROM users WHERE id = $1 AND is_org = 1",
        [org_id.into()],
    ))
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(Json(json!({ "success": true })))
}

/// ORG-28a: the admin overview of every space.
pub async fn admin_list_orgs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    if !user.role.can_manage_users() {
        return Err(forbidden("admin only"));
    }
    let backend = state.db_pool.read().get_database_backend();
    let rows = state
        .db_pool
        .read()
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT o.id, o.display_name, o.avatar_emoji, o.avatar_color, o.avatar_image,
                    o.invite_token, o.invite_code, o.invite_expires_at, o.max_members,
                    o.created_at, ou.username AS owner_username, ou.id AS owner_user_id,
                    ou.org_creation_limit,
                    (SELECT COUNT(*) FROM org_members m WHERE m.org_id = o.id) AS member_count,
                    w.balance_nano_usd
             FROM orgs o
             JOIN users ou ON ou.id = o.owner_user_id
             JOIN users w ON w.id = o.id
             ORDER BY o.created_at DESC",
            [],
        ))
        .await
        .map_err(storage)?;
    let orgs = rows
        .iter()
        .map(|row| {
            Ok(json!({
                "id": row.try_get::<String>("", "id").map_err(storage)?,
                "display_name": row.try_get::<String>("", "display_name").map_err(storage)?,
                "avatar_emoji": row.try_get::<String>("", "avatar_emoji").map_err(storage)?,
                "avatar_color": row.try_get::<String>("", "avatar_color").map_err(storage)?,
                "avatar_image": row.try_get::<Option<String>>("", "avatar_image").map_err(storage)?,
                "owner_user_id": row.try_get::<String>("", "owner_user_id").map_err(storage)?,
                "owner_username": row.try_get::<String>("", "owner_username").map_err(storage)?,
                "owner_org_creation_limit": row
                    .try_get::<Option<i64>>("", "org_creation_limit")
                    .map_err(storage)?,
                "member_count": row.try_get::<i64>("", "member_count").map_err(storage)?,
                "max_members": limit_or_default(
                    row.try_get::<Option<i64>>("", "max_members").map_err(storage)?,
                    MAX_ORG_MEMBERS,
                ),
                "balance_nano_usd": row.try_get::<String>("", "balance_nano_usd").map_err(storage)?,
                "invite_token": row.try_get::<String>("", "invite_token").map_err(storage)?,
                "invite_code": row.try_get::<Option<String>>("", "invite_code").map_err(storage)?
                    .unwrap_or_default(),
                "invite_expires_at": row
                    .try_get::<Option<String>>("", "invite_expires_at")
                    .map_err(storage)?
                    .unwrap_or_default(),
                "created_at": row.try_get::<String>("", "created_at").map_err(storage)?,
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(orgs))
}

#[derive(Debug, Deserialize)]
pub struct AdminUpdateOrgRequest {
    pub max_members: Option<i64>,
    pub owner_org_creation_limit: Option<i64>,
}

/// ORG-28b: adjust the per-org member cap and the owner's creation quota.
pub async fn admin_update_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<String>,
    Json(body): Json<AdminUpdateOrgRequest>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    if !user.role.can_manage_users() {
        return Err(forbidden("admin only"));
    }
    if let Some(max_members) = body.max_members {
        if !(1..=1000).contains(&max_members) {
            return Err(bad_request("max_members must be 1..1000"));
        }
    }
    if let Some(limit) = body.owner_org_creation_limit {
        if !(0..=100).contains(&limit) {
            return Err(bad_request("owner_org_creation_limit must be 0..100"));
        }
    }
    let backend = state.db_pool.read().get_database_backend();
    let tx = state.db_pool.write().await.begin().await.map_err(storage)?;
    let org = tx
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT owner_user_id FROM orgs WHERE id = $1",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    let owner_user_id: String = org.try_get("", "owner_user_id").map_err(storage)?;
    let now = Utc::now().to_rfc3339();
    if let Some(max_members) = body.max_members {
        tx.execute(Statement::from_sql_and_values(
            backend,
            "UPDATE orgs SET max_members = $2, updated_at = $3 WHERE id = $1",
            [
                org_id.clone().into(),
                max_members.into(),
                now.clone().into(),
            ],
        ))
        .await
        .map_err(storage)?;
    }
    if let Some(limit) = body.owner_org_creation_limit {
        tx.execute(Statement::from_sql_and_values(
            backend,
            "UPDATE users SET org_creation_limit = $2, updated_at = $3 WHERE id = $1",
            [owner_user_id.into(), limit.into(), now.into()],
        ))
        .await
        .map_err(storage)?;
    }
    tx.commit().await.map_err(storage)?;
    Ok(Json(json!({ "success": true })))
}

// ---- ORGL: org usage limits + member usage analysis ----

async fn require_org_owner(
    org_id: &str,
    user: &crate::users::User,
    state: &AppState,
) -> Result<(), AppError> {
    let role = member_role(
        &*state.db_pool.read(),
        state.db_pool.read().get_database_backend(),
        org_id,
        &user.id,
    )
    .await?
    .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?;
    if role != "owner" {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "org_forbidden",
            "owner role required",
        ));
    }
    Ok(())
}

fn window_json(w: &crate::users::org_limits::OrgSpendWindow) -> serde_json::Value {
    serde_json::json!({
        "limit_nano_usd": w.limit_nano_usd.map(|v| v.to_string()),
        "spent_nano_usd": w.spent_nano_usd.to_string(),
    })
}

/// ORGL-12: current limit set with live consumption, owner only.
pub async fn org_limits(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    require_org_owner(&org_id, &user, &state).await?;

    let read = state.db_pool.read();
    let backend = read.get_database_backend();
    let owner_id: String = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT owner_user_id FROM orgs WHERE id = $1",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "org not found"))?
        .try_get("", "owner_user_id")
        .map_err(storage)?;

    // Space limits + spent (same windows as enforcement).
    let space_levels = crate::users::org_limits::load_limit_levels(
        &state.user_store,
        &org_id,
        &owner_id,
        "00000000-0000-0000-0000-000000000000",
    )
    .await
    .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;

    let member_rows = read
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT m.user_id, u.username, m.role, m.alias,
                    m.spend_limit_total_nano_usd, m.spend_limit_hourly_nano_usd, m.spend_limit_daily_nano_usd
             FROM org_members m LEFT JOIN users u ON u.id = m.user_id
             WHERE m.org_id = $1 ORDER BY m.joined_at",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?;
    let mut members = Vec::new();
    for row in &member_rows {
        let member_id: String = row.try_get("", "user_id").map_err(storage)?;
        let levels = crate::users::org_limits::load_limit_levels(
            &state.user_store,
            &org_id,
            &member_id,
            "00000000-0000-0000-0000-000000000000",
        )
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;
        members.push(serde_json::json!({
            "user_id": member_id,
            "username": row.try_get::<Option<String>>("", "username").ok().flatten(),
            "alias": row.try_get::<Option<String>>("", "alias").ok().flatten(),
            "role": row.try_get::<String>("", "role").map_err(storage)?,
            "limits": {
                "total_nano_usd": row.try_get::<Option<String>>("", "spend_limit_total_nano_usd").ok().flatten().filter(|v| !v.is_empty()),
                "hourly_nano_usd": row.try_get::<Option<String>>("", "spend_limit_hourly_nano_usd").ok().flatten().filter(|v| !v.is_empty()),
                "daily_nano_usd": row.try_get::<Option<String>>("", "spend_limit_daily_nano_usd").ok().flatten().filter(|v| !v.is_empty()),
            },
            "spent": {
                "total_nano_usd": levels.member.as_ref().map(|m| m.total.spent_nano_usd.to_string()).unwrap_or_else(|| "0".to_string()),
                "hourly_nano_usd": levels.member.as_ref().map(|m| m.hourly.spent_nano_usd.to_string()).unwrap_or_else(|| "0".to_string()),
                "daily_nano_usd": levels.member.as_ref().map(|m| m.daily.spent_nano_usd.to_string()).unwrap_or_else(|| "0".to_string()),
            },
        }));
    }

    let key_rows = read
        .query_all(Statement::from_sql_and_values(
            backend,
            "SELECT k.id, k.name, k.created_by, u.username AS creator_username, om.alias AS creator_alias,
                    k.spend_limit_total_nano_usd, k.spend_limit_hourly_nano_usd, k.spend_limit_daily_nano_usd
             FROM api_keys k LEFT JOIN users u ON u.id = k.created_by
             LEFT JOIN org_members om ON om.org_id = k.org_id AND om.user_id = k.created_by
             WHERE k.org_id = $1
               AND (k.created_by IS NULL OR k.created_by IN
                    (SELECT user_id FROM org_members WHERE org_id = $1))
             ORDER BY k.created_at",
            [org_id.clone().into()],
        ))
        .await
        .map_err(storage)?;
    let mut keys = Vec::new();
    for row in &key_rows {
        let key_id: String = row.try_get("", "id").map_err(storage)?;
        let levels = crate::users::org_limits::load_limit_levels(
            &state.user_store,
            &org_id,
            &owner_id,
            &key_id,
        )
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;
        keys.push(serde_json::json!({
            "key_id": key_id,
            "name": row.try_get::<String>("", "name").map_err(storage)?,
            "created_by": row.try_get::<Option<String>>("", "created_by").ok().flatten(),
            "creator_username": row.try_get::<Option<String>>("", "creator_username").ok().flatten(),
            "creator_alias": row.try_get::<Option<String>>("", "creator_alias").ok().flatten(),
            "limits": {
                "total_nano_usd": row.try_get::<Option<String>>("", "spend_limit_total_nano_usd").ok().flatten().filter(|v| !v.is_empty()),
                "hourly_nano_usd": row.try_get::<Option<String>>("", "spend_limit_hourly_nano_usd").ok().flatten().filter(|v| !v.is_empty()),
                "daily_nano_usd": row.try_get::<Option<String>>("", "spend_limit_daily_nano_usd").ok().flatten().filter(|v| !v.is_empty()),
            },
            "spent": {
                "total_nano_usd": levels.key.as_ref().map(|k| k.total.spent_nano_usd.to_string()).unwrap_or_else(|| "0".to_string()),
                "hourly_nano_usd": levels.key.as_ref().map(|k| k.hourly.spent_nano_usd.to_string()).unwrap_or_else(|| "0".to_string()),
                "daily_nano_usd": levels.key.as_ref().map(|k| k.daily.spent_nano_usd.to_string()).unwrap_or_else(|| "0".to_string()),
            },
        }));
    }

    Ok(Json(serde_json::json!({
        "space": {
            "limits": {
                "total_nano_usd": space_levels.space.total.limit_nano_usd.map(|v| v.to_string()),
                "hourly_nano_usd": space_levels.space.hourly.limit_nano_usd.map(|v| v.to_string()),
                "daily_nano_usd": space_levels.space.daily.limit_nano_usd.map(|v| v.to_string()),
            },
            "spent": {
                "total_nano_usd": space_levels.space.total.spent_nano_usd.to_string(),
                "hourly_nano_usd": space_levels.space.hourly.spent_nano_usd.to_string(),
                "daily_nano_usd": space_levels.space.daily.spent_nano_usd.to_string(),
            },
        },
        "members": members,
        "keys": keys,
    })))
}

/// ORGL-10: set space + member limits, owner only.
pub async fn update_org_limits(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    headers: HeaderMap,
    body: axum::Json<serde_json::Value>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    require_org_owner(&org_id, &user, &state).await?;
    let space = body
        .get("space")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let members = body
        .get("members")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    crate::users::org_limits::apply_org_limit_patch(&state.user_store, &org_id, &space, &members)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_REQUEST, "invalid_request", e))?;
    Ok(Json(json!({"ok": true})))
}

/// ORGL-11: set key-level limits. Owner or the key's creator.
pub async fn update_org_key_limits(
    State(state): State<AppState>,
    Path((org_id, key_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::Json<serde_json::Value>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    require_org_member(&org_id, &user.id, &state).await?;
    let read = state.db_pool.read();
    let backend = read.get_database_backend();
    let key_row = read
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT created_by, user_id FROM api_keys WHERE id = $1 AND org_id = $2",
            [key_id.clone().into(), org_id.clone().into()],
        ))
        .await
        .map_err(storage)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found", "key not found"))?;
    let created_by: Option<String> = key_row.try_get("", "created_by").map_err(storage)?;
    let is_owner = member_role(&*read, backend, &org_id, &user.id)
        .await?
        .is_some_and(|r| r == "owner");
    if !is_owner && created_by.as_deref() != Some(user.id.as_str()) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "org_forbidden",
            "owner or key creator required",
        ));
    }
    crate::users::org_limits::apply_org_key_limit_patch(&state.user_store, &org_id, &key_id, &body)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_REQUEST, "invalid_request", e))?;
    Ok(Json(json!({"ok": true})))
}

#[derive(serde::Deserialize)]
pub struct MemberUsageQuery {
    pub range_hours: Option<i64>,
    pub buckets: Option<i64>,
}

/// ORGL-13: per-member usage analysis, owner only.
pub async fn org_member_usage(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<MemberUsageQuery>,
) -> AppResult<impl IntoResponse> {
    let user = get_current_user(&headers, &state).await?;
    require_org_owner(&org_id, &user, &state).await?;
    let range_hours = query.range_hours.unwrap_or(24).clamp(1, 720);
    let buckets = query.buckets.unwrap_or(24).clamp(1, 48);
    let value =
        crate::users::org_limits::member_usage(&state.user_store, &org_id, range_hours, buckets)
            .await
            .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e))?;
    Ok(Json(value))
}

#[cfg(test)]
mod tests {
    use super::{
        CreateOrgKeyRequest, create_org, create_org_key, deposit_to_org, distribute_from_org,
        join_org, leave_org, list_org_keys, org_detail, org_ledger, set_org_member_alias,
    };
    use crate::app::{AppState, RuntimeConfig, load_state_with_runtime};
    use crate::users::{AccountClass, CreateGroupInput, UserRole};
    use axum::Json;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use serde_json::Value;

    async fn make_state() -> AppState {
        load_state_with_runtime(RuntimeConfig {
            listen: "127.0.0.1:0".to_string(),
            metrics_path: "/metrics".to_string(),
            database_dsn: "sqlite::memory:".to_string(),
            request_log_spool_dir: None,
            node: crate::node_config::NodeSettings::primary_default(),
        })
        .await
        .expect("state loads")
    }

    async fn session_headers(state: &AppState, username: &str, role: UserRole) -> HeaderMap {
        let user = state
            .user_store
            .create_user(username, "password123", role, None)
            .await
            .expect("user created");
        let session = state
            .user_store
            .create_session(&user.id, 7)
            .await
            .expect("session created");
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", session.token)).expect("header value"),
        );
        headers
    }

    async fn session_headers_in_group(
        state: &AppState,
        username: &str,
        group_id: &str,
    ) -> HeaderMap {
        let user = state
            .user_store
            .create_user(username, "password123", UserRole::User, Some(group_id))
            .await
            .expect("user created");
        let session = state
            .user_store
            .create_session(&user.id, 7)
            .await
            .expect("session created");
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", session.token)).expect("header value"),
        );
        headers
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("json body")
    }

    /// ORG-17a: leaving deletes the leaver's org keys in the same transaction;
    /// a saved key string stops billing the org wallet immediately.
    #[tokio::test]
    async fn leaving_the_space_deletes_the_leavers_org_keys() {
        let state = make_state().await;
        let admin_headers = session_headers(&state, "leave_admin", UserRole::Admin).await;

        let (_, Json(enterprise_group)) = crate::dashboard_handlers::create_group(
            State(state.clone()),
            admin_headers.clone(),
            Json(CreateGroupInput {
                confirm_public_exposure: true,
                name: "ent-leave".to_string(),
                description: String::new(),
                user_selectable: true,
                sort_order: 5,
                account_class: AccountClass::Enterprise,
            }),
        )
        .await
        .expect("enterprise group created");

        let owner_headers =
            session_headers_in_group(&state, "leave_owner", &enterprise_group.id).await;
        let created = body_json(
            create_org(
                State(state.clone()),
                owner_headers.clone(),
                Json(super::CreateOrgRequest {
                    display_name: "Leave Flow".to_string(),
                    avatar_emoji: None,
                    avatar_color: None,
                    avatar_image: None,
                    invite_expiry: "never".to_string(),
                }),
            )
            .await
            .expect("org created")
            .into_response(),
        )
        .await;
        let org_id: String = created["id"].as_str().expect("org id").to_string();
        let invite_token: String = created["invite_token"]
            .as_str()
            .expect("invite token")
            .to_string();

        let (_, Json(private_group)) = crate::dashboard_handlers::create_group(
            State(state.clone()),
            admin_headers.clone(),
            Json(CreateGroupInput {
                confirm_public_exposure: true,
                name: "priv-leave".to_string(),
                description: String::new(),
                user_selectable: true,
                sort_order: 6,
                account_class: AccountClass::Private,
            }),
        )
        .await
        .expect("private group created");
        let member_headers =
            session_headers_in_group(&state, "leave_member", &private_group.id).await;
        join_org(
            State(state.clone()),
            member_headers.clone(),
            Json(super::JoinOrgRequest {
                token: invite_token,
            }),
        )
        .await
        .expect("member joined");

        let created_key = body_json(
            create_org_key(
                State(state.clone()),
                member_headers.clone(),
                Path(org_id.clone()),
                Json(CreateOrgKeyRequest {
                    name: "leaver key".to_string(),
                    share_mode: None,
                    model_limits_enabled: None,
                    model_limits: Vec::new(),
                    group_ids: Vec::new(),
                    expires_in_days: None,
                    ip_whitelist: Vec::new(),
                }),
            )
            .await
            .expect("member key creates")
            .into_response(),
        )
        .await;
        let key_id: String = created_key["id"].as_str().expect("key id").to_string();

        leave_org(State(state.clone()), member_headers, Path(org_id.clone()))
            .await
            .expect("member leaves");

        let gone = state
            .user_store
            .get_api_key_by_id(&key_id)
            .await
            .expect("lookup succeeds");
        assert!(gone.is_none(), "the leaver's org key must be deleted");

        // The owner's shared-key list (b) never surfaces keys of non-members.
        let keys = body_json(
            list_org_keys(State(state.clone()), owner_headers, Path(org_id))
                .await
                .expect("owner list succeeds")
                .into_response(),
        )
        .await;
        let shared = keys["shared"].as_array().expect("shared array");
        assert!(
            shared.is_empty(),
            "non-member keys must not appear in the owner's list"
        );
    }

    /// ORG-14a: the key picker feeds every member from the wallet's own Groups, so a
    /// member whose account class differs from the wallet's still sees — and may select —
    /// the wallet-class Groups; the member's personal Groups never leak into the list.
    #[tokio::test]
    async fn org_detail_wallet_groups_serve_every_member_class() {
        let state = make_state().await;
        let admin_headers = session_headers(&state, "org_admin", UserRole::Admin).await;

        let (_, Json(enterprise_group)) = crate::dashboard_handlers::create_group(
            State(state.clone()),
            admin_headers.clone(),
            Json(CreateGroupInput {
                confirm_public_exposure: true,
                name: "ent-pick".to_string(),
                description: String::new(),
                user_selectable: true,
                sort_order: 5,
                account_class: AccountClass::Enterprise,
            }),
        )
        .await
        .expect("enterprise group created");
        let (_, Json(private_group)) = crate::dashboard_handlers::create_group(
            State(state.clone()),
            admin_headers.clone(),
            Json(CreateGroupInput {
                confirm_public_exposure: true,
                name: "priv-pick".to_string(),
                description: String::new(),
                user_selectable: true,
                sort_order: 6,
                account_class: AccountClass::Private,
            }),
        )
        .await
        .expect("private group created");

        // The wallet lands in the first public enterprise group, so the owner is
        // created inside enterprise_group to qualify (ORG-5).
        let owner_headers =
            session_headers_in_group(&state, "org_owner", &enterprise_group.id).await;
        let created = body_json(
            create_org(
                State(state.clone()),
                owner_headers.clone(),
                Json(super::CreateOrgRequest {
                    display_name: "Wallet Fed".to_string(),
                    avatar_emoji: None,
                    avatar_color: None,
                    avatar_image: None,
                    invite_expiry: "never".to_string(),
                }),
            )
            .await
            .expect("org created")
            .into_response(),
        )
        .await;
        let org_id: String = created["id"].as_str().expect("org id").to_string();
        let invite_token: String = created["invite_token"]
            .as_str()
            .expect("owner receives the invite token")
            .to_string();

        // A private-class member joins; their own dashboard-group list holds only
        // private-class Groups, so the pre-ORG-14a picker rendered zero options.
        let member_headers =
            session_headers_in_group(&state, "org_member", &private_group.id).await;
        join_org(
            State(state.clone()),
            member_headers.clone(),
            Json(super::JoinOrgRequest {
                token: invite_token,
            }),
        )
        .await
        .expect("member joined");

        for headers in [&owner_headers, &member_headers] {
            let body = body_json(
                org_detail(State(state.clone()), headers.clone(), Path(org_id.clone()))
                    .await
                    .expect("detail succeeds")
                    .into_response(),
            )
            .await;
            let wallet_groups = body["wallet_groups"].as_array().expect("wallet_groups");
            let names: Vec<&str> = wallet_groups
                .iter()
                .map(|group| group["name"].as_str().expect("name"))
                .collect();
            assert_eq!(names, ["ent-pick"]);
        }

        // The member can create a key on the wallet-class group: the backend has
        // always validated against the wallet, and now the picker offers exactly
        // these ids.
        let response = create_org_key(
            State(state.clone()),
            member_headers.clone(),
            Path(org_id.clone()),
            Json(CreateOrgKeyRequest {
                name: "wallet-fed key".to_string(),
                share_mode: None,
                model_limits_enabled: None,
                model_limits: Vec::new(),
                group_ids: vec![enterprise_group.id.clone()],
                expires_in_days: None,
                ip_whitelist: Vec::new(),
            }),
        )
        .await
        .expect("member creates an org key on a wallet-class group")
        .into_response();
        assert_eq!(response.status(), StatusCode::CREATED);

        // A member-class group is rejected for the wallet exactly as before. The
        // map(|_| ()) keeps expect_err usable: the handler's opaque Ok type is not
        // Debug but () is.
        let rejected = create_org_key(
            State(state.clone()),
            member_headers.clone(),
            Path(org_id.clone()),
            Json(CreateOrgKeyRequest {
                name: "cross-class key".to_string(),
                share_mode: None,
                model_limits_enabled: None,
                model_limits: Vec::new(),
                group_ids: vec![private_group.id.clone()],
                expires_in_days: None,
                ip_whitelist: Vec::new(),
            }),
        )
        .await
        .map(|_| ())
        .expect_err("cross-class group must fail validation");
        assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    }

    /// ORG-22a: the org ledger carries per-entry meta and an actors map so the
    /// wallet UI can name who recharged and who received a distribution.
    #[tokio::test]
    async fn org_ledger_resolves_move_actors_with_aliases() {
        let state = make_state().await;
        let admin_headers = session_headers(&state, "ledger_admin", UserRole::Admin).await;

        let (_, Json(enterprise_group)) = crate::dashboard_handlers::create_group(
            State(state.clone()),
            admin_headers.clone(),
            Json(CreateGroupInput {
                confirm_public_exposure: true,
                name: "ent-ledger".to_string(),
                description: String::new(),
                user_selectable: true,
                sort_order: 5,
                account_class: AccountClass::Enterprise,
            }),
        )
        .await
        .expect("enterprise group created");

        let owner_headers =
            session_headers_in_group(&state, "ledger_owner", &enterprise_group.id).await;
        let created = body_json(
            create_org(
                State(state.clone()),
                owner_headers.clone(),
                Json(super::CreateOrgRequest {
                    display_name: "Ledger Flow".to_string(),
                    avatar_emoji: None,
                    avatar_color: None,
                    avatar_image: None,
                    invite_expiry: "never".to_string(),
                }),
            )
            .await
            .expect("org created")
            .into_response(),
        )
        .await;
        let org_id: String = created["id"].as_str().expect("org id").to_string();
        let invite_token: String = created["invite_token"]
            .as_str()
            .expect("invite token")
            .to_string();

        let member_headers =
            session_headers_in_group(&state, "ledger_member", &enterprise_group.id).await;
        join_org(
            State(state.clone()),
            member_headers.clone(),
            Json(super::JoinOrgRequest {
                token: invite_token,
            }),
        )
        .await
        .expect("member joined");

        let member_id = body_json(
            org_detail(State(state.clone()), owner_headers.clone(), Path(org_id.clone()))
                .await
                .expect("org detail")
                .into_response(),
        )
        .await;
        let members = member_id["members"].as_array().expect("members array");
        let member_user_id = members
            .iter()
            .find(|entry| entry["username"].as_str() == Some("ledger_member"))
            .expect("member listed")
            ["user_id"]
            .as_str()
            .expect("member id")
            .to_string();

        set_org_member_alias(
            State(state.clone()),
            owner_headers.clone(),
            Path((org_id.clone(), member_user_id.clone())),
            Json(super::SetOrgMemberAliasRequest {
                alias: Some("ledger-nickname".to_string()),
            }),
        )
        .await
        .expect("alias set");

        // Fund the member so they can recharge the org wallet (ORG-12).
        state
            .user_store
            .update_user(
                &member_user_id,
                None,
                None,
                None,
                None,
                Some("1000000000"),
                Some(true),
                None,
                None,
            )
            .await
            .expect("member funded");

        deposit_to_org(
            State(state.clone()),
            member_headers.clone(),
            Path(org_id.clone()),
            Json(super::WalletMoveRequest {
                amount_nano_usd: "500000000".to_string(),
            }),
        )
        .await
        .expect("member deposits");

        distribute_from_org(
            State(state.clone()),
            owner_headers.clone(),
            Path(org_id.clone()),
            Json(super::DistributeRequest {
                member_user_id: member_user_id.clone(),
                amount_nano_usd: "200000000".to_string(),
            }),
        )
        .await
        .expect("owner distributes");

        let ledger = body_json(
            org_ledger(State(state.clone()), owner_headers.clone(), Path(org_id.clone()))
                .await
                .expect("ledger reads")
                .into_response(),
        )
        .await;

        let entries = ledger["entries"].as_array().expect("entries array");
        assert_eq!(entries.len(), 2, "one receive row and one grant row");
        let receive = entries
            .iter()
            .find(|entry| entry["kind"].as_str() == Some("org_deposit_receive"))
            .expect("receive entry");
        let grant = entries
            .iter()
            .find(|entry| entry["kind"].as_str() == Some("org_grant"))
            .expect("grant entry");
        assert_eq!(
            receive["meta"]["from_user_id"].as_str(),
            Some(member_user_id.as_str())
        );
        assert_eq!(
            grant["meta"]["to_user_id"].as_str(),
            Some(member_user_id.as_str())
        );

        let actor = &ledger["actors"][&member_user_id];
        assert_eq!(actor["username"].as_str(), Some("ledger_member"));
        assert_eq!(actor["alias"].as_str(), Some("ledger-nickname"));
    }
}

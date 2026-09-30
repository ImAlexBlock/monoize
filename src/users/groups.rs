use super::store::parse_group_ids_json;
use super::{AccountClass, UserStore};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, QueryResult, TransactionTrait, Value as SeaValue};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

fn default_public() -> bool {
    true
}

/// One `monoize_groups` registry row (`groups-registry.spec.md` §1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub user_selectable: bool,
    pub is_public: bool,
    #[serde(default)]
    pub account_class: AccountClass,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGroupInput {
    pub name: String,
    #[serde(default)]
    pub confirm_public_exposure: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_public")]
    #[serde(rename = "is_public", alias = "user_selectable")]
    pub user_selectable: bool,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub account_class: AccountClass,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateGroupInput {
    pub name: Option<String>,
    #[serde(default)]
    pub confirm_public_exposure: bool,
    pub description: Option<String>,
    #[serde(rename = "is_public", alias = "user_selectable")]
    pub user_selectable: Option<bool>,
    pub sort_order: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReorderGroupsInput {
    pub group_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupStoreError {
    NotFound,
    PublicNameConflict(String),
    PublicExposureConfirmationRequired,
    InvalidName,
    InvalidDescription,
    InvalidReorder(String),
    CannotDeleteDefault,
    GroupInUse,
    Storage(String),
}

const GROUP_COLUMNS: &str = "id, name, description, is_default, user_selectable, is_public, account_class, sort_order, created_at, updated_at";
const QUALIFIED_GROUP_COLUMNS: &str = "g.id, g.name, g.description, g.is_default, g.user_selectable, g.is_public, g.account_class, g.sort_order, g.created_at, g.updated_at";

fn storage(error: impl std::fmt::Display) -> GroupStoreError {
    GroupStoreError::Storage(error.to_string())
}

fn validate_name(raw: &str) -> Result<String, GroupStoreError> {
    crate::public_name::canonicalize_public_name(raw)
        .map(|name| name.value)
        .map_err(|_| GroupStoreError::InvalidName)
}

fn validate_description(raw: &str) -> Result<String, GroupStoreError> {
    let description = raw.trim().to_string();
    if description.chars().count() > 256 {
        return Err(GroupStoreError::InvalidDescription);
    }
    Ok(description)
}

fn parse_time(row: &QueryResult, column: &str) -> Result<DateTime<Utc>, GroupStoreError> {
    let raw: String = row.try_get("", column).map_err(storage)?;
    DateTime::parse_from_rfc3339(&raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(storage)
}

pub(crate) fn row_to_group(row: &QueryResult) -> Result<Group, GroupStoreError> {
    let account_class_raw: String = row.try_get("", "account_class").map_err(storage)?;
    let account_class = AccountClass::from_str(&account_class_raw).ok_or_else(|| {
        GroupStoreError::Storage(format!(
            "invalid persisted monoize_groups.account_class: {account_class_raw:?}"
        ))
    })?;
    Ok(Group {
        id: row.try_get("", "id").map_err(storage)?,
        name: row.try_get("", "name").map_err(storage)?,
        description: row.try_get("", "description").map_err(storage)?,
        is_default: row.try_get::<i32>("", "is_default").map_err(storage)? != 0,
        user_selectable: row.try_get::<i32>("", "user_selectable").map_err(storage)? != 0,
        is_public: row.try_get::<i32>("", "is_public").map_err(storage)? != 0,
        account_class,
        sort_order: row.try_get("", "sort_order").map_err(storage)?,
        created_at: parse_time(row, "created_at")?,
        updated_at: parse_time(row, "updated_at")?,
    })
}

fn is_name_unique_violation(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    (lower.contains("unique") || lower.contains("duplicate"))
        && (lower.contains("name") || lower.contains("uq_monoize_groups_name_lower"))
}

impl UserStore {
    /// List every registry row in canonical order (GR-D5).
    pub async fn list_groups(&self) -> Result<Vec<Group>, String> {
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "SELECT {GROUP_COLUMNS} FROM monoize_groups \
                     ORDER BY sort_order ASC, created_at ASC, id ASC"
                ),
                vec![],
            ))
            .await
            .map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| row_to_group(row).map_err(|error| format!("{error:?}")))
            .collect()
    }

    pub async fn list_groups_for_user(
        &self,
        user_id: &str,
        role: crate::users::UserRole,
    ) -> Result<Vec<Group>, String> {
        if role.can_manage_users() {
            return self.list_groups().await;
        }
        let rows = self.db.read().query_all(self.db.stmt(
            &format!("SELECT {QUALIFIED_GROUP_COLUMNS} FROM monoize_groups g JOIN users u ON u.id = $1 AND u.account_class = g.account_class WHERE g.is_public = 1 OR EXISTS (SELECT 1 FROM user_group_grants ug WHERE ug.user_id = u.id AND ug.group_id = g.id) ORDER BY g.sort_order ASC, g.created_at ASC, g.id ASC"),
            vec![user_id.into()],
        )).await.map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| row_to_group(row).map_err(|e| format!("{e:?}")))
            .collect()
    }

    pub async fn accessible_group_ids(
        &self,
        user_id: &str,
        role: crate::users::UserRole,
    ) -> Result<Vec<String>, String> {
        let visibility = if role.can_manage_users() {
            "1 = 1"
        } else {
            "g.is_public = 1 OR EXISTS (SELECT 1 FROM user_group_grants ug WHERE ug.user_id = u.id AND ug.group_id = g.id)"
        };
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "SELECT g.id FROM monoize_groups g JOIN users u ON u.id = $1 AND u.account_class = g.account_class WHERE {visibility} ORDER BY g.sort_order ASC, g.created_at ASC, g.id ASC"
                ),
                vec![user_id.into()],
            ))
            .await
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .map(|row| row.try_get("", "id").map_err(|error| error.to_string()))
            .collect()
    }

    pub async fn list_groups_by_account_class(
        &self,
        account_class: AccountClass,
    ) -> Result<Vec<Group>, String> {
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "SELECT {GROUP_COLUMNS} FROM monoize_groups WHERE account_class = $1 \
                     ORDER BY sort_order ASC, created_at ASC, id ASC"
                ),
                vec![account_class.as_str().into()],
            ))
            .await
            .map_err(|error| error.to_string())?;
        rows.iter()
            .map(|row| row_to_group(row).map_err(|error| format!("{error:?}")))
            .collect()
    }

    pub async fn grant_group_access(&self, user_id: &str, group_id: &str) -> Result<(), String> {
        let matches = self.db.read().query_one(self.db.stmt(
            "SELECT 1 AS one FROM users u JOIN monoize_groups g ON g.id = $2 AND g.account_class = u.account_class WHERE u.id = $1",
            vec![user_id.into(), group_id.into()],
        )).await.map_err(|error| error.to_string())?;
        if matches.is_none() {
            return Err("unknown group id".to_string());
        }
        self.db.write().await.execute(self.db.stmt(
            "INSERT INTO user_group_grants (user_id, group_id, created_at) VALUES ($1, $2, $3) ON CONFLICT (user_id, group_id) DO NOTHING",
            vec![user_id.into(), group_id.into(), Utc::now().to_rfc3339().into()],
        )).await.map_err(|e| e.to_string())?;
        self.api_key_cache.invalidate_all();
        Ok(())
    }

    /// Users granted access to one Group, oldest grant first.
    pub async fn list_group_grant_users(
        &self,
        group_id: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                "SELECT u.id, u.username FROM user_group_grants ug                  JOIN users u ON u.id = ug.user_id WHERE ug.group_id = $1                  ORDER BY ug.created_at ASC, u.id ASC",
                vec![group_id.into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("", "id").map_err(|e| e.to_string())?,
                    row.try_get("", "username").map_err(|e| e.to_string())?,
                ))
            })
            .collect()
    }

    pub async fn revoke_group_access(&self, user_id: &str, group_id: &str) -> Result<(), String> {
        self.db
            .write()
            .await
            .execute(self.db.stmt(
                "DELETE FROM user_group_grants WHERE user_id = $1 AND group_id = $2",
                vec![user_id.into(), group_id.into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        self.api_key_cache.invalidate_all();
        Ok(())
    }

    pub async fn get_group_by_id(&self, id: &str) -> Result<Option<Group>, String> {
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(
                &format!("SELECT {GROUP_COLUMNS} FROM monoize_groups WHERE id = $1"),
                vec![id.into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        match row {
            Some(row) => Ok(Some(row_to_group(&row).map_err(|e| format!("{e:?}"))?)),
            None => Ok(None),
        }
    }

    /// The id of the single `is_default = 1` row (GR-D2).
    pub async fn default_group_id(&self) -> Result<String, String> {
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(
                "SELECT id FROM monoize_groups WHERE is_default = 1 LIMIT 1",
                vec![],
            ))
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "default group row missing (GR-D2 violated)".to_string())?;
        row.try_get("", "id").map_err(|e| e.to_string())
    }

    /// GR-C3: every element must reference an existing registry row.
    /// Returns the first unknown id, or `None` when all ids exist.
    pub async fn find_unknown_group_id(
        &self,
        group_ids: &[String],
    ) -> Result<Option<String>, String> {
        for id in group_ids {
            if self.get_group_by_id(id).await?.is_none() {
                return Ok(Some(id.clone()));
            }
        }
        Ok(None)
    }

    pub async fn create_group(&self, input: CreateGroupInput) -> Result<Group, GroupStoreError> {
        let name = validate_name(&input.name)?;
        if !input.confirm_public_exposure {
            return Err(GroupStoreError::PublicExposureConfirmationRequired);
        }
        let public_name = crate::public_name::canonicalize_public_name(&name)
            .map_err(|_| GroupStoreError::InvalidName)?;
        let description = validate_description(&input.description)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();

        if self.group_name_exists(None, &name).await? {
            return Err(GroupStoreError::PublicNameConflict(name));
        }
        let is_public = input.user_selectable;
        let result = self
            .db
            .write()
            .await
            .execute(self.db.stmt(
                "INSERT INTO monoize_groups (id, name, public_name, public_name_key, description, is_default, user_selectable, is_public, account_class, sort_order, created_at, updated_at) \
                 VALUES ($1, $2, $2, $3, $4, 0, $5, $6, $7, $8, $9, $9)",
                vec![
                    id.clone().into(),
                    name.clone().into(),
                    SeaValue::Bytes(Some(Box::new(public_name.key))),
                    description.clone().into(),
                    SeaValue::Int(Some(if input.user_selectable { 1 } else { 0 })),
                    SeaValue::Int(Some(if is_public { 1 } else { 0 })),
                    input.account_class.as_str().into(),
                    SeaValue::Int(Some(input.sort_order)),
                    now.to_rfc3339().into(),
                ],
            ))
            .await;
        if let Err(error) = result {
            let message = error.to_string();
            if is_name_unique_violation(&message) {
                return Err(GroupStoreError::PublicNameConflict(name));
            }
            return Err(GroupStoreError::Storage(message));
        }

        crate::monoize_routing::bump_registry_generation();
        Ok(Group {
            id,
            name,
            description,
            is_default: false,
            user_selectable: input.user_selectable,
            is_public,
            account_class: input.account_class,
            sort_order: input.sort_order,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn update_group(
        &self,
        id: &str,
        input: UpdateGroupInput,
    ) -> Result<Group, GroupStoreError> {
        let name = input.name.as_deref().map(validate_name).transpose()?;
        let description = input
            .description
            .as_deref()
            .map(validate_description)
            .transpose()?;

        let existing = self
            .get_group_by_id(id)
            .await
            .map_err(GroupStoreError::Storage)?
            .ok_or(GroupStoreError::NotFound)?;

        if name.as_ref().is_some_and(|name| name != &existing.name)
            && !input.confirm_public_exposure
        {
            return Err(GroupStoreError::PublicExposureConfirmationRequired);
        }

        if let Some(name) = &name
            && self.group_name_exists(Some(id), name).await?
        {
            return Err(GroupStoreError::PublicNameConflict(name.clone()));
        }

        let mut set_clauses = Vec::new();
        let mut values: Vec<SeaValue> = Vec::new();
        let mut idx = 1usize;
        if let Some(name) = &name {
            set_clauses.push(format!("name = ${idx}"));
            values.push(name.clone().into());
            idx += 1;
            set_clauses.push(format!("public_name = ${idx}"));
            values.push(name.clone().into());
            idx += 1;
            set_clauses.push(format!("public_name_key = ${idx}"));
            values.push(SeaValue::Bytes(Some(Box::new(name.as_bytes().to_vec()))));
            idx += 1;
        }
        if let Some(description) = &description {
            set_clauses.push(format!("description = ${idx}"));
            values.push(description.clone().into());
            idx += 1;
        }
        if let Some(user_selectable) = input.user_selectable {
            set_clauses.push(format!("user_selectable = ${idx}"));
            values.push(SeaValue::Int(Some(if user_selectable { 1 } else { 0 })));
            idx += 1;
            set_clauses.push(format!("is_public = ${idx}"));
            values.push(SeaValue::Int(Some(if user_selectable { 1 } else { 0 })));
            idx += 1;
        }
        if let Some(sort_order) = input.sort_order {
            set_clauses.push(format!("sort_order = ${idx}"));
            values.push(SeaValue::Int(Some(sort_order)));
            idx += 1;
        }
        if set_clauses.is_empty() {
            return Ok(existing);
        }
        let now = Utc::now();
        set_clauses.push(format!("updated_at = ${idx}"));
        values.push(now.to_rfc3339().into());
        idx += 1;
        values.push(id.into());

        let result = self
            .db
            .write()
            .await
            .execute(self.db.stmt(
                &format!(
                    "UPDATE monoize_groups SET {} WHERE id = ${idx}",
                    set_clauses.join(", ")
                ),
                values,
            ))
            .await;
        if let Err(error) = result {
            let message = error.to_string();
            if is_name_unique_violation(&message) {
                return Err(GroupStoreError::PublicNameConflict(
                    name.clone().unwrap_or_else(|| existing.name.clone()),
                ));
            }
            return Err(GroupStoreError::Storage(message));
        }

        // GR-A6: cached authentication results are keyed to registry state.
        self.api_key_cache.invalidate_all();
        crate::monoize_routing::bump_registry_generation();

        Ok(Group {
            id: existing.id,
            name: name.unwrap_or(existing.name),
            description: description.unwrap_or(existing.description),
            is_default: existing.is_default,
            user_selectable: input.user_selectable.unwrap_or(existing.user_selectable),
            is_public: input.user_selectable.unwrap_or(existing.is_public),
            account_class: existing.account_class,
            sort_order: input.sort_order.unwrap_or(existing.sort_order),
            created_at: existing.created_at,
            updated_at: now,
        })
    }

    pub async fn reorder_groups(&self, input: ReorderGroupsInput) -> Result<(), GroupStoreError> {
        if input.group_ids.len() > 199 {
            return Err(GroupStoreError::InvalidReorder(
                "group reorder accepts at most 199 ids".to_string(),
            ));
        }

        let mut unique_ids = HashSet::new();
        for id in &input.group_ids {
            if !unique_ids.insert(id.clone()) {
                return Err(GroupStoreError::InvalidReorder(
                    "group_ids contains duplicates".to_string(),
                ));
            }
        }

        let write = self.db.write().await;
        let tx = write.begin().await.map_err(storage)?;
        if self.db.is_postgres() {
            tx.execute_unprepared("LOCK TABLE monoize_groups IN SHARE ROW EXCLUSIVE MODE")
                .await
                .map_err(storage)?;
        }

        if input.group_ids.is_empty() {
            return Err(GroupStoreError::InvalidReorder(
                "group_ids must not be empty".to_string(),
            ));
        }

        let first = tx
            .query_one(self.db.stmt(
                "SELECT account_class FROM monoize_groups WHERE id = $1",
                vec![input.group_ids[0].clone().into()],
            ))
            .await
            .map_err(storage)?
            .ok_or_else(|| {
                GroupStoreError::InvalidReorder("group_ids contains an unknown id".to_string())
            })?;
        let account_class: String = first.try_get("", "account_class").map_err(storage)?;
        let rows = tx
            .query_all(self.db.stmt(
                "SELECT id FROM monoize_groups WHERE account_class = $1 ORDER BY id",
                vec![account_class.into()],
            ))
            .await
            .map_err(storage)?;
        let existing_ids: HashSet<String> = rows
            .into_iter()
            .map(|row| row.try_get("", "id").map_err(storage))
            .collect::<Result<_, _>>()?;
        if existing_ids.len() != input.group_ids.len() || existing_ids != unique_ids {
            return Err(GroupStoreError::InvalidReorder(
                "group_ids must contain every group of one account class exactly once".to_string(),
            ));
        }

        let mut values = Vec::with_capacity(input.group_ids.len() * 2 + 1);
        let mut cases = Vec::with_capacity(input.group_ids.len());
        for (sort_order, id) in input.group_ids.iter().enumerate() {
            let id_index = values.len() + 1;
            values.push(id.clone().into());
            let sort_order_index = values.len() + 1;
            values.push(SeaValue::Int(Some(sort_order as i32)));
            cases.push(format!("WHEN ${id_index} THEN ${sort_order_index}"));
        }
        let updated_at_index = values.len() + 1;
        values.push(Utc::now().to_rfc3339().into());
        tx.execute(self.db.stmt(
            &format!(
                    "UPDATE monoize_groups \
                 SET sort_order = CASE id {} END, updated_at = ${updated_at_index} \
                 WHERE id IN ({})",
                    cases.join(" "),
                    (1..=input.group_ids.len())
                        .map(|index| format!("${}", index * 2 - 1))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            values,
        ))
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        self.api_key_cache.invalidate_all();
        crate::monoize_routing::bump_registry_generation();
        Ok(())
    }

    /// Delete a non-default Group and update non-Provider references in one transaction.
    pub async fn delete_group(&self, id: &str) -> Result<(), GroupStoreError> {
        let target = self
            .get_group_by_id(id)
            .await
            .map_err(GroupStoreError::Storage)?
            .ok_or(GroupStoreError::NotFound)?;
        if target.is_default {
            return Err(GroupStoreError::CannotDeleteDefault);
        }
        let default_group_id = self
            .default_group_id()
            .await
            .map_err(GroupStoreError::Storage)?;

        let write = self.db.write().await;
        let tx = write.begin().await.map_err(storage)?;
        let provider_reference = tx
            .query_one(self.db.stmt(
                "SELECT 1 AS one FROM monoize_providers WHERE group_id = $1 LIMIT 1",
                vec![id.into()],
            ))
            .await
            .map_err(storage)?;
        if provider_reference.is_some() {
            return Err(GroupStoreError::GroupInUse);
        }

        // GR-X1: members move to the default group.
        tx.execute(self.db.stmt(
            "UPDATE users SET group_id = $1 WHERE group_id = $2",
            vec![default_group_id.clone().into(), id.into()],
        ))
        .await
        .map_err(storage)?;

        // GR-X2: drop the id from key selections. An empty list permits all Groups.
        let rows = tx
            .query_all(self.db.stmt("SELECT id, group_ids FROM api_keys", vec![]))
            .await
            .map_err(storage)?;
        for row in rows {
            let row_id: String = row.try_get("", "id").map_err(storage)?;
            let raw: Option<String> = row.try_get("", "group_ids").map_err(storage)?;
            let group_ids = parse_group_ids_json(raw.as_deref(), "api_keys.group_ids")
                .map_err(GroupStoreError::Storage)?;
            if !group_ids.iter().any(|gid| gid == id) {
                continue;
            }
            let remaining: Vec<String> = group_ids.into_iter().filter(|gid| gid != id).collect();
            tx.execute(self.db.stmt(
                "UPDATE api_keys SET group_ids = $1 WHERE id = $2",
                vec![
                    serde_json::to_string(&remaining).map_err(storage)?.into(),
                    row_id.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        }

        // GR-X4: an emptied plan ceiling stays [] (unrestricted).
        let rows = tx
            .query_all(
                self.db
                    .stmt("SELECT id, group_ids FROM billing_plans", vec![]),
            )
            .await
            .map_err(storage)?;
        for row in rows {
            let row_id: String = row.try_get("", "id").map_err(storage)?;
            let raw: Option<String> = row.try_get("", "group_ids").map_err(storage)?;
            let group_ids = parse_group_ids_json(raw.as_deref(), "billing_plans.group_ids")
                .map_err(GroupStoreError::Storage)?;
            if !group_ids.iter().any(|gid| gid == id) {
                continue;
            }
            let remaining: Vec<String> = group_ids.into_iter().filter(|gid| gid != id).collect();
            tx.execute(self.db.stmt(
                "UPDATE billing_plans SET group_ids = $1 WHERE id = $2",
                vec![
                    serde_json::to_string(&remaining).map_err(storage)?.into(),
                    row_id.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        }

        let result = tx
            .execute(
                self.db
                    .stmt("DELETE FROM monoize_groups WHERE id = $1", vec![id.into()]),
            )
            .await
            .map_err(storage)?;
        if result.rows_affected() != 1 {
            return Err(GroupStoreError::NotFound);
        }

        tx.commit().await.map_err(storage)?;
        self.api_key_cache.invalidate_all();
        crate::monoize_routing::bump_registry_generation();
        Ok(())
    }

    async fn group_name_exists(
        &self,
        exclude_id: Option<&str>,
        name: &str,
    ) -> Result<bool, GroupStoreError> {
        let (sql, values): (&str, Vec<SeaValue>) = match exclude_id {
            Some(id) => (
                "SELECT COUNT(*) AS cnt FROM monoize_groups WHERE lower(trim(name)) = lower(trim($1)) AND id != $2",
                vec![name.into(), id.into()],
            ),
            None => (
                "SELECT COUNT(*) AS cnt FROM monoize_groups WHERE lower(trim(name)) = lower(trim($1))",
                vec![name.into()],
            ),
        };
        let row = self
            .db
            .read()
            .query_one(self.db.stmt(sql, values))
            .await
            .map_err(storage)?
            .ok_or_else(|| GroupStoreError::Storage("count query returned no row".to_string()))?;
        let count: i64 = row.try_get("", "cnt").map_err(storage)?;
        Ok(count > 0)
    }
}

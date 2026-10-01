use super::UserStore;
use chrono::Utc;
use sea_orm::{ConnectionTrait, QueryResult, Value as SeaValue};
use serde::{Deserialize, Serialize};

const TITLE_MAX_CHARS: usize = 200;
const CONTENT_MAX_CHARS: usize = 5000;
pub const ANNOUNCEMENT_TYPES: [&str; 4] = ["info", "success", "warning", "error"];

#[derive(Debug, Clone, Serialize)]
pub struct Announcement {
    pub id: String,
    pub title: String,
    pub content: String,
    #[serde(rename = "type")]
    pub announcement_type: String,
    pub pinned: bool,
    pub enabled: bool,
    pub created_at: String,
    pub created_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_read: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementListResult {
    pub announcements: Vec<Announcement>,
    pub unread_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateAnnouncementInput {
    pub title: String,
    pub content: String,
    #[serde(default = "default_type")]
    pub announcement_type: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAnnouncementInput {
    pub title: Option<String>,
    pub content: Option<String>,
    pub announcement_type: Option<String>,
    pub pinned: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct MarkAnnouncementsReadInput {
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnouncementStoreError {
    NotFound,
    InvalidTitle,
    InvalidContent,
    InvalidType,
    Storage(String),
}

fn default_type() -> String {
    "info".to_string()
}

fn default_enabled() -> bool {
    true
}

fn storage(error: impl std::fmt::Display) -> AnnouncementStoreError {
    AnnouncementStoreError::Storage(error.to_string())
}

impl std::fmt::Display for AnnouncementStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(formatter, "announcement not found"),
            Self::InvalidTitle => write!(formatter, "announcement title must be 1-200 characters"),
            Self::InvalidContent => {
                write!(formatter, "announcement content must be 1-5000 characters")
            }
            Self::InvalidType => write!(
                formatter,
                "announcement type must be info, success, warning, or error"
            ),
            Self::Storage(message) => write!(formatter, "announcement storage error: {message}"),
        }
    }
}

fn storage_message(error: AnnouncementStoreError) -> String {
    error.to_string()
}

pub fn validate_announcement_title(raw: &str) -> Result<String, AnnouncementStoreError> {
    let title = raw.trim().to_string();
    if title.is_empty() || title.chars().count() > TITLE_MAX_CHARS {
        return Err(AnnouncementStoreError::InvalidTitle);
    }
    Ok(title)
}

pub fn validate_announcement_content(raw: &str) -> Result<String, AnnouncementStoreError> {
    let content = raw.trim().to_string();
    if content.is_empty() || content.chars().count() > CONTENT_MAX_CHARS {
        return Err(AnnouncementStoreError::InvalidContent);
    }
    Ok(content)
}

pub fn validate_announcement_type(raw: &str) -> Result<String, AnnouncementStoreError> {
    if ANNOUNCEMENT_TYPES.contains(&raw) {
        Ok(raw.to_string())
    } else {
        Err(AnnouncementStoreError::InvalidType)
    }
}

const ANNOUNCEMENT_COLUMNS: &str =
    "id, title, content, type, pinned, enabled, created_at, created_by";

fn row_to_announcement(row: &QueryResult) -> Result<Announcement, AnnouncementStoreError> {
    Ok(Announcement {
        id: row.try_get("", "id").map_err(storage)?,
        title: row.try_get("", "title").map_err(storage)?,
        content: row.try_get("", "content").map_err(storage)?,
        announcement_type: row.try_get("", "type").map_err(storage)?,
        pinned: row.try_get::<i32>("", "pinned").map_err(storage)? != 0,
        enabled: row.try_get::<i32>("", "enabled").map_err(storage)? != 0,
        created_at: row.try_get("", "created_at").map_err(storage)?,
        created_by: row.try_get::<String>("", "created_by").ok(),
        is_read: None,
    })
}

impl UserStore {
    /// AN-4: enabled announcements for a user with their read state.
    pub async fn list_announcements_for_user(
        &self,
        user_id: &str,
        limit: i64,
    ) -> Result<AnnouncementListResult, String> {
        let rows = self.db.read()
            .query_all(self.db.stmt(
                "SELECT a.id, a.title, a.content, a.type, a.pinned, a.enabled, a.created_at, a.created_by, \
                 CASE WHEN r.announcement_id IS NOT NULL THEN 1 ELSE 0 END AS is_read \
                 FROM announcements a \
                 LEFT JOIN announcement_reads r ON r.announcement_id = a.id AND r.user_id = $2 \
                 WHERE a.enabled = 1 \
                 ORDER BY a.pinned DESC, a.created_at DESC, a.id ASC LIMIT $1",
                vec![limit.into(), user_id.to_string().into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        let mut announcements = Vec::new();
        for row in &rows {
            let mut announcement = row_to_announcement(row).map_err(storage_message)?;
            announcement.is_read = Some(
                row.try_get::<i32>("", "is_read")
                    .map_err(|e| e.to_string())?
                    != 0,
            );
            announcements.push(announcement);
        }
        let unread_count: i64 = self
            .db
            .read()
            .query_one(self.db.stmt(
                "SELECT COUNT(*) AS unread FROM announcements a \
                 WHERE a.enabled = 1 AND NOT EXISTS ( \
                     SELECT 1 FROM announcement_reads r \
                     WHERE r.announcement_id = a.id AND r.user_id = $1)",
                vec![user_id.to_string().into()],
            ))
            .await
            .map_err(|e| e.to_string())?
            .and_then(|row| row.try_get::<i64>("", "unread").ok())
            .unwrap_or(0);
        Ok(AnnouncementListResult {
            announcements,
            unread_count,
        })
    }

    /// AN-6: every announcement including disabled ones.
    pub async fn list_announcements_admin(&self) -> Result<Vec<Announcement>, String> {
        let rows = self
            .db
            .read()
            .query_all(self.db.stmt(
                &format!(
                    "SELECT {ANNOUNCEMENT_COLUMNS} FROM announcements \
                     ORDER BY pinned DESC, created_at DESC, id ASC"
                ),
                vec![],
            ))
            .await
            .map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| row_to_announcement(row).map_err(storage_message))
            .collect()
    }

    /// AN-7.
    pub async fn create_announcement(
        &self,
        input: CreateAnnouncementInput,
        created_by: &str,
    ) -> Result<Announcement, AnnouncementStoreError> {
        let title = validate_announcement_title(&input.title)?;
        let content = validate_announcement_content(&input.content)?;
        let announcement_type = validate_announcement_type(&input.announcement_type)?;
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339();
        let pinned = i32::from(input.pinned);
        let enabled = i32::from(input.enabled);
        self.db.write().await
            .execute(self.db.stmt(
                "INSERT INTO announcements (id, title, content, type, pinned, enabled, created_at, created_by) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                vec![
                    id.clone().into(),
                    title.clone().into(),
                    content.clone().into(),
                    announcement_type.clone().into(),
                    pinned.into(),
                    enabled.into(),
                    created_at.clone().into(),
                    created_by.to_string().into(),
                ],
            ))
            .await
            .map_err(storage)?;
        Ok(Announcement {
            id,
            title,
            content,
            announcement_type,
            pinned: input.pinned,
            enabled: input.enabled,
            created_at,
            created_by: Some(created_by.to_string()),
            is_read: None,
        })
    }

    /// AN-8.
    pub async fn update_announcement(
        &self,
        announcement_id: &str,
        input: UpdateAnnouncementInput,
    ) -> Result<Announcement, AnnouncementStoreError> {
        let mut title = None;
        if let Some(raw) = input.title.as_deref() {
            title = Some(validate_announcement_title(raw)?);
        }
        let mut content = None;
        if let Some(raw) = input.content.as_deref() {
            content = Some(validate_announcement_content(raw)?);
        }
        let mut announcement_type = None;
        if let Some(raw) = input.announcement_type.as_deref() {
            announcement_type = Some(validate_announcement_type(raw)?);
        }
        let pinned = input.pinned.map(i32::from);
        let enabled = input.enabled.map(i32::from);

        let txn = self.db.begin_write().await.map_err(storage)?;
        let updated = txn
            .query_one(self.db.stmt(
                "UPDATE announcements SET \
                     title = COALESCE($1, title), \
                     content = COALESCE($2, content), \
                     type = COALESCE($3, type), \
                     pinned = COALESCE($4, pinned), \
                     enabled = COALESCE($5, enabled) \
                 WHERE id = $6 \
                 RETURNING id, title, content, type, pinned, enabled, created_at, created_by",
                vec![
                    title
                        .map(|value| SeaValue::String(Some(Box::new(value))))
                        .unwrap_or(SeaValue::String(None)),
                    content
                        .map(|value| SeaValue::String(Some(Box::new(value))))
                        .unwrap_or(SeaValue::String(None)),
                    announcement_type
                        .map(|value| SeaValue::String(Some(Box::new(value))))
                        .unwrap_or(SeaValue::String(None)),
                    pinned
                        .map(|value| SeaValue::Int(Some(value)))
                        .unwrap_or(SeaValue::Int(None)),
                    enabled
                        .map(|value| SeaValue::Int(Some(value)))
                        .unwrap_or(SeaValue::Int(None)),
                    announcement_id.to_string().into(),
                ],
            ))
            .await
            .map_err(storage)?
            .ok_or(AnnouncementStoreError::NotFound)?;
        let announcement = row_to_announcement(&updated)?;
        txn.commit().await.map_err(storage)?;
        Ok(announcement)
    }

    /// AN-9: delete the announcement and its read rows in one transaction.
    pub async fn delete_announcement(
        &self,
        announcement_id: &str,
    ) -> Result<(), AnnouncementStoreError> {
        let txn = self.db.begin_write().await.map_err(storage)?;
        let deleted = txn
            .execute(self.db.stmt(
                "DELETE FROM announcements WHERE id = $1",
                vec![announcement_id.to_string().into()],
            ))
            .await
            .map_err(storage)?;
        if deleted.rows_affected() == 0 {
            return Err(AnnouncementStoreError::NotFound);
        }
        txn.execute(self.db.stmt(
            "DELETE FROM announcement_reads WHERE announcement_id = $1",
            vec![announcement_id.to_string().into()],
        ))
        .await
        .map_err(storage)?;
        txn.commit().await.map_err(storage)?;
        Ok(())
    }

    /// AN-5: mark the given announcements (or all enabled ones for an empty
    /// list) read for the user; idempotent.
    pub async fn mark_announcements_read(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<i64, String> {
        let read_at = Utc::now().to_rfc3339();
        let txn = self.db.begin_write().await.map_err(|e| e.to_string())?;
        if ids.is_empty() {
            txn.execute(self.db.stmt(
                "INSERT INTO announcement_reads (user_id, announcement_id, read_at) \
                 SELECT $1, a.id, $2 FROM announcements a WHERE a.enabled = 1 \
                 AND NOT EXISTS (SELECT 1 FROM announcement_reads r \
                     WHERE r.user_id = $1 AND r.announcement_id = a.id)",
                vec![user_id.to_string().into(), read_at.into()],
            ))
            .await
            .map_err(|e| e.to_string())?;
        } else {
            for id in ids {
                txn.execute(self.db.stmt(
                    "INSERT INTO announcement_reads (user_id, announcement_id, read_at) \
                     SELECT $1, a.id, $2 FROM announcements a \
                     WHERE a.id = $3 AND a.enabled = 1 \
                     AND NOT EXISTS (SELECT 1 FROM announcement_reads r \
                         WHERE r.user_id = $1 AND r.announcement_id = a.id)",
                    vec![
                        user_id.to_string().into(),
                        read_at.clone().into(),
                        id.clone().into(),
                    ],
                ))
                .await
                .map_err(|e| e.to_string())?;
            }
        }
        txn.commit().await.map_err(|e| e.to_string())?;

        self.db
            .read()
            .query_one(self.db.stmt(
                "SELECT COUNT(*) AS unread FROM announcements a \
                 WHERE a.enabled = 1 AND NOT EXISTS ( \
                     SELECT 1 FROM announcement_reads r \
                     WHERE r.announcement_id = a.id AND r.user_id = $1)",
                vec![user_id.to_string().into()],
            ))
            .await
            .map_err(|e| e.to_string())?
            .and_then(|row| row.try_get::<i64>("", "unread").ok())
            .ok_or_else(|| "unread count query failed".to_string())
    }
}

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbBackend,
    DbErr, Statement, TransactionTrait, Value,
};
use sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub struct WriteGuard<'a> {
    conn: &'a DatabaseConnection,
    _guard: Option<tokio::sync::MutexGuard<'a, ()>>,
}

impl Deref for WriteGuard<'_> {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        self.conn
    }
}

pub struct WriteTransaction {
    txn: Option<DatabaseTransaction>,
    #[allow(dead_code)]
    _guard: Option<Box<tokio::sync::OwnedMutexGuard<()>>>,
}

impl WriteTransaction {
    pub async fn commit(mut self) -> Result<(), DbErr> {
        if let Some(txn) = self.txn.take() {
            txn.commit().await?;
        }
        Ok(())
    }

    pub async fn rollback(mut self) -> Result<(), DbErr> {
        if let Some(txn) = self.txn.take() {
            txn.rollback().await?;
        }
        Ok(())
    }
}

impl Deref for WriteTransaction {
    type Target = DatabaseTransaction;
    fn deref(&self) -> &Self::Target {
        self.txn.as_ref().expect("transaction already consumed")
    }
}

/// Wraps a pair of Sea ORM connections: one for writes (single-connection for SQLite,
/// standard pool for PostgreSQL) and one for reads (10-connection pool for SQLite,
/// shared with write pool for PostgreSQL).
///
/// For SQLite, all write access is serialized through a tokio Mutex to prevent
/// concurrent write failures and billing bypass via race conditions.
#[derive(Debug, Clone)]
pub struct DbPool {
    read: DatabaseConnection,
    write_conn: DatabaseConnection,
    write_lock: Arc<Mutex<()>>,
    backend: DbBackend,
    sqlite_filesystem_id: Option<Arc<str>>,
    accounting: Arc<crate::accounting::state::AccountingCore>,
}

fn positive_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn positive_env_u32(name: &str, default: u32) -> u32 {
    positive_env_u64(name, default as u64) as u32
}

impl DbPool {
    /// Create a new DbPool from a database DSN.
    ///
    /// For SQLite DSNs (starting with "sqlite://"):
    ///   - Creates a write pool with max 1 connection (single-writer)
    ///   - Creates a read pool with max 10 connections
    ///   - Applies WAL mode and connection-local PRAGMAs, including a 15s busy timeout
    ///
    /// For PostgreSQL DSNs (starting with "postgres://" or "postgresql://"):
    ///   - Creates a single connection pool used for both reads and writes
    ///   - Default pool settings from Sea ORM
    pub async fn connect(dsn: &str) -> Result<Self, DbErr> {
        let dsn = dsn.trim();
        if dsn.starts_with("sqlite://") || dsn.starts_with("sqlite::memory:") {
            Self::connect_sqlite(dsn).await
        } else if dsn.starts_with("postgres://") || dsn.starts_with("postgresql://") {
            Self::connect_postgres(dsn).await
        } else {
            Err(DbErr::Custom(format!(
                "unsupported database DSN scheme: {dsn}"
            )))
        }
    }

    async fn connect_sqlite(dsn: &str) -> Result<Self, DbErr> {
        ensure_sqlite_file(dsn).map_err(DbErr::Custom)?;

        if is_sqlite_memory_dsn(dsn) {
            let opts = Self::sqlite_connect_options(dsn, 1);
            let conn = Database::connect(opts).await?;
            return Ok(Self {
                read: conn.clone(),
                write_conn: conn,
                write_lock: Arc::new(Mutex::new(())),
                backend: DbBackend::Sqlite,
                sqlite_filesystem_id: Some(format!("memory:{}", uuid::Uuid::new_v4()).into()),
                accounting: Arc::new(crate::accounting::state::AccountingCore::legacy()),
            });
        }

        let base_dsn = if dsn.contains('?') {
            dsn.to_string()
        } else {
            format!("{dsn}?mode=rwc")
        };

        let read_connections = positive_env_u32("MONOIZE_SQLITE_READ_CONNECTIONS", 4);
        let write_opts = Self::sqlite_connect_options(&base_dsn, 1);
        let read_opts = Self::sqlite_connect_options(&base_dsn, read_connections);

        let write = Database::connect(write_opts).await?;
        let read = Database::connect(read_opts).await?;
        let sqlite_filesystem_id = sqlite_filesystem_id(&write).await?;

        Ok(Self {
            read,
            write_conn: write,
            write_lock: Arc::new(Mutex::new(())),
            backend: DbBackend::Sqlite,
            sqlite_filesystem_id: Some(sqlite_filesystem_id.into()),
            accounting: Arc::new(crate::accounting::state::AccountingCore::legacy()),
        })
    }

    fn sqlite_connect_options(dsn: &str, max_connections: u32) -> ConnectOptions {
        let mut opts = ConnectOptions::new(dsn);
        opts.max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .sqlx_logging(false);
        // DB26: per-connection page cache and mmap window are env-tunable so a
        // co-located deployment can bound the pool's aggregate memory.
        let cache_kib = positive_env_u64("MONOIZE_SQLITE_CACHE_KIB", 16384);
        let mmap_bytes = positive_env_u64("MONOIZE_SQLITE_MMAP_BYTES", 67108864);
        opts.map_sqlx_sqlite_opts(move |opts| {
            opts.journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .busy_timeout(Duration::from_secs(15))
                .foreign_keys(true)
                .pragma("cache_size", format!("-{cache_kib}"))
                .pragma("mmap_size", mmap_bytes.to_string())
        });
        opts
    }

    async fn connect_postgres(dsn: &str) -> Result<Self, DbErr> {
        let opts = ConnectOptions::new(dsn)
            .max_connections(20)
            .acquire_timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .sqlx_logging(false)
            .to_owned();

        let conn = Database::connect(opts).await?;

        Ok(Self {
            read: conn.clone(),
            write_conn: conn,
            write_lock: Arc::new(Mutex::new(())),
            backend: DbBackend::Postgres,
            sqlite_filesystem_id: None,
            accounting: Arc::new(crate::accounting::state::AccountingCore::legacy()),
        })
    }

    /// Get the read connection (for SELECT queries).
    pub fn read(&self) -> &DatabaseConnection {
        &self.read
    }

    pub(crate) fn accounting_core(&self) -> Arc<crate::accounting::state::AccountingCore> {
        self.accounting.clone()
    }

    /// Render one accounting column name for the current schema layout. An
    /// activation writer holding the core lock keeps callers on the legacy
    /// layout; monetary operations cannot run inside activation anyway.
    pub fn accounting_column<'a>(&self, legacy_name: &'a str) -> &'a str {
        let layout = self
            .accounting
            .layout()
            .unwrap_or(crate::accounting::schema::SchemaLayout::LegacyUsd);
        crate::accounting::schema::column(layout, legacy_name)
    }

    pub fn accounting(&self) -> crate::accounting::state::AccountingState {
        crate::accounting::state::AccountingState::for_pool(self.clone())
    }

    /// Acquire the write connection. For SQLite, this serializes all writes
    /// through a tokio Mutex to prevent concurrent write failures.
    /// For PostgreSQL, the returned guard holds no lock (no-op).
    pub async fn write(&self) -> WriteGuard<'_> {
        if self.backend == DbBackend::Sqlite {
            let guard = self.write_lock.lock().await;
            WriteGuard {
                conn: &self.write_conn,
                _guard: Some(guard),
            }
        } else {
            WriteGuard {
                conn: &self.write_conn,
                _guard: None,
            }
        }
    }

    /// Get the database backend type.
    pub fn backend(&self) -> DbBackend {
        self.backend
    }

    /// Check if this is a SQLite backend.
    pub fn is_sqlite(&self) -> bool {
        self.backend == DbBackend::Sqlite
    }

    /// Check if this is a PostgreSQL backend.
    pub fn is_postgres(&self) -> bool {
        self.backend == DbBackend::Postgres
    }

    pub(crate) fn sqlite_filesystem_id(&self) -> Option<&str> {
        self.sqlite_filesystem_id.as_deref()
    }

    /// Acquire write connection and begin an explicit transaction.
    pub async fn begin_write(&self) -> Result<WriteTransaction, DbErr> {
        let guard = if self.backend == DbBackend::Sqlite {
            Some(Box::new(self.write_lock.clone().lock_owned().await))
        } else {
            None
        };
        let txn = self.write_conn.begin().await?;
        Ok(WriteTransaction {
            txn: Some(txn),
            _guard: guard,
        })
    }

    pub async fn with_immediate_write<T, E, F>(&self, operation: F) -> Result<T, E>
    where
        E: From<DbErr>,
        F: for<'a> FnOnce(
            &'a DatabaseTransaction,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>,
    {
        if !self.is_sqlite() {
            return Err(E::from(DbErr::Custom(
                "serialized SQLite write transactions are available only for SQLite".to_string(),
            )));
        }
        let guard = self.write_lock.clone().lock_owned().await;
        self.write_conn
            .execute_unprepared("PRAGMA busy_timeout = 5000")
            .await
            .map_err(E::from)?;

        // Keep the transaction object alive across the entire operation. Using
        // separate DatabaseConnection calls lets SQLx return the pooled SQLite
        // connection between BEGIN and COMMIT, which can make COMMIT observe no
        // active transaction.
        let txn = match self.write_conn.begin().await {
            Ok(txn) => txn,
            Err(error) => {
                let _ = self
                    .write_conn
                    .execute_unprepared("PRAGMA busy_timeout = 15000")
                    .await;
                return Err(E::from(error));
            }
        };
        let transaction = WriteTransaction {
            txn: Some(txn),
            _guard: Some(Box::new(guard)),
        };
        // SeaORM begins deferred SQLite transactions. Acquire the database-wide
        // writer lock before the callback reads a snapshot; a process-local mutex
        // alone cannot serialize blue-green processes. No migration row changes.
        let outcome = match transaction
            .execute_unprepared("UPDATE seaql_migrations SET version = version WHERE 0")
            .await
        {
            Ok(_) => operation(&*transaction).await,
            Err(error) => Err(E::from(error)),
        };
        match outcome {
            Ok(value) => {
                let result = transaction.commit().await;
                if let Err(error) = &result {
                    match self.write_conn.ping().await {
                        Ok(()) => {
                            tracing::warn!(error = %error, "SQLite transaction commit failed; connection remains responsive")
                        }
                        Err(ping_error) => {
                            tracing::error!(error = %error, ping_error = %ping_error, "SQLite transaction commit failed and connection health check failed")
                        }
                    }
                }
                if let Err(error) = self
                    .write_conn
                    .execute_unprepared("PRAGMA busy_timeout = 15000")
                    .await
                {
                    tracing::error!(error = %error, "failed to restore SQLite busy timeout after commit");
                }
                result.map_err(E::from)?;
                Ok(value)
            }
            Err(error) => {
                let result = transaction.rollback().await;
                if let Err(rollback_error) = &result {
                    match self.write_conn.ping().await {
                        Ok(()) => {
                            tracing::warn!(error = %rollback_error, "SQLite transaction rollback failed; connection remains responsive")
                        }
                        Err(ping_error) => {
                            tracing::error!(error = %rollback_error, ping_error = %ping_error, "SQLite transaction rollback failed and connection health check failed")
                        }
                    }
                }
                if let Err(restore_error) = self
                    .write_conn
                    .execute_unprepared("PRAGMA busy_timeout = 15000")
                    .await
                {
                    tracing::error!(error = %restore_error, "failed to restore SQLite busy timeout after rollback");
                }
                result.map_err(E::from)?;
                Err(error)
            }
        }
    }

    pub(crate) async fn with_sqlite_quota_probe<T, E, F>(&self, operation: F) -> Result<T, E>
    where
        E: From<DbErr>,
        F: for<'a> FnOnce(
            &'a DatabaseConnection,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>,
    {
        if !self.is_sqlite() {
            return Err(E::from(DbErr::Custom(
                "SQLite quota probe requires a SQLite database".to_string(),
            )));
        }
        let _guard = self.write_lock.clone().lock_owned().await;
        self.write_conn
            .execute_unprepared("PRAGMA busy_timeout = 5000")
            .await
            .map_err(E::from)?;
        let outcome = operation(&self.write_conn).await;
        let restored = self
            .write_conn
            .execute_unprepared("PRAGMA busy_timeout = 15000")
            .await;
        match (outcome, restored) {
            (_, Err(error)) => Err(E::from(error)),
            (outcome, Ok(_)) => outcome,
        }
    }

    /// Create a Statement with automatic placeholder conversion.
    /// Write SQL with $1, $2, ... placeholders.
    /// For SQLite, $N placeholders are auto-converted to numbered ?N placeholders.
    pub fn stmt(&self, sql: &str, values: Vec<Value>) -> Statement {
        if self.backend == DbBackend::Sqlite {
            let mut result = String::with_capacity(sql.len());
            let mut chars = sql.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '$' && chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                    result.push('?');
                    while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                        result.push(chars.next().expect("peeked placeholder digit"));
                    }
                } else {
                    result.push(ch);
                }
            }
            Statement::from_sql_and_values(DbBackend::Sqlite, result, values)
        } else {
            Statement::from_sql_and_values(self.backend, sql, values)
        }
    }
}

async fn sqlite_filesystem_id(connection: &DatabaseConnection) -> Result<String, DbErr> {
    let rows = connection
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA database_list".to_string(),
        ))
        .await?;
    let path = rows
        .iter()
        .find_map(|row| {
            let name = row.try_get::<String>("", "name").ok()?;
            (name == "main")
                .then(|| row.try_get::<String>("", "file").ok())
                .flatten()
        })
        .filter(|path| !path.is_empty())
        .ok_or_else(|| DbErr::Custom("SQLite main database path is unavailable".to_string()))?;
    filesystem_id_for_path(std::path::Path::new(&path)).map_err(|error| {
        DbErr::Custom(format!(
            "SQLite filesystem identity is unavailable: {error}"
        ))
    })
}

#[cfg(unix)]
fn filesystem_id_for_path(path: &std::path::Path) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt as _;

    Ok(format!("unix-dev:{:x}", std::fs::metadata(path)?.dev()))
}

#[cfg(windows)]
fn filesystem_id_for_path(path: &std::path::Path) -> std::io::Result<String> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let file = std::fs::File::open(path)?;
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let succeeded = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(format!(
        "windows-volume:{:08x}",
        information.dwVolumeSerialNumber
    ))
}

#[cfg(not(any(unix, windows)))]
fn filesystem_id_for_path(_path: &std::path::Path) -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "SQLite filesystem identity is unsupported on this platform",
    ))
}

fn is_sqlite_memory_dsn(dsn: &str) -> bool {
    let dsn = dsn.trim();
    dsn.starts_with("sqlite::memory:") || dsn.contains(":memory:") || dsn.contains("mode=memory")
}

fn ensure_sqlite_file(dsn: &str) -> Result<(), String> {
    let dsn = dsn.trim();
    if !dsn.starts_with("sqlite://") {
        return Ok(());
    }
    if dsn.contains(":memory:") || dsn.contains("mode=memory") {
        return Ok(());
    }
    let path_part = dsn.trim_start_matches("sqlite://");
    let path_part = path_part.split('?').next().unwrap_or("");
    if path_part.is_empty() {
        return Ok(());
    }
    let path = std::path::PathBuf::from(path_part);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("sqlite_dir_create_failed: {err}"))?;
        }
    }
    if !path.exists() {
        std::fs::File::create(&path).map_err(|err| format!("sqlite_file_create_failed: {err}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::DbPool;
    use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};

    #[tokio::test]
    async fn sqlite_numbered_placeholders_preserve_repeated_bind_semantics() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        let row = db
            .read()
            .query_one(db.stmt("SELECT $1 || ':' || $1 AS bound_value", vec!["same".into()]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<String>("", "bound_value").unwrap(),
            "same:same"
        );
    }

    #[tokio::test]
    async fn sqlite_numbered_placeholders_preserve_out_of_order_bind_semantics() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        let row = db
            .read()
            .query_one(db.stmt(
                "SELECT $2 || $1 || $2 AS bound_value",
                vec!["A".into(), "B".into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<String>("", "bound_value").unwrap(), "BAB");
    }

    #[tokio::test]
    async fn every_sqlite_read_pool_connection_receives_required_pragmas() {
        let db_path = std::path::PathBuf::from("target/test-databases")
            .join(format!("pragma-{}.sqlite", uuid::Uuid::new_v4()));
        let dsn = format!("sqlite://{}", db_path.display());
        let db = DbPool::connect(&dsn).await.unwrap();
        // DB26: the pool shape is env-tuned; force exactly the configured number
        // of pinned transactions so every pooled connection is observed.
        let expected_cache_size: i64 = -(std::env::var("MONOIZE_SQLITE_CACHE_KIB")
            .ok()
            .and_then(|raw| raw.trim().parse::<i64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(16384));
        let expected_mmap: i64 = std::env::var("MONOIZE_SQLITE_MMAP_BYTES")
            .ok()
            .and_then(|raw| raw.trim().parse::<i64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(67_108_864);
        let pool_size =
            std::env::var("MONOIZE_SQLITE_READ_CONNECTIONS")
                .ok()
                .and_then(|raw| raw.trim().parse::<i64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(4);
        let mut transactions = Vec::new();
        let mut observed = Vec::new();

        for _ in 0..pool_size {
            let txn = db.read().begin().await.unwrap();
            let journal: String = txn
                .query_one(db.stmt("PRAGMA journal_mode", vec![]))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "journal_mode")
                .unwrap();
            let busy_timeout: i64 = txn
                .query_one(db.stmt("PRAGMA busy_timeout", vec![]))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "timeout")
                .unwrap();
            let foreign_keys: i64 = txn
                .query_one(db.stmt("PRAGMA foreign_keys", vec![]))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "foreign_keys")
                .unwrap();
            let synchronous: i64 = txn
                .query_one(db.stmt("PRAGMA synchronous", vec![]))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "synchronous")
                .unwrap();
            let cache_size: i64 = txn
                .query_one(db.stmt("PRAGMA cache_size", vec![]))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "cache_size")
                .unwrap();
            let mmap_size: i64 = txn
                .query_one(db.stmt("PRAGMA mmap_size", vec![]))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "mmap_size")
                .unwrap();
            observed.push((
                journal,
                busy_timeout,
                foreign_keys,
                synchronous,
                cache_size,
                mmap_size,
            ));
            transactions.push(txn);
        }

        for (journal, busy_timeout, foreign_keys, synchronous, cache_size, mmap_size) in observed {
            assert_eq!(journal, "wal");
            assert_eq!(busy_timeout, 15_000);
            assert_eq!(foreign_keys, 1);
            assert_eq!(synchronous, 1);
            assert_eq!(cache_size, expected_cache_size);
            assert_eq!(mmap_size, expected_mmap);
        }
        for txn in transactions {
            txn.rollback().await.unwrap();
        }

        drop(db);
        for path in [
            db_path.clone(),
            db_path.with_extension("sqlite-wal"),
            db_path.with_extension("sqlite-shm"),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[tokio::test]
    async fn immediate_write_rolls_back_errors_and_restores_busy_timeout() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        db.write()
            .await
            .execute_unprepared(
                "CREATE TABLE seaql_migrations (version TEXT PRIMARY KEY, applied_at BIGINT NOT NULL)",
            )
            .await
            .unwrap();
        db.write()
            .await
            .execute_unprepared("CREATE TABLE immediate_probe (value INTEGER NOT NULL)")
            .await
            .unwrap();

        let probe = db.clone();
        let observed: Result<i64, DbErr> = db
            .with_immediate_write(move |connection| {
                Box::pin(async move {
                    connection
                        .execute_unprepared("INSERT INTO immediate_probe VALUES (1)")
                        .await?;
                    let row = connection
                        .query_one(probe.stmt("PRAGMA busy_timeout", vec![]))
                        .await?
                        .unwrap();
                    row.try_get("", "timeout")
                })
            })
            .await;
        assert_eq!(observed.unwrap(), 5_000);

        let failed: Result<(), DbErr> = db
            .with_immediate_write(|connection| {
                Box::pin(async move {
                    connection
                        .execute_unprepared("INSERT INTO immediate_probe VALUES (2)")
                        .await?;
                    Err(DbErr::Custom("expected failure".to_string()))
                })
            })
            .await;
        assert!(matches!(failed, Err(DbErr::Custom(message)) if message == "expected failure"));

        let row = db
            .read()
            .query_one(db.stmt("SELECT COUNT(*) AS value FROM immediate_probe", vec![]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i64>("", "value").unwrap(), 1);
        let write = db.write().await;
        let busy = write
            .query_one(db.stmt("PRAGMA busy_timeout", vec![]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(busy.try_get::<i64>("", "timeout").unwrap(), 15_000);
    }

    #[tokio::test]
    async fn independent_sqlite_writer_waits_before_reading_application_state() {
        let directory = tempfile::tempdir().unwrap();
        let dsn = format!("sqlite://{}", directory.path().join("writers.db").display());
        let primary = DbPool::connect(&dsn).await.unwrap();
        primary
            .write()
            .await
            .execute_unprepared(
                "CREATE TABLE seaql_migrations (version TEXT PRIMARY KEY, applied_at BIGINT NOT NULL);
                 CREATE TABLE writer_probe (value INTEGER NOT NULL);
                 INSERT INTO writer_probe VALUES (0)",
            )
            .await
            .unwrap();
        let candidate = DbPool::connect(&dsn).await.unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let holder = tokio::spawn(async move {
            primary
                .with_immediate_write::<_, DbErr, _>(move |connection| {
                    Box::pin(async move {
                        entered_tx.send(()).unwrap();
                        release_rx.await.unwrap();
                        connection
                            .execute_unprepared("UPDATE writer_probe SET value = 1")
                            .await?;
                        Ok(())
                    })
                })
                .await
        });
        entered_rx.await.unwrap();
        let (observed_tx, mut observed_rx) = tokio::sync::oneshot::channel();
        let candidate_db = candidate.clone();
        let contender = tokio::spawn(async move {
            candidate
                .with_immediate_write::<_, DbErr, _>(move |connection| {
                    Box::pin(async move {
                        let row = connection
                            .query_one(candidate_db.stmt("SELECT value FROM writer_probe", vec![]))
                            .await?
                            .unwrap();
                        observed_tx.send(row.try_get::<i64>("", "value")?).unwrap();
                        Ok(())
                    })
                })
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut observed_rx)
                .await
                .is_err(),
            "a second pool must wait for the writer before reading its snapshot"
        );
        release_tx.send(()).unwrap();
        holder.await.unwrap().unwrap();
        contender.await.unwrap().unwrap();
        assert_eq!(observed_rx.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn cancelled_immediate_write_releases_the_shared_database_lock() {
        let directory = tempfile::tempdir().unwrap();
        let dsn = format!("sqlite://{}", directory.path().join("cancel.db").display());
        let primary = DbPool::connect(&dsn).await.unwrap();
        primary
            .write()
            .await
            .execute_unprepared(
                "CREATE TABLE seaql_migrations (version TEXT PRIMARY KEY, applied_at BIGINT NOT NULL)",
            )
            .await
            .unwrap();
        let candidate = DbPool::connect(&dsn).await.unwrap();
        let held_db = primary.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let holder = tokio::spawn(async move {
            held_db
                .with_immediate_write::<(), DbErr, _>(move |_connection| {
                    Box::pin(async move {
                        entered_tx.send(()).unwrap();
                        std::future::pending().await
                    })
                })
                .await
        });
        entered_rx.await.unwrap();
        holder.abort();
        assert!(holder.await.unwrap_err().is_cancelled());
        for db in [&primary, &candidate] {
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                db.with_immediate_write::<(), DbErr, _>(|_| Box::pin(async { Ok(()) })),
            )
            .await
            .expect("cancelled writer must release the connection and database lock")
            .unwrap();
        }
    }

    #[tokio::test]
    async fn quota_probe_uses_five_seconds_and_restores_fifteen_seconds() {
        let db = DbPool::connect("sqlite::memory:").await.unwrap();
        let probe = db.clone();
        let observed: Result<i64, DbErr> = db
            .with_sqlite_quota_probe(move |connection| {
                Box::pin(async move {
                    connection
                        .query_one(probe.stmt("PRAGMA busy_timeout", vec![]))
                        .await?
                        .unwrap()
                        .try_get("", "timeout")
                })
            })
            .await;
        assert_eq!(observed.unwrap(), 5_000);

        let write = db.write().await;
        let restored = write
            .query_one(db.stmt("PRAGMA busy_timeout", vec![]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.try_get::<i64>("", "timeout").unwrap(), 15_000);
    }

    #[tokio::test]
    async fn memory_identity_is_stable_across_clones_and_changes_on_reconnect() {
        let first = DbPool::connect("sqlite::memory:").await.unwrap();
        let cloned = first.clone();
        let second = DbPool::connect("sqlite::memory:").await.unwrap();

        let first_identity = first.sqlite_filesystem_id().unwrap();
        assert!(first_identity.starts_with("memory:"));
        assert_eq!(first_identity, cloned.sqlite_filesystem_id().unwrap());
        assert_ne!(first_identity, second.sqlite_filesystem_id().unwrap());
    }
}

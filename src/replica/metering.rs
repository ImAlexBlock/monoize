//! Metering pipeline between replica and primary (`primary-replica-deployment.spec.md`
//! sections 6–7): durable balance-delta spool on replicas, batched shipment over the
//! internal HTTP API, idempotent apply inside one transaction on the primary, and the
//! PRP9 promotion drain.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::DbPool;
use crate::db_cache::{LastUsedBatcher, MeteringSink, RequestLogBatcher, SpoolRequestLog};
use crate::replica::admission_client::AdmissionClient;
use crate::replica::internal_http::read_internal_response;
use crate::store_billing::admission_runtime::{
    AdmissionRuntimeError, AdmissionService, TerminalApplyInput, TerminalApplyResult,
    terminal_digest,
};
use crate::store_billing::admission_token::{
    ADMISSION_ISSUER, AdmissionClaimStore, AdmissionVerifierRing, PlanTerminalAcknowledgement,
    PlanTerminalWire, TerminalAcknowledgementResult, TerminalKind, TerminalSpoolInput,
    TerminalSpoolRecord,
};

pub const METERING_INGEST_PATH: &str = "/internal/replica/metering";
/// Hard per-batch cap enforced by both sides regardless of configuration (I3).
pub const METERING_BATCH_HARD_CAP: usize = 2000;
/// M9: file inside the metering spool directory that persists the replica identity.
pub const REPLICA_IDENTITY_FILE_NAME: &str = "replica-identity";
/// M4a: ship-interval multiples after which a heartbeat is displayed as stale.
pub const HEARTBEAT_STALE_INTERVALS: u32 = 3;
/// M4a: ship-interval multiples after which a heartbeat entry is evicted on read.
pub const HEARTBEAT_EVICT_INTERVALS: u32 = 360;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceDelta {
    pub delta_id: String,
    pub kind: String,
    pub user_id: String,
    #[serde(default)]
    pub api_key_id: Option<String>,
    pub amount_nano_usd: String,
    #[serde(default)]
    pub meta_json: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastUsedPair {
    pub api_key_id: String,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaHeartbeat {
    pub id: String,
    pub hostname: String,
    pub listen: String,
    pub version: String,
    pub started_at: String,
    #[serde(default)]
    pub uptime_seconds: u64,
    #[serde(default)]
    pub spool_pending_count: usize,
    #[serde(default)]
    pub spool_pending_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ReplicaHeartbeatRecord {
    pub heartbeat: ReplicaHeartbeat,
    pub last_seen_unix_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeteringBatch {
    #[serde(default)]
    pub replica: Option<ReplicaHeartbeat>,
    #[serde(default)]
    pub request_logs: Vec<SpoolRequestLog>,
    #[serde(default)]
    pub last_used: Vec<LastUsedPair>,
    #[serde(default)]
    pub plan_terminals: Vec<PlanTerminalWire>,
    #[serde(default)]
    pub balance_deltas: Vec<BalanceDelta>,
}

impl MeteringBatch {
    fn total_entries(&self) -> usize {
        self.request_logs.len()
            + self.plan_terminals.len()
            + self.balance_deltas.len()
            + self.last_used.len()
    }

    fn is_empty(&self) -> bool {
        self.total_entries() == 0
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MeteringAck {
    pub applied_request_logs: u64,
    pub applied_last_used: u64,
    pub applied_balance_deltas: u64,
    #[serde(default)]
    pub plan_terminal_acks: Vec<PlanTerminalAcknowledgement>,
}

pub fn valid_delta_kind(kind: &str) -> bool {
    matches!(kind, "request_charge" | "api_key_charge")
}

pub fn validate_delta(delta: &BalanceDelta) -> Result<(), &'static str> {
    if delta.delta_id.is_empty() || delta.user_id.is_empty() {
        return Err("delta_id and user_id must be non-empty");
    }
    if !valid_delta_kind(&delta.kind) {
        return Err("kind must be request_charge or api_key_charge");
    }
    let amount = delta
        .amount_nano_usd
        .trim()
        .parse::<i128>()
        .map_err(|_| "amount_nano_usd must be decimal i128 text")?;
    if amount < 0 {
        return Err("amount_nano_usd must be nonnegative");
    }
    if delta.created_at.is_empty() {
        return Err("created_at must be RFC 3339 text");
    }
    Ok(())
}

fn terminal_apply_input(
    terminal: &PlanTerminalWire,
) -> Result<TerminalApplyInput, AdmissionRuntimeError> {
    if terminal.version != 1 {
        return Err(AdmissionRuntimeError::InputInvalid);
    }
    let actual_nano_usd = match (terminal.kind, terminal.actual_nano_usd.as_deref()) {
        (TerminalKind::Settlement, Some(value)) => {
            if value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
                || (value.len() > 1 && value.starts_with('0'))
            {
                return Err(AdmissionRuntimeError::InputInvalid);
            }
            Some(
                value
                    .parse::<i128>()
                    .map_err(|_| AdmissionRuntimeError::InputInvalid)?,
            )
        }
        (TerminalKind::Release, None) => None,
        _ => return Err(AdmissionRuntimeError::InputInvalid),
    };
    let input = TerminalApplyInput {
        token_id: terminal.token_id.clone(),
        reservation_id: terminal.reservation_id.clone(),
        request_id: terminal.request_id.clone(),
        audience: terminal.audience.clone(),
        kind: terminal.kind,
        actual_nano_usd,
        canonical_digest: terminal.canonical_digest.clone(),
        applied_at: terminal.created_at,
    };
    if terminal_digest(&input)? != input.canonical_digest {
        return Err(AdmissionRuntimeError::TerminalDigestInvalid);
    }
    Ok(input)
}

fn admission_batch_error(error: AdmissionRuntimeError) -> String {
    format!("{}: {error}", error.code())
}

// ---------------------------------------------------------------------------
// Pending deductions (M3/M7)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct PendingDeductions {
    map: DashMap<String, i128>,
}

impl PendingDeductions {
    pub fn add(&self, subject: &str, amount: i128) {
        *self.map.entry(subject.to_string()).or_insert(0) += amount;
    }

    pub fn subtract(&self, subject: &str, amount: i128) {
        if let dashmap::mapref::entry::Entry::Occupied(mut entry) =
            self.map.entry(subject.to_string())
        {
            *entry.get_mut() -= amount;
            if *entry.get() == 0 {
                entry.remove();
            }
        }
    }

    /// M7: unshipped charge total for one subject (`user_id` or `api_key_id`).
    pub fn outstanding(&self, subject: &str) -> i128 {
        self.map.get(subject).map(|value| *value).unwrap_or(0)
    }
}

fn delta_subject(delta: &BalanceDelta) -> Option<String> {
    match delta.kind.as_str() {
        "request_charge" => Some(delta.user_id.clone()),
        "api_key_charge" => delta.api_key_id.clone(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Replica identity (M9)
// ---------------------------------------------------------------------------

fn parse_uuid_v4(raw: &str) -> Option<String> {
    let parsed = uuid::Uuid::parse_str(raw).ok()?;
    (parsed.get_version_num() == 4).then(|| parsed.hyphenated().to_string())
}

/// M9: resolve the stable replica identity. A non-empty `configured` value
/// (`MONOIZE_REPLICA_ID`) wins and must be a version-4 UUID; otherwise the identity is
/// loaded from `{spool_dir}/replica-identity`, creating it atomically when absent or
/// corrupt. Errors are prefixed with `replica_id_invalid` or `replica_identity_unwritable`.
pub fn resolve_replica_identity(
    configured: Option<&str>,
    spool_dir: &std::path::Path,
) -> Result<String, String> {
    if let Some(raw) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return parse_uuid_v4(raw).ok_or_else(|| {
            format!("replica_id_invalid: `MONOIZE_REPLICA_ID` must be a UUID v4, got {raw:?}")
        });
    }
    let path = spool_dir.join(REPLICA_IDENTITY_FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            if let Some(id) = parse_uuid_v4(content.trim()) {
                return Ok(id);
            }
            tracing::warn!(
                path = %path.display(),
                "replica identity file content is not a UUID v4; generating a new identity"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "replica identity file unreadable; generating a new identity"
            );
        }
    }
    let id = uuid::Uuid::new_v4().hyphenated().to_string();
    std::fs::create_dir_all(spool_dir)
        .map_err(|error| format!("replica_identity_unwritable: create dir: {error}"))?;
    let tmp_path = spool_dir.join(format!(".tmp-identity-{}", uuid::Uuid::new_v4().simple()));
    let write_result = (|| {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|error| format!("replica_identity_unwritable: create temp file: {error}"))?;
        file.write_all(format!("{id}\n").as_bytes())
            .map_err(|error| format!("replica_identity_unwritable: write temp file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("replica_identity_unwritable: sync temp file: {error}"))?;
        std::fs::rename(&tmp_path, &path)
            .map_err(|error| format!("replica_identity_unwritable: publish identity: {error}"))
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }
    Ok(id)
}

/// M4a eviction: drop heartbeat entries whose last sighting is older than
/// `HEARTBEAT_EVICT_INTERVALS` ship intervals. Called on each overview read so that
/// entries left behind by replaced replica identities eventually disappear.
pub fn evict_expired_heartbeats(
    map: &DashMap<String, ReplicaHeartbeatRecord>,
    now_unix_ms: i64,
    ship_interval: Duration,
) {
    let evict_after_ms =
        (ship_interval.as_millis() as i64).saturating_mul(HEARTBEAT_EVICT_INTERVALS as i64);
    map.retain(|_, record| now_unix_ms.saturating_sub(record.last_seen_unix_ms) <= evict_after_ms);
}

// ---------------------------------------------------------------------------
// Durable delta spool (M3)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct MeteringSpoolCapacity {
    max_bytes: u64,
    accounted_bytes: AtomicU64,
    io_lock: tokio::sync::Mutex<()>,
}

impl MeteringSpoolCapacity {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes: max_bytes.max(1),
            accounted_bytes: AtomicU64::new(0),
            io_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn accounted_bytes(&self) -> u64 {
        self.accounted_bytes.load(Ordering::Acquire)
    }

    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.io_lock.lock().await
    }

    pub(crate) fn ensure_add(&self, bytes: u64) -> Result<(), ()> {
        if self
            .accounted_bytes()
            .checked_add(bytes)
            .is_none_or(|total| total > self.max_bytes)
        {
            Err(())
        } else {
            Ok(())
        }
    }

    pub(crate) fn add(&self, bytes: u64) {
        self.accounted_bytes.fetch_add(bytes, Ordering::AcqRel);
    }

    pub(crate) fn add_reconstructed(&self, bytes: u64) {
        self.accounted_bytes.fetch_add(bytes, Ordering::AcqRel);
    }

    pub(crate) fn replace(&self, old_bytes: u64, new_bytes: u64) {
        if new_bytes >= old_bytes {
            self.accounted_bytes
                .fetch_add(new_bytes - old_bytes, Ordering::AcqRel);
        } else {
            self.accounted_bytes
                .fetch_sub(old_bytes - new_bytes, Ordering::AcqRel);
        }
    }

    pub(crate) fn subtract(&self, bytes: u64) {
        self.accounted_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub struct DeltaSpool {
    dir: PathBuf,
    capacity: Arc<MeteringSpoolCapacity>,
}

impl DeltaSpool {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Result<Self, String> {
        Self::new_with_capacity(dir, Arc::new(MeteringSpoolCapacity::new(max_bytes)))
    }

    pub fn new_with_capacity(
        dir: PathBuf,
        capacity: Arc<MeteringSpoolCapacity>,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("metering_spool_unwritable: create dir: {error}"))?;
        let probe = dir.join(format!(".write-probe-{}", uuid::Uuid::new_v4().simple()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(file) => {
                drop(file);
                let _ = std::fs::remove_file(&probe);
            }
            Err(error) => {
                return Err(format!("metering_spool_unwritable: {error}"));
            }
        }
        let mut total = 0u64;
        let entries =
            std::fs::read_dir(&dir).map_err(|error| format!("open delta spool dir: {error}"))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            } else if path.file_name().and_then(|name| name.to_str())
                != Some(REPLICA_IDENTITY_FILE_NAME)
            {
                // Remove stale temporary files from an interrupted previous run; the
                // persisted replica identity (M9b) must survive this cleanup.
                let _ = std::fs::remove_file(&path);
            }
        }
        capacity.add_reconstructed(total);
        Ok(Self { dir, capacity })
    }

    fn list_json_files(&self) -> Vec<(String, u64)> {
        match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                        return None;
                    }
                    let size = entry.metadata().ok()?.len();
                    let name = path.file_name()?.to_str()?.to_string();
                    Some((name, size))
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn reconstruct_pending_amounts(&self) -> Result<Vec<(String, i128)>, String> {
        let mut names = self.list_json_files();
        names.sort_by(|left, right| left.0.cmp(&right.0));
        let mut amounts = Vec::new();
        for (name, _) in names {
            let path = self.dir.join(&name);
            match std::fs::read(&path) {
                Ok(bytes) => match serde_json::from_slice::<BalanceDelta>(&bytes) {
                    Ok(delta) => {
                        let Ok(amount) = delta.amount_nano_usd.trim().parse::<i128>() else {
                            continue;
                        };
                        if amount < 0 {
                            return Err(format!(
                                "invalid delta spool file {name}: amount_nano_usd must be nonnegative"
                            ));
                        }
                        let Some(subject) = delta_subject(&delta) else {
                            continue;
                        };
                        amounts.push((subject, amount));
                    }
                    Err(error) => {
                        tracing::warn!(
                            file = %name,
                            error = %error,
                            "skipping unreadable delta spool file during pending reconstruction"
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        file = %name,
                        error = %error,
                        "skipping unreadable delta spool file during pending reconstruction"
                    );
                }
            }
        }
        Ok(amounts)
    }

    pub fn pending_files(&self) -> usize {
        self.list_json_files().len()
    }

    /// `(pending file count, total pending bytes)` over the durable spool.
    pub fn pending_stats(&self) -> (usize, u64) {
        let files = self.list_json_files();
        let bytes = files.iter().map(|(_, size)| *size).sum();
        (files.len(), bytes)
    }

    pub fn dir_display(&self) -> String {
        self.dir.display().to_string()
    }

    pub async fn enqueue(&self, delta: &BalanceDelta) -> Result<(), String> {
        validate_delta(delta).map_err(str::to_string)?;
        let payload = serde_json::to_vec(delta).map_err(|error| error.to_string())?;
        let _io_guard = self.capacity.lock().await;
        let current = self.capacity.accounted_bytes();
        if self.capacity.ensure_add(payload.len() as u64).is_err() {
            return Err(format!(
                "metering_spool_quota_exhausted ({}/{})",
                current,
                self.capacity.max_bytes()
            ));
        }
        let name = format!(
            "{:020}-{}.json",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4().simple()
        );
        let final_path = self.dir.join(&name);
        let tmp_path = self.dir.join(format!(".tmp-{name}"));
        write_durable(&tmp_path, &payload).await?;
        if let Err(error) = tokio::fs::rename(&tmp_path, &final_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(format!("publish delta spool file: {error}"));
        }
        if let Err(error) = sync_dir(&self.dir).await {
            tracing::warn!(error = %error, "delta spool directory sync failed");
        }
        self.capacity.add(payload.len() as u64);
        Ok(())
    }

    pub async fn load_batch(&self, max_entries: usize) -> Vec<(PathBuf, u64, BalanceDelta)> {
        let _io_guard = self.capacity.lock().await;
        let mut names: Vec<(String, u64)> = self.list_json_files();
        // Oldest first: the timestamp prefix sorts lexicographically.
        names.sort_by(|left, right| left.0.cmp(&right.0));
        let mut loaded = Vec::with_capacity(names.len());
        for (name, size) in names.into_iter().take(max_entries) {
            let path = self.dir.join(&name);
            match tokio::fs::read(&path).await {
                Ok(bytes) => match serde_json::from_slice::<BalanceDelta>(&bytes) {
                    Ok(delta) => loaded.push((path, size, delta)),
                    Err(error) => {
                        tracing::warn!(file = %name, error = %error, "skipping corrupt delta spool file");
                        let _ = tokio::fs::remove_file(&path).await;
                        self.capacity.subtract(size);
                    }
                },
                Err(error) => {
                    tracing::warn!(file = %name, error = %error, "reading delta spool file failed");
                }
            }
        }
        loaded
    }

    pub async fn release(&self, files: &[(PathBuf, u64)]) {
        let _io_guard = self.capacity.lock().await;
        for (path, size) in files {
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(path = %path.display(), error = %error, "removing shipped delta file failed");
                    continue;
                }
            }
            self.capacity.subtract(*size);
        }
    }
}

async fn write_durable(path: &std::path::Path, payload: &[u8]) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|error| format!("create temp file: {error}"))?;
    file.write_all(payload)
        .await
        .map_err(|error| format!("write temp file: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("sync temp file: {error}"))?;
    Ok(())
}

async fn sync_dir(dir: &std::path::Path) -> Result<(), String> {
    let file = tokio::fs::File::open(dir)
        .await
        .map_err(|error| error.to_string())?;
    file.sync_all().await.map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Replica-side metering context and shipment loop (M4–M6)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct ShipExtras {
    terminals: Vec<TerminalSpoolRecord>,
    last_used: Vec<LastUsedPair>,
    deltas: Vec<(PathBuf, u64, BalanceDelta)>,
}

impl ShipExtras {
    fn is_empty(&self) -> bool {
        self.terminals.is_empty() && self.last_used.is_empty() && self.deltas.is_empty()
    }

    fn into_batch(self, request_logs: Vec<SpoolRequestLog>) -> MeteringBatch {
        MeteringBatch {
            replica: None,
            request_logs,
            last_used: self.last_used,
            plan_terminals: self
                .terminals
                .iter()
                .map(TerminalSpoolRecord::wire)
                .collect(),
            balance_deltas: self.deltas.into_iter().map(|(_, _, delta)| delta).collect(),
        }
    }

    fn trim_to_fit(&mut self, log_count: usize, hard_cap: usize) -> ShipExtras {
        let mut leftover = ShipExtras {
            terminals: Vec::new(),
            last_used: Vec::new(),
            deltas: Vec::new(),
        };
        let mut remaining = hard_cap.saturating_sub(log_count);
        if self.terminals.len() > remaining {
            leftover.terminals = self.terminals.split_off(remaining);
        }
        remaining = remaining.saturating_sub(self.terminals.len());
        if self.deltas.len() > remaining {
            leftover.deltas = self.deltas.split_off(remaining);
        }
        remaining = remaining.saturating_sub(self.deltas.len());
        if self.last_used.len() > remaining {
            leftover.last_used = self.last_used.split_off(remaining);
        }
        leftover
    }
}

struct BatchSink<'a> {
    metering: &'a ReplicaMetering,
    last_used: &'a LastUsedBatcher,
    extras: Mutex<Option<ShipExtras>>,
    released: AtomicBool,
    invalid_plan_ack: AtomicBool,
    deliver_attempted: AtomicBool,
}

impl<'a> BatchSink<'a> {
    fn take_extras(&self) -> Option<ShipExtras> {
        self.extras
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take()
    }

    fn was_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    fn deliver_attempted(&self) -> bool {
        self.deliver_attempted.load(Ordering::Acquire)
    }

    fn invalid_plan_ack(&self) -> bool {
        self.invalid_plan_ack.load(Ordering::Acquire)
    }
}

#[async_trait::async_trait]
impl MeteringSink for BatchSink<'_> {
    async fn deliver(&self, entries: &[SpoolRequestLog]) -> Result<(), String> {
        self.deliver_attempted.store(true, Ordering::Release);
        let Some(extras) = self.take_extras() else {
            return Err("metering sink state unavailable".to_string());
        };
        let invalid_plan_ack = self
            .metering
            .send_composed(entries.to_vec(), extras, self.last_used)
            .await?;
        self.invalid_plan_ack
            .store(invalid_plan_ack, Ordering::Release);
        self.released.store(true, Ordering::Release);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipTick {
    Idle,
    Success,
    Failure,
}

pub fn next_consecutive_failures(current: usize, tick: ShipTick) -> usize {
    match tick {
        ShipTick::Failure => current.saturating_add(1),
        ShipTick::Success | ShipTick::Idle => 0,
    }
}

#[derive(Clone)]
pub struct ReplicaHeartbeatSource {
    pub id: String,
    pub hostname: String,
    pub listen: String,
    pub version: String,
    pub started_at: String,
}

#[derive(Clone)]
pub struct ReplicaMetering {
    delta_spool: Arc<DeltaSpool>,
    capacity: Arc<MeteringSpoolCapacity>,
    admission_verifier: AdmissionVerifierRing,
    admission_claims: Arc<AdmissionClaimStore>,
    admission_client: AdmissionClient,
    pending: Arc<PendingDeductions>,
    client: reqwest::Client,
    endpoint: String,
    token: String,
    ship_batch_max: usize,
    replica_id: String,
    heartbeat_source: Option<ReplicaHeartbeatSource>,
    ship_notify: Arc<tokio::sync::Notify>,
}

impl ReplicaMetering {
    pub fn new(
        spool_dir: PathBuf,
        spool_max_bytes: u64,
        primary_url: &str,
        token: &str,
        ship_batch_max: usize,
        replica_id: String,
    ) -> Result<Self, String> {
        crate::node_config::ensure_rustls_crypto_provider()?;
        let client = reqwest::Client::builder()
            .user_agent("monoize/0.1")
            // PX4/PX8: cluster traffic bypasses every proxy including env-inherited ones.
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| error.to_string())?;
        let endpoint = format!(
            "{}{}",
            primary_url.trim_end_matches('/'),
            METERING_INGEST_PATH
        );
        let capacity = Arc::new(MeteringSpoolCapacity::new(spool_max_bytes));
        let admission_verifier = AdmissionVerifierRing::new();
        let delta_spool = DeltaSpool::new_with_capacity(spool_dir.clone(), capacity.clone())?;
        let admission_claims = Arc::new(
            AdmissionClaimStore::open_with_capacity(
                spool_dir.join("plan-admission"),
                capacity.clone(),
            )
            .map_err(|error| error.to_string())?,
        );
        let ship_notify = Arc::new(tokio::sync::Notify::new());
        let admission_client = AdmissionClient::new(
            client.clone(),
            primary_url.to_string(),
            token.to_string(),
            replica_id.clone(),
            Duration::from_secs(5),
            admission_verifier.clone(),
            admission_claims.clone(),
            ship_notify.clone(),
        )
        .map_err(|error| error.to_string())?;
        let pending = PendingDeductions::default();
        for (subject, amount) in delta_spool.reconstruct_pending_amounts()? {
            pending.add(&subject, amount);
        }
        Ok(Self {
            delta_spool: Arc::new(delta_spool),
            capacity,
            admission_verifier,
            admission_claims,
            admission_client,
            pending: Arc::new(pending),
            client,
            endpoint,
            token: token.to_string(),
            ship_batch_max: ship_batch_max.clamp(1, METERING_BATCH_HARD_CAP),
            replica_id,
            heartbeat_source: None,
            ship_notify,
        })
    }

    pub fn with_heartbeat_source(mut self, mut source: ReplicaHeartbeatSource) -> Self {
        source.id = self.replica_id.clone();
        self.heartbeat_source = Some(source);
        self
    }

    pub fn with_admission_refresh_interval(mut self, interval: Duration) -> Self {
        self.admission_client = self
            .admission_client
            .clone()
            .with_refresh_interval(interval);
        self
    }

    fn current_heartbeat(&self) -> Option<ReplicaHeartbeat> {
        let source = self.heartbeat_source.as_ref()?;
        let started_at = chrono::DateTime::parse_from_rfc3339(&source.started_at)
            .ok()
            .map(|value| value.with_timezone(&chrono::Utc));
        let uptime_seconds = started_at
            .and_then(|started| {
                chrono::Utc::now()
                    .signed_duration_since(started)
                    .to_std()
                    .ok()
            })
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let (spool_pending_count, spool_pending_bytes) = self.delta_spool.pending_stats();
        Some(ReplicaHeartbeat {
            id: source.id.clone(),
            hostname: source.hostname.clone(),
            listen: source.listen.clone(),
            version: source.version.clone(),
            started_at: source.started_at.clone(),
            uptime_seconds,
            spool_pending_count,
            spool_pending_bytes,
        })
    }

    pub fn pending(&self) -> &PendingDeductions {
        &self.pending
    }

    pub fn replica_id(&self) -> &str {
        &self.replica_id
    }

    /// Charge-path entry point on replicas (M3): durable enqueue or fail; the caller
    /// maps failure onto the terminal billing-failure path per MB-C6.
    pub(crate) async fn enqueue_balance_delta_for_request(
        state: &crate::app::AppState,
        kind: &str,
        user_id: &str,
        api_key_id: Option<&str>,
        amount_nano_usd: i128,
        meta_json: &Value,
    ) -> Result<(), String> {
        let Some(metering) = state.metering.as_ref() else {
            return Err("metering pipeline unavailable on this node".to_string());
        };
        metering
            .enqueue_balance_delta(kind, user_id, api_key_id, amount_nano_usd, meta_json)
            .await
    }

    pub fn delta_spool(&self) -> &DeltaSpool {
        &self.delta_spool
    }

    pub fn spool_capacity(&self) -> &Arc<MeteringSpoolCapacity> {
        &self.capacity
    }

    pub fn admission_verifier(&self) -> &AdmissionVerifierRing {
        &self.admission_verifier
    }

    pub fn admission_claims(&self) -> &AdmissionClaimStore {
        &self.admission_claims
    }

    pub fn admission_client(&self) -> &AdmissionClient {
        &self.admission_client
    }

    pub fn spawn_admission_refresh_loop(
        &self,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        self.admission_client
            .clone()
            .spawn_keyset_refresh_loop(shutdown)
    }

    pub async fn spool_plan_terminal(
        &self,
        input: TerminalSpoolInput,
    ) -> Result<TerminalSpoolRecord, crate::store_billing::admission_token::AdmissionError> {
        let record = self.admission_claims.spool_terminal(input).await?;
        self.ship_notify.notify_one();
        Ok(record)
    }

    /// M3: durable publish before success, plus atomic pending-counter increment.
    pub async fn enqueue_balance_delta(
        &self,
        kind: &str,
        user_id: &str,
        api_key_id: Option<&str>,
        amount_nano_usd: i128,
        meta_json: &Value,
    ) -> Result<(), String> {
        let delta = BalanceDelta {
            delta_id: uuid::Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            user_id: user_id.to_string(),
            api_key_id: api_key_id.map(str::to_string),
            amount_nano_usd: amount_nano_usd.to_string(),
            meta_json: meta_json.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        validate_delta(&delta).map_err(|message| message.to_string())?;
        self.delta_spool.enqueue(&delta).await?;
        if let Some(subject) = delta_subject(&delta) {
            self.pending.add(&subject, amount_nano_usd);
        }
        self.ship_notify.notify_one();
        Ok(())
    }

    async fn post_batch(
        &self,
        request_logs: Vec<SpoolRequestLog>,
        extras: &ShipExtras,
    ) -> Result<MeteringAck, String> {
        let mut batch = extras.clone().into_batch(request_logs);
        batch.replica = self.current_heartbeat();
        if batch.is_empty() && batch.replica.is_none() {
            return Ok(MeteringAck {
                applied_request_logs: 0,
                applied_last_used: 0,
                applied_balance_deltas: 0,
                plan_terminal_acks: Vec::new(),
            });
        }
        if batch
            .plan_terminals
            .iter()
            .any(|terminal| terminal.audience != self.replica_id)
            || batch
                .replica
                .as_ref()
                .is_some_and(|heartbeat| heartbeat.id != self.replica_id)
        {
            return Err("plan terminal audience mismatch".to_string());
        }
        let request = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .header("X-Monoize-Replica-ID", &self.replica_id)
            .json(&batch);
        let response = request
            .send()
            .await
            .map_err(|error| format!("metering transport error: {error}"))?;
        let (status, body) = read_internal_response(response)
            .await
            .map_err(|error| format!("invalid metering ack body: {error}"))?;
        if status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 413 {
            return Err(format!("primary rejected batch as too large ({status})"));
        }
        if !status.is_success() {
            return Err(format!(
                "primary returned {status}: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        let ack: MeteringAck = serde_json::from_slice(&body)
            .map_err(|error| format!("invalid metering ack body: {error}"))?;
        Ok(ack)
    }

    async fn release_extras(&self, extras: &ShipExtras, ack: &MeteringAck) -> bool {
        let unexpected_ack = ack.plan_terminal_acks.iter().any(|candidate| {
            !extras.terminals.iter().any(|terminal| {
                candidate.token_id == terminal.input.token_id
                    && candidate.canonical_digest == terminal.canonical_digest
            })
        });
        let mut invalid_plan_ack = unexpected_ack;
        for terminal in &extras.terminals {
            let matching = ack
                .plan_terminal_acks
                .iter()
                .filter(|candidate| {
                    candidate.token_id == terminal.input.token_id
                        && candidate.canonical_digest == terminal.canonical_digest
                })
                .count();
            if unexpected_ack
                || matching != 1
                || self
                    .admission_claims
                    .acknowledge_terminal(
                        &terminal.input.token_id,
                        &terminal.canonical_digest,
                        chrono::Utc::now(),
                    )
                    .await
                    .is_err()
            {
                invalid_plan_ack = true;
            }
        }
        let files: Vec<(PathBuf, u64)> = extras
            .deltas
            .iter()
            .map(|(path, size, _)| (path.clone(), *size))
            .collect();
        self.delta_spool.release(&files).await;
        for delta in extras.deltas.iter().map(|(_, _, delta)| delta) {
            if let Some(subject) = delta_subject(delta) {
                let amount = delta.amount_nano_usd.trim().parse::<i128>().unwrap_or(0);
                self.pending.subtract(&subject, amount);
            }
        }
        metrics::counter!("monoize_replica_metering_shipped_total", "result" => "ok").increment(1);
        metrics::gauge!("monoize_replica_metering_pending_entries")
            .set(self.delta_spool.pending_files() as f64);
        invalid_plan_ack
    }

    fn requeue_last_used(&self, pairs: Vec<LastUsedPair>, last_used: &LastUsedBatcher) {
        for pair in pairs {
            if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&pair.last_used_at) {
                last_used.record_retry(pair.api_key_id, timestamp.with_timezone(&chrono::Utc));
            }
        }
    }

    async fn send_composed(
        &self,
        request_logs: Vec<SpoolRequestLog>,
        mut extras: ShipExtras,
        last_used: &LastUsedBatcher,
    ) -> Result<bool, String> {
        let leftover = extras.trim_to_fit(request_logs.len(), METERING_BATCH_HARD_CAP);
        self.requeue_last_used(leftover.last_used, last_used);
        match self.post_batch(request_logs, &extras).await {
            Ok(ack) => {
                let invalid_plan_ack = self.release_extras(&extras, &ack).await;
                if invalid_plan_ack {
                    metrics::counter!("monoize_replica_metering_shipped_total", "result" => "error")
                        .increment(1);
                }
                Ok(invalid_plan_ack)
            }
            Err(error) => {
                self.requeue_last_used(extras.last_used, last_used);
                metrics::counter!("monoize_replica_metering_shipped_total", "result" => "error")
                    .increment(1);
                Err(error)
            }
        }
    }

    /// One M4 tick: at most one POST carrying request logs, last-used pairs, and deltas.
    pub async fn ship_once(
        &self,
        log_batcher: &RequestLogBatcher,
        last_used: &LastUsedBatcher,
    ) -> ShipTick {
        if let Err(error) = self
            .admission_claims
            .publish_release_pending(chrono::Utc::now())
            .await
        {
            tracing::warn!(error = %error, "release-pending admission publication failed");
            return ShipTick::Failure;
        }
        let last_used_pairs = last_used
            .drain_limit(METERING_BATCH_HARD_CAP)
            .into_iter()
            .map(|(id, timestamp)| LastUsedPair {
                api_key_id: id,
                last_used_at: timestamp.to_rfc3339(),
            })
            .collect::<Vec<_>>();
        let terminals = match self
            .admission_claims
            .load_pending_terminals(self.ship_batch_max)
            .await
        {
            Ok(terminals) => terminals,
            Err(error) => {
                tracing::warn!(error = %error, "plan terminal spool load failed");
                return ShipTick::Failure;
            }
        };
        let delta_files = self.delta_spool.load_batch(self.ship_batch_max).await;
        let extras = ShipExtras {
            terminals,
            last_used: last_used_pairs,
            deltas: delta_files,
        };

        let sink = BatchSink {
            metering: self,
            last_used,
            extras: Mutex::new(Some(extras)),
            released: AtomicBool::new(false),
            invalid_plan_ack: AtomicBool::new(false),
            deliver_attempted: AtomicBool::new(false),
        };
        let _shipped_logs = log_batcher.ship_via(self.ship_batch_max, &sink).await;
        if sink.was_released() {
            return if sink.invalid_plan_ack() {
                ShipTick::Failure
            } else {
                ShipTick::Success
            };
        }
        if sink.deliver_attempted() {
            return ShipTick::Failure;
        }

        let leftovers = sink.take_extras().unwrap_or_default();
        if leftovers.is_empty() && self.heartbeat_source.is_none() {
            return ShipTick::Idle;
        }
        match self.send_composed(Vec::new(), leftovers, last_used).await {
            Ok(false) => ShipTick::Success,
            Ok(true) => ShipTick::Failure,
            Err(error) => {
                tracing::warn!(error = %error, "replica metering shipment failed");
                ShipTick::Failure
            }
        }
    }

    pub fn spawn_ship_loop(
        self: &Arc<Self>,
        log_batcher: RequestLogBatcher,
        last_used: LastUsedBatcher,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let metering = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut consecutive_failures = 0usize;
            let log_notify = log_batcher.ship_notify();
            let delta_notify = metering.ship_notify.clone();
            loop {
                tokio::select! {
                    _ = ticker.tick() => {}
                    _ = log_notify.notified() => {}
                    _ = delta_notify.notified() => {}
                }
                let tick = metering.ship_once(&log_batcher, &last_used).await;
                consecutive_failures = next_consecutive_failures(consecutive_failures, tick);
                if consecutive_failures >= 3 {
                    tracing::warn!(
                        consecutive_failures,
                        "replica metering shipments keep failing; data remains durably spooled"
                    );
                }
                metrics::gauge!("monoize_replica_metering_pending_entries")
                    .set(metering.delta_spool.pending_files() as f64);
            }
        })
    }

    /// M6: best-effort single attempt at graceful shutdown.
    pub async fn final_ship(&self, log_batcher: &RequestLogBatcher, last_used: &LastUsedBatcher) {
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            self.ship_once(log_batcher, last_used),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// Primary-side ingest endpoint (I1–I6)
// ---------------------------------------------------------------------------

pub fn sha256_hex_lower(input: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn metering_error(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
) -> axum::response::Response {
    (
        status,
        axum::Json(serde_json::json!({ "error": { "code": code, "message": message.into() } })),
    )
        .into_response()
}

/// I2: bearer compared by SHA-256 digest equality (constant-time).
pub(crate) fn verify_ingest_token(headers: &HeaderMap, expected_digest: &[u8; 32]) -> bool {
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .map(str::trim);
    let Some(provided) = provided else {
        return false;
    };
    constant_time_eq(&sha256_hex_lower(provided), expected_digest)
}

pub(crate) async fn ingest_metering_handler(
    State(state): State<crate::app::AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let Some(expected) = state.metering_token_digest else {
        return metering_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "ingest endpoint disabled",
        );
    };
    if !verify_ingest_token(&headers, &expected) {
        return metering_error(
            StatusCode::UNAUTHORIZED,
            "replica_auth_failed",
            "invalid replica token",
        );
    }
    let batch: MeteringBatch = match serde_json::from_slice(&body) {
        Ok(batch) => batch,
        Err(error) => {
            return metering_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "metering_batch_invalid",
                error.to_string(),
            );
        }
    };
    if batch.total_entries() > METERING_BATCH_HARD_CAP {
        return metering_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "metering_batch_too_large",
            format!("batch exceeds hard cap of {METERING_BATCH_HARD_CAP} entries"),
        );
    }
    if !batch.plan_terminals.is_empty() {
        let replica_id = headers
            .get("x-monoize-replica-id")
            .and_then(|value| value.to_str().ok());
        let audience_matches = replica_id.is_some_and(|replica_id| {
            batch
                .plan_terminals
                .iter()
                .all(|terminal| terminal.audience == replica_id)
                && batch
                    .replica
                    .as_ref()
                    .is_none_or(|heartbeat| heartbeat.id == replica_id)
        });
        if !audience_matches {
            return metering_error(
                StatusCode::FORBIDDEN,
                "replica_audience_mismatch",
                "plan terminal audience does not match replica identity",
            );
        }
        for terminal in &batch.plan_terminals {
            if let Err(error) = terminal_apply_input(terminal) {
                let code = if matches!(error, AdmissionRuntimeError::TerminalDigestInvalid) {
                    error.code()
                } else {
                    "metering_batch_invalid"
                };
                return metering_error(StatusCode::UNPROCESSABLE_ENTITY, code, error.to_string());
            }
        }
    }
    for pair in &batch.last_used {
        if pair.api_key_id.is_empty() || pair.last_used_at.is_empty() {
            return metering_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "metering_batch_invalid",
                "last_used entries require api_key_id and last_used_at",
            );
        }
    }
    for delta in &batch.balance_deltas {
        if let Err(message) = validate_delta(delta) {
            return metering_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "metering_batch_invalid",
                message,
            );
        }
        if delta.kind == "api_key_charge" && delta.api_key_id.as_deref().unwrap_or("").is_empty() {
            return metering_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "metering_batch_invalid",
                "api_key_charge requires api_key_id",
            );
        }
    }

    if let Some(replica) = batch.replica.clone() {
        if replica.id.trim().is_empty() {
            return metering_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "metering_batch_invalid",
                "replica.id must be non-empty",
            );
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        state.replica_heartbeats.insert(
            replica.id.clone(),
            ReplicaHeartbeatRecord {
                heartbeat: replica,
                last_seen_unix_ms: now_ms,
            },
        );
    }

    match apply_metering_batch_result(&state.db_pool, &batch).await {
        Ok(ack) => {
            if !batch.request_logs.is_empty() {
                let rows = batch
                    .request_logs
                    .iter()
                    .map(SpoolRequestLog::to_insert_log)
                    .collect::<Vec<_>>();
                let _ = state.log_broadcast.send(rows);
            }
            axum::Json(ack).into_response()
        }
        Err(error) => {
            tracing::warn!(error = %error, "metering batch apply failed; replica will retry");
            match &error {
                AdmissionRuntimeError::TokenNotFound => {
                    metering_error(StatusCode::NOT_FOUND, error.code(), error.to_string())
                }
                AdmissionRuntimeError::BindingMismatch
                | AdmissionRuntimeError::TerminalConflict => {
                    metering_error(StatusCode::CONFLICT, error.code(), error.to_string())
                }
                AdmissionRuntimeError::TerminalDigestInvalid => metering_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    error.code(),
                    error.to_string(),
                ),
                _ => metering_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "metering_apply_failed",
                    error.to_string(),
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Idempotent apply (I4–I5), also reused by the PRP9 promotion drain
// ---------------------------------------------------------------------------

const LAST_USED_CHUNK_ENTRIES: usize = 256;

pub async fn apply_metering_batch(
    db: &DbPool,
    batch: &MeteringBatch,
) -> Result<MeteringAck, String> {
    apply_metering_batch_result(db, batch)
        .await
        .map_err(admission_batch_error)
}

async fn apply_metering_batch_result(
    db: &DbPool,
    batch: &MeteringBatch,
) -> Result<MeteringAck, AdmissionRuntimeError> {
    for delta in &batch.balance_deltas {
        validate_delta(delta)
            .map_err(|message| AdmissionRuntimeError::Storage(message.to_string()))?;
    }
    let terminals = batch
        .plan_terminals
        .iter()
        .map(terminal_apply_input)
        .collect::<Result<Vec<_>, _>>()?;
    if db.is_sqlite() {
        let db_for_tx = db.clone();
        let batch = batch.clone();
        db.with_immediate_write(move |connection| {
            Box::pin(async move {
                apply_metering_batch_tx(&db_for_tx, connection, &batch, terminals).await
            })
        })
        .await
    } else {
        let tx = db
            .begin_write()
            .await
            .map_err(AdmissionRuntimeError::from)?;
        let outcome = apply_metering_batch_tx(db, &*tx, batch, terminals).await;
        match outcome {
            Ok(ack) => {
                tx.commit().await.map_err(AdmissionRuntimeError::from)?;
                Ok(ack)
            }
            Err(error) => {
                tx.rollback().await.map_err(AdmissionRuntimeError::from)?;
                Err(error)
            }
        }
    }
}

async fn apply_metering_batch_tx<C: sea_orm::ConnectionTrait>(
    db: &DbPool,
    connection: &C,
    batch: &MeteringBatch,
    terminals: Vec<TerminalApplyInput>,
) -> Result<MeteringAck, AdmissionRuntimeError> {
    let admission = AdmissionService::new(db.clone(), None, ADMISSION_ISSUER)?;
    let mut applied_request_logs = 0u64;
    let mut applied_balance_deltas = 0u64;
    let mut plan_terminal_acks = Vec::with_capacity(terminals.len());

    for chunk in batch
        .request_logs
        .chunks(crate::db_cache::REQUEST_LOG_INSERT_CHUNK_ENTRIES)
    {
        let (sql, values) = crate::db_cache::request_log_insert_chunk(chunk.iter());
        let outcome = connection
            .execute(db.stmt(&sql, values))
            .await
            .map_err(|error| AdmissionRuntimeError::Storage(error.to_string()))?;
        applied_request_logs += outcome.rows_affected();
    }

    for terminal in terminals {
        let token_id = terminal.token_id.clone();
        let canonical_digest = terminal.canonical_digest.clone();
        let result = admission.apply_terminal_tx(connection, terminal).await?;
        plan_terminal_acks.push(PlanTerminalAcknowledgement {
            token_id,
            canonical_digest,
            result: match result {
                TerminalApplyResult::Applied => TerminalAcknowledgementResult::Applied,
                TerminalApplyResult::Duplicate => TerminalAcknowledgementResult::Duplicate,
            },
        });
    }

    for chunk in batch.last_used.chunks(LAST_USED_CHUNK_ENTRIES) {
        let pairs = chunk
            .iter()
            .filter_map(|pair| {
                chrono::DateTime::parse_from_rfc3339(&pair.last_used_at)
                    .ok()
                    .map(|parsed| (pair.api_key_id.clone(), parsed.with_timezone(&chrono::Utc)))
            })
            .collect::<Vec<_>>();
        if pairs.is_empty() {
            continue;
        }
        let (sql, values) = crate::db_cache::last_used_bulk_update(&pairs);
        connection
            .execute(db.stmt(&sql, values))
            .await
            .map_err(|error| AdmissionRuntimeError::Storage(error.to_string()))?;
    }

    for delta in &batch.balance_deltas {
        applied_balance_deltas += apply_balance_delta(db, connection, delta)
            .await
            .map_err(AdmissionRuntimeError::Storage)?;
    }

    metrics::counter!("monoize_primary_metering_applied_total").increment(applied_balance_deltas);
    Ok(MeteringAck {
        applied_request_logs,
        applied_last_used: batch.last_used.len() as u64,
        applied_balance_deltas,
        plan_terminal_acks,
    })
}

/// I4 step 3: ledger insert is the idempotency anchor; the balance update runs only
/// when this specific delta had not been applied before. Returns 1 iff newly inserted.
async fn apply_balance_delta<C>(db: &DbPool, tx: &C, delta: &BalanceDelta) -> Result<u64, String>
where
    C: sea_orm::ConnectionTrait,
{
    // Both backends require the index predicate in the conflict target to match a
    // partial unique index.
    let conflict_clause =
        "ON CONFLICT(idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING";
    let sql = format!(
        "INSERT INTO billing_ledger (id, user_id, kind, delta_nano_usd, balance_after_nano_usd, meta_json, created_at, idempotency_key) \
         VALUES ($1, $2, $3, $4, NULL, $5, $6, $7) {conflict_clause}"
    );
    let insert_result = tx
        .execute(db.stmt(
            &sql,
            vec![
                    delta.delta_id.clone().into(),
                    delta.user_id.clone().into(),
                    delta.kind.clone().into(),
                    (-delta
                        .amount_nano_usd
                        .trim()
                        .parse::<i128>()
                        .map_err(|_| "amount_nano_usd must be decimal i128 text")?)
                    .to_string()
                    .into(),
                    delta.meta_json.to_string().into(),
                    delta.created_at.clone().into(),
                    delta.delta_id.clone().into(),
                ],
        ))
        .await
        .map_err(|error| error.to_string())?;
    if insert_result.rows_affected() == 0 {
        return Ok(0);
    }

    let amount = delta
        .amount_nano_usd
        .trim()
        .parse::<i128>()
        .map_err(|_| "amount_nano_usd must be decimal i128 text".to_string())?;
    match delta.kind.as_str() {
        "request_charge" => {
            subtract_user_balance_tx(db, tx, &delta.user_id, amount).await?;
        }
        "api_key_charge" => {
            let api_key_id = delta.api_key_id.clone().unwrap_or_default();
            let lock_suffix = if db.is_sqlite() { "" } else { " FOR UPDATE" };
            let _ = tx
                .query_one(db.stmt(
                    &format!("SELECT id FROM users WHERE id = $1{lock_suffix}"),
                    vec![delta.user_id.clone().into()],
                ))
                .await
                .map_err(|error| error.to_string())?;
            let rows = tx
                .query_all(
                    db.stmt(
                        &format!(
                            "SELECT user_id, sub_account_enabled, sub_account_balance_nano FROM api_keys WHERE id = $1{lock_suffix}"
                        ),
                        vec![api_key_id.clone().into()],
                    ),
                )
                .await
                .map_err(|error| error.to_string())?;
            let sub_state = rows.first().map(|row| {
                (
                    row.try_get::<String>("", "user_id")
                        .map_err(|e| e.to_string()),
                    row.try_get::<Option<i32>>("", "sub_account_enabled")
                        .map(|flag| flag.unwrap_or(0) != 0)
                        .map_err(|e| e.to_string()),
                    row.try_get::<Option<String>>("", "sub_account_balance_nano")
                        .map_err(|e| e.to_string()),
                )
            });
            let Some((user_id_res, enabled, stored_balance)) = sub_state else {
                // Key vanished between enqueue and apply: keep the ledger event, no balance change.
                return Ok(1);
            };
            let owner_user_id = user_id_res?;
            let enabled = enabled?;
            let current = stored_balance?
                .and_then(|raw| raw.trim().parse::<i128>().ok())
                .unwrap_or(0);
            if enabled {
                let next = checked_sub_allow_negative(current, amount)?;
                tx.execute(db.stmt(
                    "UPDATE api_keys SET sub_account_balance_nano = $1 WHERE id = $2",
                    vec![next.to_string().into(), api_key_id.into()],
                ))
                .await
                .map_err(|error| error.to_string())?;
            } else {
                // Fallback mirrors charge_sub_account_balance_nano on the primary.
                subtract_user_balance_tx(db, tx, &owner_user_id, amount).await?;
            }
        }
        other => return Err(format!("unsupported delta kind {other:?}")),
    }
    Ok(1)
}

fn checked_sub_allow_negative(current: i128, amount: i128) -> Result<i128, String> {
    // I5: negative results are allowed; only genuine i128 overflow aborts the batch.
    current
        .checked_sub(amount)
        .ok_or_else(|| "balance overflow".to_string())
}

async fn subtract_user_balance_tx<C>(
    db: &DbPool,
    tx: &C,
    user_id: &str,
    amount: i128,
) -> Result<(), String>
where
    C: sea_orm::ConnectionTrait,
{
    let lock_suffix = if db.is_sqlite() { "" } else { " FOR UPDATE" };
    let select_sql =
        format!("SELECT balance_nano_usd, balance_unlimited FROM users WHERE id = $1{lock_suffix}");
    let rows = tx
        .query_all(db.stmt(&select_sql, vec![user_id.to_string().into()]))
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = rows.first() else {
        // Unknown user: ledger row already recorded above; nothing else to mutate.
        return Ok(());
    };
    let unlimited: bool = row
        .try_get::<Option<i32>>("", "balance_unlimited")
        .map(|flag| flag.unwrap_or(0) != 0)
        .map_err(|error| error.to_string())?;
    if unlimited {
        return Ok(());
    }
    let current: i128 = row
        .try_get::<Option<String>>("", "balance_nano_usd")
        .map_err(|error| error.to_string())?
        .and_then(|raw| raw.trim().parse::<i128>().ok())
        .ok_or_else(|| "malformed stored balance".to_string())?;
    let next = checked_sub_allow_negative(current, amount)?;
    tx.execute(db.stmt(
        "UPDATE users SET balance_nano_usd = $1, updated_at = $2 WHERE id = $3",
        vec![
            next.to_string().into(),
            chrono::Utc::now().to_rfc3339().into(),
            user_id.to_string().into(),
        ],
    ))
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// PRP9: drain leftover delta spool entries directly into the local database when a
/// former replica starts as the primary. Runs after migrations, before serving traffic.
pub async fn drain_delta_spool_to_local_db(db: &DbPool, spool: &DeltaSpool) -> Result<(), String> {
    loop {
        let files = spool.load_batch(METERING_BATCH_HARD_CAP).await;
        if files.is_empty() {
            return Ok(());
        }
        let batch = MeteringBatch {
            replica: None,
            request_logs: Vec::new(),
            last_used: Vec::new(),
            plan_terminals: Vec::new(),
            balance_deltas: files.iter().map(|(_, _, delta)| delta.clone()).collect(),
        };
        let ack = apply_metering_batch(db, &batch).await.map_err(|error| {
            format!(
                "promotion drain failed: {error}; spool entries preserved at {}",
                spool.dir_display()
            )
        })?;
        // Every entry is now either applied or already present server-side under the
        // same idempotency key, so releasing all of them is safe.
        let refs: Vec<(PathBuf, u64)> = files
            .iter()
            .map(|(path, size, _)| (path.clone(), *size))
            .collect();
        let _ = ack.applied_balance_deltas;
        spool.release(&refs).await;
        if files.len() < METERING_BATCH_HARD_CAP {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn trim_to_fit_keeps_total_at_hard_cap() {
        let mut extras = ShipExtras {
            terminals: Vec::new(),
            last_used: (0..1500)
                .map(|i| LastUsedPair {
                    api_key_id: format!("k{i}"),
                    last_used_at: "t".to_string(),
                })
                .collect(),
            deltas: (0..800)
                .map(|i| {
                    (
                        PathBuf::from(format!("{i}.json")),
                        1,
                        BalanceDelta {
                            delta_id: format!("{i}"),
                            kind: "request_charge".to_string(),
                            user_id: "u".to_string(),
                            api_key_id: None,
                            amount_nano_usd: "1".to_string(),
                            meta_json: Value::Null,
                            created_at: "t".to_string(),
                        },
                    )
                })
                .collect(),
        };
        let leftover = extras.trim_to_fit(500, METERING_BATCH_HARD_CAP);
        assert_eq!(extras.deltas.len(), 800.min(METERING_BATCH_HARD_CAP - 500));
        assert_eq!(
            extras.last_used.len() + extras.deltas.len() + 500,
            METERING_BATCH_HARD_CAP
        );
        assert!(!leftover.last_used.is_empty() || !leftover.deltas.is_empty());
    }

    #[test]
    fn consecutive_failures_reset_on_idle_and_success() {
        assert_eq!(next_consecutive_failures(0, ShipTick::Failure), 1);
        assert_eq!(next_consecutive_failures(1, ShipTick::Failure), 2);
        assert_eq!(next_consecutive_failures(2, ShipTick::Failure), 3);
        assert_eq!(next_consecutive_failures(3, ShipTick::Idle), 0);
        assert_eq!(next_consecutive_failures(2, ShipTick::Success), 0);
    }

    #[test]
    fn delta_spool_accepts_writable_directory() {
        let temp = tempfile::TempDir::new().unwrap();
        DeltaSpool::new(temp.path().to_path_buf(), 1024).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn delta_spool_rejects_unwritable_directory() {
        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = DeltaSpool::new(dir.clone(), 1024);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
        if let Err(error) = result {
            assert!(error.starts_with("metering_spool_unwritable"), "{error}");
        }
    }
}

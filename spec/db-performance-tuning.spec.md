# Database Performance Tuning Specification

## 0. Status

- **Purpose:** Reduce SQLite write contention and query latency through in-memory batching, caching, and PRAGMA tuning.
- **Scope:** Applies to the `db_cache` module and its integration with `UserStore`. Flush-to-database behaviors (DPT-LU3/DPT-LU6, DPT-RL4) apply to the `primary` node role; on a `replica` they are replaced by the shipment pipeline of `primary-replica-deployment.spec.md` M4–M5 with buffering semantics preserved (PRP12 there). Replica `ship_via` MUST use the same on-disk `.json` discovery as DPT-RL4 even when the in-memory buffer is empty.
- **Dependencies:** `dashmap` (concurrent hash map), `tokio` (async runtime).

## 1. Module Structure

DPT1. All performance-tuning constructs MUST reside in `src/db_cache.rs`, exported as `pub mod db_cache` from the crate root.

DPT2. The module MUST expose these public runtime types:
- `LastUsedBatcher`
- `RequestLogBatcher`
- `ApiKeyCache`
- `BalanceCache`

The module MUST also expose `RequestLogAdmissionError` so forwarding preflight and terminal finalization can distinguish spool quota exhaustion from spool unavailability.

## 2. LastUsedBatcher

### 2.1 State

DPT-LU1. `LastUsedBatcher` MUST hold a `DashMap<String, DateTime<Utc>>` keyed by `api_key_id`.

DPT-LU2. When `record(api_key_id, timestamp)` is called for an existing entry, the buffer MUST retain the later of the existing and supplied timestamps. A failed flush MUST reinsert the failed timestamp without replacing a newer concurrently recorded timestamp.

DPT-LU2a. The distinct-key capacity MUST be configurable. The default is `10000`, selected by `MONOIZE_LAST_USED_BUFFER_ENTRIES`. When the buffer is full, an update to an existing key remains accepted and a previously unseen key MAY be omitted because `last_used_at` is non-billing metadata. Every omission MUST increment the counter `monoize_last_used_buffer_dropped_total` by one. The first omission in a saturation episode MUST also emit one `warn`-level log record carrying the capacity, and a later omission MUST emit a new warning only after a `record` call observes the buffer below capacity. A buffer that never drains MUST NOT emit more than one warning.

### 2.2 Flush

DPT-LU3. `flush(db)` MUST atomically drain all buffered entries (via `retain(|_,_| false)`) and update them through bounded bulk `UPDATE ... CASE` statements within a single write-lock acquisition. The default chunk size is `256`, selected by `MONOIZE_LAST_USED_FLUSH_CHUNK_ENTRIES` and clamped to `[1, 400]` so each portable statement uses at most 800 bound values. Flush MUST execute at most one database round trip per non-empty chunk, not one round trip per key.

DPT-LU4. If one bulk UPDATE fails, the error MUST be logged at `warn` level and every entry in that chunk MUST be returned to the buffer for a later flush. The flush MUST continue processing remaining chunks.

DPT-LU5. If the buffer is empty at flush time, the method MUST return immediately without acquiring a write lock.

### 2.3 Background Task

DPT-LU6. `spawn_flush_task(db, interval)` MUST spawn a tokio task that calls `flush` at the given interval. The interval ticker MUST use `MissedTickBehavior::Delay`.

DPT-LU7. The default flush interval (as configured in `UserStore::spawn_background_tasks`) MUST be 30 seconds.

## 3. RequestLogBatcher

### 3.1 State

DPT-RL1. `RequestLogBatcher` MUST hold at most a configurable number of in-memory spool references. The default is `128`, selected by `MONOIZE_REQUEST_LOG_BUFFER_ENTRIES`.

DPT-RL2. Each accepted entry MUST be serialized to a unique file in the request-log spool before `push` reports success. `RuntimeConfig.request_log_spool_dir: Option<PathBuf>` MUST select the spool directory when it is `Some(path)`. When it is `None`, `RequestLogBatcher` MUST preserve the existing resolution order: use `MONOIZE_REQUEST_LOG_SPOOL_DIR` when that variable exists, otherwise use `./data/request-log-spool`. `RuntimeConfig::from_env()` MUST set this field to `None`; production therefore keeps the existing environment/default behavior. Two `AppState` instances with distinct explicit paths MUST not discover, flush, or account for each other's spool files.

### 3.2 Push

DPT-RL3. `push(log)` MUST assign the stable database row UUID, durably publish one spool file by temporary-file write followed by same-directory atomic rename, and then MAY append an in-memory reference under the mutex. It MUST return `Result` so the caller can fail closed when persistence is unavailable.

DPT-RL3a. The spool byte quota MUST be configurable. The default is `536870912`, selected by `MONOIZE_REQUEST_LOG_SPOOL_MAX_BYTES`. One serialized entry MUST be no larger than `8388608` bytes by default, selected by `MONOIZE_REQUEST_LOG_SPOOL_ENTRY_MAX_BYTES`. Terminal-log preflight MUST reject `MONOIZE_REQUEST_LOG_SPOOL_ENTRY_MAX_BYTES` values below `4096` with `RequestLogAdmissionError::EntryQuotaTooSmall`; no reservation or admission marker may be created for that configuration.

DPT-RL3b. If accepting an entry would exceed either quota, or its one durable spool write attempt fails, unreserved `push` MUST return an error. It MUST NOT broadcast the log or remove its pending snapshot. `push` is for bounded internal producers, MUST NOT degrade an oversized entry, and MUST NOT retry indefinitely.

DPT-RL3c. `reserve_terminal_log()` MUST atomically reserve one `MONOIZE_REQUEST_LOG_SPOOL_ENTRY_MAX_BYTES` unit against the sum of durable spool bytes and outstanding reservations. Concurrent reservations MUST NOT oversubscribe the spool quota. The reservation MUST preassign one stable spool-row UUID and one stable final `.json` path. Before returning success it MUST create, sync, and atomically publish a unique unarmed write-probe marker in the spool directory and sync that directory. On Windows, directory sync MUST open the directory with write access and `FILE_FLAG_BACKUP_SEMANTICS` before calling `FlushFileBuffers`. Dropping the final clone of an unarmed reservation MUST remove the probe and release the reservation.

DPT-RL3c-1. Before upstream dispatch, `arm_reserved(fallback_log, reservation)` MUST atomically replace the unarmed marker with a valid, durably synced `SpoolRequestLog` fallback that uses the reservation's stable UUID. The fallback MUST have terminal `status = "error"`, identify the admitted request, and state that terminal finalization was interrupted. Arming MUST fail unless the reservation belongs to that batcher, is unclaimed, and is unarmed. `push_reserved` MUST fail unless arming completed.

DPT-RL3c-2. Exactly one `push_reserved(log, reservation)` call MAY claim an armed reservation. It MUST encode the terminal log with the reservation's stable UUID and durably rename it to the reservation's stable final path. After claim, a filesystem write, sync, or rename failure MUST leave the armed fallback and reservation active and MUST retry the same stable UUID and final path until durable rename succeeds. Retry delay MUST start at `10 ms`, double after each failed attempt, and stop increasing at `1000 ms`. `push_reserved` MUST release `flush_lock` before each retry sleep. After durable rename, the reservation MUST remove the armed marker, convert the full reservation to the serialized terminal entry's actual byte count, and release unused reserved bytes.

DPT-RL3c-3. Dropping the final clone of an armed or claimed reservation before terminal completion MUST preserve the fallback by atomically promoting its marker to the stable final `.json` path. If promotion succeeds, quota accounting MUST convert the reservation to the fallback file's actual byte count. If promotion fails, the marker MUST remain on disk and the process MUST retain the conservative full reservation.

DPT-RL3c-4. `cancel_reserved(reservation)` MAY cancel an armed but unclaimed reservation for a producer whose completed outcome is specified not to generate a row. Cancellation MUST exclusively transition the reservation out of the claimable state, then retry until the marker is absent and the spool directory sync succeeds. Retry delay MUST start at `10 ms`, double after each failed attempt, and stop increasing at `1000 ms`. It MUST mark the reservation consumed and release its full quota only after durable cancellation. An unarmed, claimed, consumed, or foreign reservation MUST be rejected. The caller MUST await cancellation before discarding the lifecycle.

DPT-RL3d. `push(log)` without a preflight reservation is reserved for internal producers such as active probes and tests. It MUST atomically reserve the serialized entry's actual byte count before writing and MUST obey the same combined quota.

DPT-RL3e. `arm_reserved` and `push_reserved` MUST serialize the complete entry. They MUST NOT replace the entry with a compact representation or generate `error_code = "request_log_payload_truncated"`. If the complete encoding exceeds the reservation, the operation MUST return `RequestLogAdmissionError::EntryTooLarge`. An `arm_reserved` size failure MUST occur before the reservation enters the armed state. A `push_reserved` size failure MUST leave the armed fallback available for durable promotion. The operation MUST NOT persist a partial terminal entry.

DPT-RL3f. A successful local spool-file publication MUST remain serialized by `flush_lock` until `spool_bytes`, `admitted_bytes`, and the in-memory spool reference reflect that file. `flush` MUST NOT delete a locally published file between its final rename and its accounting transition.

### 3.3 Flush

DPT-RL4. `flush(db)` MUST select a bounded batch of durable spool files and open one database transaction. It MUST partition the selected entries into consecutive chunks of at most `20` rows. Each non-empty chunk MUST execute exactly one multi-row `INSERT INTO request_logs (...) VALUES (...), (...) ON CONFLICT(id) DO NOTHING` statement. Each row MUST bind exactly the `42` request-log columns, so one statement binds at most `840` values and remains below the portable SQLite `999`-variable ceiling. Placeholder numbering MUST be contiguous across every row in one statement and MUST restart at `$1` for each chunk. The transaction MUST commit only after every chunk succeeds.

DPT-RL5. Each INSERT MUST use the stable UUID stored in the spool entry and the log entry's captured `created_at` value instead of flush time.

DPT-RL6. If transaction begin, any INSERT, or commit fails, the selected spool files MUST remain available for a later retry and the failure MUST be logged at `warn` level.

DPT-RL7. After a successful commit, the selected spool files MUST be deleted. An ambiguous commit outcome is safe because retries use `ON CONFLICT(id) DO NOTHING` with the stable UUID.

DPT-RL7a. Removing a committed spool file MUST subtract its size from `spool_bytes` and `admitted_bytes` without unsigned wraparound. If a file was published outside the current batcher's accounting window, subtraction MUST stop at zero. Such a file MUST NOT make later admission fail with a false quota-exhausted state.

### 3.4 Background Task

DPT-RL8. `spawn_flush_task(db, interval)` MUST spawn a tokio task that calls `flush` at the given interval. The interval ticker MUST use `MissedTickBehavior::Delay`.

DPT-RL9. The default flush interval (as configured in `UserStore::spawn_background_tasks`) MUST be 2 seconds.

### 3.5 Crash recovery

DPT-RL10. Spool files left by an abrupt process exit MUST be discovered and retried by a later process using the same spool directory. Memory-buffer capacity MUST NOT limit recovery of on-disk entries. Startup MUST delete legacy constant-content and unarmed write-probe admission markers. Startup MUST parse every armed admission marker as a bounded `SpoolRequestLog`; if its stable final path is absent, startup MUST atomically promote the marker to that path and sync the directory. If the final path already exists, startup MUST retain the final file and remove the duplicate marker. Startup MUST fail without deleting a non-legacy admission marker that is neither a valid fallback nor an already-published final row.

## 4. ApiKeyCache

### 4.1 State

DPT-AK1. `ApiKeyCache` MUST hold a `DashMap<String, CachedApiKeyEntry>` keyed by the complete API key string. The first 12 characters MUST NOT be used as a cache identity.

DPT-AK2. Each `CachedApiKeyEntry` MUST contain:
- `api_key: ApiKey` (the full API key record)
- `user: User` (the owning user record)
- `cached_at: Instant` (wall-clock timestamp at insertion)
- `generation: u64` (the cache invalidation generation observed before the database read)

DPT-AK3. The TTL (as configured in `UserStore::new`) MUST be 60 seconds.

DPT-AK3a. Entry capacity MUST be configurable. The default is `10000`, selected by `MONOIZE_API_KEY_CACHE_CAPACITY`. Insertion at capacity MUST evict an existing entry before publishing the new entry.

### 4.2 Lookup

DPT-AK4. `get(key)` MUST return `Some((ApiKey, User))` if and only if:
1. An entry exists for the complete key, AND
2. `cached_at.elapsed() <= ttl`, AND
3. The entry generation equals the current invalidation generation.

DPT-AK5. If an entry exists but `cached_at.elapsed() > ttl`, the cache MUST remove the entry only if the currently stored entry is still expired at removal time (conditional remove), and then return `None`.

### 4.3 Security Invariant

DPT-AK6. A cache hit MUST NOT bypass full-key plaintext verification. The caller (`validate_api_key`) MUST still compare the supplied complete key with `cached_key.key` on every cache-hit path.

DPT-AK7. A cache hit MUST additionally verify that `cached_key.enabled == true`, `cached_user.enabled == true`, and the key is not expired (`expires_at > now` or `expires_at` is None). If any check fails, the entry MUST be invalidated and the caller MUST fall through to the database path.

### 4.4 Insertion

DPT-AK8. A cache-miss database read MUST capture the current invalidation generation before reading. The result MUST be inserted only while that generation remains current.

DPT-AK8a. After insertion, the cache MUST read the generation again. If it changed, the cache MUST conditionally remove the entry whose stored generation equals the stale generation and report insertion failure. The caller MUST repeat database validation instead of returning that stale result.

### 4.5 Invalidation

DPT-AK9. The cache MUST maintain reverse indexes from API-key ID and user ID to complete-token cache keys. The following invalidation methods MUST exist:
- `invalidate_by_key_id(key_id)`: Remove entries named by the API-key-ID reverse index.
- `invalidate_by_user_id(user_id)`: Remove entries named by the user-ID reverse index.
- `invalidate_by_key_ids(key_ids)`: Remove entries named by the API-key-ID reverse indexes.
- `invalidate(key)`: Remove the entry for the complete API key.
- `invalidate_all()`: Clear the entire cache.

DPT-AK9a. Every explicit invalidation MUST increment the cache generation before removing entries. A database result read before that increment MUST NOT be published afterward.

DPT-AK10. Invalidation MUST be called on the following mutation paths:

| Mutation | Invalidation Method |
|---|---|
| `delete_api_key(id)` | `invalidate_by_key_id(id)` |
| `update_api_key(key_id, input)` | `invalidate_by_key_id(key_id)` |
| `batch_delete_api_keys(ids)` | `invalidate_by_key_ids(ids)` |
| `delete_user(id)` | `invalidate_by_user_id(id)` |
| `update_user(id, ..., any persisted field changed, ...)` | `invalidate_by_user_id(id)` |
| `update_last_login(id)` | `invalidate_by_user_id(id)` after the database write succeeds |
| `decrement_api_key_quota(api_key_id)` | `invalidate_by_key_id(api_key_id)` |

DPT-AK11. `update_user` MUST invalidate the API key cache whenever the update modifies any persisted user field.

DPT-AK12. `decrement_api_key_quota(api_key_id)` MUST invalidate API key cache entries for that key via `invalidate_by_key_id(api_key_id)` after the quota update executes.

DPT-AK13. `ApiKeyCache` MUST provide a background eviction task that periodically removes expired entries using `retain`.

DPT-AK14. Explicit ID/user invalidation MUST NOT scan the complete-token cache. Eviction and every removal path MUST remove corresponding reverse-index membership. Stale empty reverse-index sets MUST be removed.

DPT-AK15. `delete_user(id)` MUST execute the following operations in one write transaction: lock the matching user row, reject a missing user, delete that user, verify that exactly one user row was deleted, and commit. It MUST NOT query or materialize the deleted user's API-key IDs. After commit it MUST call `ApiKeyCache::invalidate_by_user_id(id)` and `BalanceCache::invalidate(id)` and return `Result<(), String>`.

## 5. BalanceCache

### 5.1 State

DPT-BC1. `BalanceCache` MUST hold a `DashMap<String, CachedBalanceEntry>` keyed by `user_id`.

DPT-BC2. Each `CachedBalanceEntry` MUST contain:
- `balance: UserBalance`
- `cached_at: Instant`
- `generation: u64`

DPT-BC3. The TTL (as configured in `UserStore::new`) MUST be 30 seconds.

DPT-BC3a. Entry capacity MUST be configurable. The default is `10000`, selected by `MONOIZE_BALANCE_CACHE_CAPACITY`. Insertion at capacity MUST evict an existing entry before publishing the new entry.

### 5.2 Lookup

DPT-BC4. `get(user_id)` MUST return `Some(UserBalance)` if and only if:
1. An entry exists for the given user_id, AND
2. `cached_at.elapsed() <= ttl`, AND
3. The entry generation equals the current invalidation generation.

DPT-BC5. If an entry exists but `cached_at.elapsed() > ttl`, the cache MUST remove the entry only if the currently stored entry is still expired at removal time (conditional remove), and then return `None`.

### 5.3 Insertion

DPT-BC6. `get_user_balance(user_id)` MUST check the cache first. On cache miss, it MUST capture the current invalidation generation, query the database, and insert the result only while that generation remains current.

DPT-BC6a. After insertion, the cache MUST read the generation again. If it changed, the cache MUST conditionally remove the entry whose stored generation equals the stale generation and `get_user_balance` MUST repeat the database read.

### 5.4 Invalidation

DPT-BC7. The following invalidation methods MUST exist:
- `invalidate(user_id)`: Remove the entry for the given user_id.
- `invalidate_all()`: Clear the entire cache.

DPT-BC7a. Every explicit invalidation MUST increment the cache generation before removing entries. A database result read before that increment MUST NOT be published afterward.

DPT-BC8. Invalidation MUST be called on the following mutation paths:

| Mutation | Invalidation Method |
|---|---|
| `charge_user_balance_nano_inner(user_id, ...)` | `invalidate(user_id)` — after transaction commit |
| `admin_adjust_user_balance(user_id, ...)` | `invalidate(user_id)` — after transaction commit |
| `update_user(id, ..., balance_nano_usd=Some(_), ...)` | `invalidate(id)` |
| `update_user(id, ..., balance_unlimited=Some(_), ...)` | `invalidate(id)` |
| `delete_user(id)` | `invalidate(id)` |

DPT-BC9. `update_user` MUST invalidate the balance cache only when `balance_nano_usd` or `balance_unlimited` is being changed. Other user field updates MUST NOT trigger balance cache invalidation.

### 5.5 Staleness Bound

DPT-BC10. After a balance mutation commits and its same-process invalidation completes, later cache reads MUST NOT return a value read before that invalidation. The TTL remains 30 seconds for changes made outside this process.

### 5.6 UsageReadCache

DPT-UR1. `UsageReadCache` MUST cache persisted balance lookups for the read-only balance surfaces `GET /api/codex/usage` and `GET /user/balance`. The TTL MUST be 3 seconds.

DPT-UR2. `UsageReadCache` MUST NOT be used by any path that admits or rejects a request, and MUST NOT be used for spending, quota, or settlement decisions. Those paths MUST read the balance directly. A balance returned by `UsageReadCache` is display-only and MAY be up to 3 seconds stale.

DPT-UR3. Concurrent lookups for the same user MUST collapse into at most one in-flight database read. A lookup that arrives while another lookup for the same user is in flight MUST wait for that read and MUST use its result.

DPT-UR4. Errors MUST NOT be cached. A failed database read MUST NOT be replayed to later lookups within the TTL window.

DPT-UR5. Entry capacity MUST be bounded. At capacity, insertion MUST clear existing entries before publishing the new entry.


DPT-BC11. `BalanceCache` MUST provide a background eviction task that periodically removes expired entries using `retain`.

## 6. UserStore Integration

### 6.1 Construction

DPT-US1. `UserStore::new(db)` MUST construct all five subsystems:
- `LastUsedBatcher::new()`
- `RequestLogBatcher::new(128)`
- `ApiKeyCache::new(Duration::from_secs(60))`
- `BalanceCache::new(Duration::from_secs(30))`
- `UsageReadCache::new()`

### 6.2 Lifecycle

DPT-US2. `spawn_background_tasks()` MUST be called after `UserStore` construction (during application startup, after `load_state()`). It MUST spawn:
- flush task for `LastUsedBatcher` (30s interval),
- flush task for `RequestLogBatcher` (2s interval),
- eviction task for `ApiKeyCache` (30s interval),
- eviction task for `BalanceCache` (30s interval),
- expired-session cleanup task using the DPT-US10 interval.

DPT-US3. `flush_all_batchers()` MUST be called during application shutdown. It MUST flush both `LastUsedBatcher` and `RequestLogBatcher` to ensure buffered data is persisted.

DPT-US4. `flush_all_batchers()` MUST be called in two shutdown paths:
1. Inside the `shutdown_signal` handler (after signal receipt, before graceful shutdown completes).
2. After `axum::serve` returns (to catch any data buffered during the drain period).

### 6.3 validate_api_key Integration

DPT-US5. `validate_api_key(key)` MUST follow this execution path:
1. If `key.len() < 12`, return `None`.
2. Check `ApiKeyCache::get(key)` using the complete key.
3. On cache hit: verify `enabled`, `user.enabled`, `expires_at`, and `key != cached_key.key` (plaintext equality). If all pass, call `last_used_batcher.record(...)` and return the cached result. If any check fails, invalidate the cache entry and MUST immediately revalidate via the database path in the same call; cache validation failure alone MUST NOT produce an authentication error response.
4. On cache miss: capture the cache generation, query the database by complete key equality, verify enabled/expired/key-equality/user, and insert into `ApiKeyCache` only if the generation remains current.
5. If publication fails because the generation changed, repeat step 2. Otherwise call `last_used_batcher.record(...)` and return.

### 6.4 Request Log Integration

DPT-US6. `insert_request_log_pending()`, `update_pending_request_log_channel()`, and `update_pending_request_log_usage()` MUST be no-op stubs (return `Ok(())` immediately with no DB interaction).

DPT-US7. `finalize_request_log(log)` and `insert_request_log(log)` MUST push the `InsertRequestLog` to `RequestLogBatcher` instead of performing direct DB writes.

DPT-US8. `cleanup_pending_request_logs()` MUST remain functional and continue to transition any `status = "pending"` rows to `status = "error"`. This handles the edge case where the process crashes after a previous version created pending rows, or during the data-loss window between `push` and `flush`.

### 6.5 Session Retention

DPT-US9. `cleanup_expired_sessions()` MUST execute one set-based `DELETE FROM sessions WHERE expires_at <= threshold` statement and return the number of deleted rows. The threshold MUST be the current UTC time encoded with the same RFC3339 representation used by `create_session`. The cleanup MUST NOT first query or materialize session rows.

DPT-US10. `UserStore` construction MUST complete one DPT-US9 cleanup before returning. `spawn_background_tasks()` MUST repeat DPT-US9 after each configured interval. `MONOIZE_SESSION_CLEANUP_INTERVAL_SECONDS` MUST select a positive whole-second interval. Its default MUST be `3600`; a missing, empty, zero, negative, invalid, or overflowing value MUST select the default.

## 6.1 Query indexing

DPT-IDX1. Every list endpoint that reads one page of a user-scoped table ordered by time
MUST be served by an index whose leading column is the scoping column and whose remaining
columns match the `ORDER BY` in both sequence and direction. An index on the scoping column
alone is not sufficient: SQLite locates the rows but then materialises all of them into a
temporary B-tree to sort, so the cost grows with the user's total row count rather than with
the page size.

DPT-IDX2. `billing_ledger` MUST carry `(user_id, created_at DESC, id DESC)`, matching the
wallet ledger query. Measured on production data before this index, the busiest account held
74,523 rows and one page cost 137 ms serially, degrading to 9 requests per second at 100
concurrent callers while endpoints not reading this table served 2,400 per second on the
same process.

DPT-IDX3. `request_logs` MUST carry `(user_id, created_at)` for the same reason.

## 7. SpendWindowCache

### 7.1 Purpose

DPT-SW0. `SpendWindowCache` exists so that forwarding preflight does not run the
`request_logs` window aggregates (`load_key_windows`, `load_limit_levels`) on every
request. Those aggregates scan the key's or org's full `request_logs` history and are
the dominant read-pool load under burst traffic.

### 7.2 State

DPT-SW1. Two typed `SpendWindowCache` instances MUST exist:
- A key-level cache: `DashMap` keyed by `key:{api_key_id}`, value `Option<OrgSpendWindows>`
  (a cached `None` means the key row does not exist and MUST also be cached).
- An org-level cache: `DashMap` keyed by `org:{org_id}:{member_id}:{api_key_id}`, value
  `OrgLimitLevels`.

Each entry MUST carry `cached_at: Instant`.

DPT-SW2. The TTL MUST be 5000 ms by default, selected by `MONOIZE_SPEND_WINDOW_CACHE_TTL_MS`.
A value of `0` MUST disable caching (every lookup loads from the database). Parsing
follows RRB-C1 except that `0` is meaningful.

DPT-SW3. Entry capacity MUST be 10000 by default, selected by
`MONOIZE_SPEND_WINDOW_CACHE_CAPACITY`. Insertion at capacity MUST evict before publishing.

### 7.3 Single-flight

DPT-SW4. Concurrent lookups for the same cache key MUST collapse into at most one
database load. A lookup that arrives while a load for the same key is in flight MUST
await that load and use its result. After the awaited load completes, the waiter MUST
re-check the cache before loading again.

### 7.4 Semantics

DPT-SW5. Cached values MUST be used only by forwarding admission preflight
(`ensure_balance_before_forward` and its replica equivalent). Settlement, balance
mutation, quota reservation, and admin views MUST read the database directly and MUST
NOT consult this cache.

DPT-SW6. A load error MUST NOT be cached. The error MUST propagate to the caller, which
keeps its existing fail-open behavior.

DPT-SW7. The admitted staleness bound is the TTL: preflight may evaluate a window that
excludes spend settled within the last `TTL` milliseconds. This is safe because
(a) settlement re-evaluates the exact windows against fresh data and records breaches,
and (b) the in-flight spend ceiling (`InFlightSpend`) bounds how far one request can
overdraft beyond a cached negative.

DPT-SW8. After a successful wallet charge or plan settlement for api key `K` of user
`U`, the cache MUST invalidate `key:{K}` and every `org:{*}:{*}:{K}` entry, and every
`org:{O}:*` entry where `O` is an org of `U`, best-effort. Invalidation accelerates
convergence; correctness relies on DPT-SW7, not on invalidation.

DPT-SW9. Mutations that change `api_keys.spend_limit_*` columns or org spend limits
MUST invalidate the affected `key:{id}` / `org:{org_id}:*` entries after the database
write succeeds.

## 8. Registry Snapshot Cache

### 8.1 Purpose

DPT-RR0. The routing hot path reads the provider/channel/model registry
(`monoize_providers`, `monoize_provider_models`, `monoize_groups`) on every request.
These tables change only through admin mutation. A process-local snapshot serves
hot-path reads.

### 8.2 State

DPT-RR1. A registry snapshot MUST contain: the full `list_providers()` result (ordered
by priority ASC, created_at ASC), the full `load_channels_bulk(None)` result keyed by
provider id, and the group-id to account-class mapping from `monoize_groups`. The
served reads are: `list_providers()`, `list_providers_by_account_class()`,
`available_model_names()`, `available_model_names_for_account_class()`, and
`load_channels_bulk(None)`. Model-registry and billing-rate lookups remain direct
indexed point reads; they are outside this snapshot.

DPT-RR2. Snapshot refresh MUST trigger when either:
1. The registry generation counter changes, or
2. The snapshot age exceeds `MONOIZE_REGISTRY_SNAPSHOT_TTL_MS` (default 60000; `0`
   selects refresh-on-every-read).

DPT-RR3. Every write to `monoize_providers` or `monoize_provider_models` (provider
create/update/delete/reorder) and every write to `monoize_groups` (group
create/update/reorder/delete) MUST increment the registry generation after the write
commits. The generation counter is process-wide.

DPT-RR3a. A successful nonempty Provider reorder MUST increment the registry
generation after committing the priority updates and before returning success.
The next Provider list read from any store in that process MUST refresh its cached
order and priorities. An empty reorder performs no registry write and need not
increment the generation.

DPT-RR4. Snapshot rebuilds MUST serialize on one mutex per shared snapshot state.
A reader MAY reuse an installed snapshot only when its generation equals the
process-wide generation observed at the check and its age does not exceed the TTL.
TTL `0` MUST prevent reuse. A reader that requires refresh MUST wait for the rebuild
mutex and repeat this check after acquiring it. If no installed snapshot passes the
second check, that reader MUST rebuild and install a new snapshot before releasing
the mutex.

DPT-RR5. A failed rebuild MUST return its error to the calling reader and leave the
previous snapshot installed. The reader MUST NOT return the previous snapshot as a
substitute for that error. The next reader MUST retry the failed rebuild. A generation
change during a rebuild MUST make the resulting snapshot require refresh on the next
read; it MUST NOT be recorded as a generation already incorporated by that rebuild.

DPT-RR6. The snapshot holds whole tables whose row counts are bounded by admin input
(thousands). No per-request allocation growth is permitted.

### 8.3 Placement

DPT-RR7. The snapshot MUST live in the routing store layer (`MonoizeRoutingStore`) so
every caller of the covered read functions shares one snapshot.

### 8.4 Validation

DPT-RR8. Tests that mutate registry rows through direct SQL MUST increment the
registry generation after the write commits and before asserting refreshed read
results. Validation MUST include a corruption test that starts with an installed
valid snapshot, requires each attempted rebuild to return its decoding error, and
verifies that the installed snapshot remains unchanged. After a valid repair commits
and increments the generation, the next read MUST rebuild successfully and replace
that snapshot.

## 9. Dashboard Aggregate Cache

DPT-DA1. The following dashboard reads MUST be served through a server-side result
cache: admin usage ranking (per user and per model, plus the totals and global model
ranking it embeds), the admin overview's embedded aggregates (usage ranking, today
totals, channels-today), bucketed dashboard analytics, and cache-hit-rate by users.
The revenue report reads pre-aggregated rollup tables plus a bounded live-day window
and is outside this cache.

DPT-DA2. The cache key MUST be the endpoint identity plus the serialized query
parameters. The TTL MUST be 10000 ms by default, selected by
`MONOIZE_DASHBOARD_AGG_CACHE_TTL_MS`; `0` disables caching. Capacity MUST be 256
entries by default, selected by `MONOIZE_DASHBOARD_AGG_CACHE_CAPACITY`; insertion at
capacity MUST evict before publishing.

DPT-DA3. Only successful query results MUST be cached. Errors propagate uncached.

DPT-DA4. Cache entries hold the serialized response payload. A cache hit MUST not
re-run the aggregate query.

## 10. Concurrency Properties

DPT-C1. `LastUsedBatcher` and `ApiKeyCache` use `DashMap` with one read/write lock per shard. Reads may proceed concurrently; a write excludes reads and writes on the same shard.

DPT-C2. `RequestLogBatcher` uses `tokio::sync::Mutex` for the buffer. The `push` operation holds the lock only for the duration of `Vec::push`. The `flush` operation holds the lock only for the duration of `std::mem::replace` (buffer swap), then releases it before executing DB writes.

DPT-C3. `BalanceCache` uses `DashMap` with the same concurrency properties as DPT-C1.

DPT-C4. Capacity eviction in `LastUsedBatcher`, `ApiKeyCache`, `BalanceCache`, `SpendWindowCache`, and `DashboardAggCache` MUST copy the selected key and release every iterator and entry guard before removing that key. With no other caller holding a shard lock, insertion or retry at capacity MUST complete without waiting for a lock held by that same operation.

# Primary/Replica Deployment Specification

## 0A. LynShen public status source

PR-MIG-1. Public status MUST use the `request_logs` rows already shipped by the Replica
metering pipeline. It MUST NOT add a fourth shipment data class.

PR-MIG-2. Primary and Replica request-log durability, shipment, replay, and preflight MUST
follow `request-logs.spec.md`, `db-performance-tuning.spec.md`, and this specification.
Public status MUST NOT add a separate dispatch semaphore or status spool.

PR-MIG-3. Existing per-Channel proxy, header, affinity, probe, and missing-usage behavior
applies to the singular embedded Channel stored on its Provider after migration. Schema
rules that add these values to `monoize_channels` describe only their historical source;
the destructive migration moves the surviving values to `monoize_providers.channel_*`.

## 0. Status

- Product name: Monoize.
- Internal protocol name: `URP-Proto`.
- Scope: optional multi-node deployment consisting of exactly one node in role `primary` and zero or more nodes in role `replica`, sharing one PostgreSQL database. Also defines the node-local upstream proxy configuration that applies to every node regardless of role.
- Out of scope: automatic leader election, SQLite-backed replicas, multi-primary writes, cross-region replication, database-level replication tooling.
- Terminology: "primary" is the node role whose process is the only writer of Monoize business tables; "replica" is a node role whose process serves traffic without writing business tables. A deployment has exactly one primary; the assignment is manual (section 9).

## 1. Role selection and validation

PRP1. The node role MUST be resolved from the `MONOIZE_NODE_ROLE` environment variable. Accepted values are `primary` and `replica` (ASCII, case-sensitive). An absent or empty value MUST resolve to `primary`. Any other value MUST stop startup with error `node_role_invalid`.

PRP2. Role and all related settings are resolved once at startup and are immutable for the lifetime of the process. No runtime endpoint MAY change them.

PRP3. A replica node MUST reject a SQLite DSN (`sqlite://...`, `sqlite::memory:`) at startup with error `replica_requires_postgres`.

PRP4. A replica node MUST require `MONOIZE_PRIMARY_INTERNAL_URL`. The value MUST be a valid absolute `http://` or `https://` URL; otherwise startup MUST fail with error `replica_primary_url_required`.

PRP5. A replica node MUST require `MONOIZE_REPLICA_TOKEN`. An absent or empty value MUST stop startup with error `replica_token_required`. A configured value with fewer than 32 Unicode scalar values MUST stop startup with error `replica_token_too_short` on every node role.

PRP6. A primary node with a valid `MONOIZE_REPLICA_TOKEN` MUST mount the metering ingest endpoint defined in section 7. A primary without it runs in single-node compatibility mode: the ingest endpoint MUST NOT be mounted, and requests to its path MUST return 404.

PRP7. Tuning variables, each read once at startup:

| Variable | Default | Constraint | Error on violation |
|---|---|---|---|
| `MONOIZE_CONFIG_POLL_INTERVAL_SECONDS` | `5` | positive integer | `config_poll_interval_invalid` |
| `MONOIZE_METERING_SHIP_INTERVAL_SECONDS` | `10` | positive integer | `metering_ship_interval_invalid` |
| `MONOIZE_METERING_SHIP_BATCH_MAX_ENTRIES` | `500` | integer in `[1, 2000]` | `metering_batch_limit_invalid` |
| `MONOIZE_REPLICA_METERING_SPOOL_DIR` | `./data/replica-metering-spool` | filesystem path | n/a |
| `MONOIZE_REPLICA_METERING_SPOOL_MAX_BYTES` | `536870912` | positive integer | `metering_spool_quota_invalid` |

A malformed value outside the stated constraint MUST stop startup with the listed error code.

## 2. Node-local upstream proxy

PX1. Every node, regardless of role, MAY configure one outbound HTTP proxy via the `MONOIZE_UPSTREAM_PROXY_URL` environment variable. Primary and replica values are independent because the variable is process-local.

PX2. When `MONOIZE_UPSTREAM_PROXY_URL` is set to a non-empty value, it MUST be an absolute URL with scheme `http` or `https`. Any other scheme (including `socks5`) MUST stop startup with error `upstream_proxy_config_invalid`. An absent or empty value MUST mean direct connection.

PX3. When configured, the shared outbound client used for external upstream calls (LLM provider forwarding, models.dev catalog sync) on that node MUST route all its requests through this proxy.

PX4. Internal cluster traffic — the replica metering shipment of section 6 — MUST NOT use the configured upstream proxy. The client performing this traffic MUST be constructed to bypass both the configured proxy and environment-inherited proxies.

PX5. The upstream proxy configuration MUST NOT be stored in `system_settings` and MUST NOT be editable from the dashboard. Changing it requires a process restart.

PX6. Per-Channel egress proxy resolution: for one upstream request issued for Channel `c`, the effective proxy is `c.proxy_url` when it is a non-empty custom URL (channel-management.spec.md CP-INV-14), otherwise the node-level `MONOIZE_UPSTREAM_PROXY_URL`, otherwise direct connection. The same resolution applies to active-probe requests for that Channel. If a custom Channel `proxy_url` cannot be used to construct an HTTP client, that Channel's request (including an active-probe) MUST fail closed; it MUST NOT fall back to the node-global or direct client.

PX7. The application MUST cache one HTTP client per distinct effective proxy URL (including the direct case) instead of constructing a client per request. Cache entries are immutable after construction; channel `proxy_url` changes take effect by resolving a different cached client on the next request.

PX7a. Before inserting an uncached custom URL, if the custom-proxy cache currently contains at least 64 entries, the application MUST select one arbitrary cached key and attempt to remove it before publishing the new client. Eviction MUST release the iterator's shard read lock before removing its selected key. Concurrent insertions MAY temporarily exceed 64 entries. Eviction MUST NOT terminate requests that already hold a clone of the evicted client.

PX8. The metering shipment of section 6 always resolves to the no-proxy internal client regardless of any Channel `proxy_url`.

## 3. Startup behavior

### 3.1 Primary

PRP8. Startup on a primary MUST remain exactly as specified by `database-configuration.spec.md` DB16–DB19 and `unified_responses_proxy.spec.md` C-series: connect, run migrations, ensure defaults, construct stores, spawn background tasks.

PRP9. If the metering spool directory (PRP7) contains durable delta entries left over from an earlier replica life of this data directory, the primary MUST drain them through the idempotent apply routine of section 7 against the local database after migrations complete and before the listener accepts requests. A drain failure MUST stop startup with error `metering_drain_failed`. Request-log spool leftovers need no special handling because the normal flush path consumes them (DPT-RL4).

### 3.2 Replica

PRP10. A replica MUST NOT execute any migration. After `DbPool::connect()`, it MUST evaluate the same applied-versus-embedded version comparison as DB16a–DB16d in read-only form:

- outcome equivalent to DB16b's "migrations needed" state ⇒ startup MUST fail with error `replica_schema_pending`;
- outcome equivalent to DB16c acceptance (rollback binary ahead) ⇒ startup MAY continue;
- fully-applied state ⇒ continue.

PRP11. On a replica the following write-producing startup steps MUST NOT run: `SettingsStore` default insertion and transform-rule-id canonicalization writes, `ensure_active_probe_system_user`, session-expired cleanup deletion, request-log retention deletion, and the active-probe scheduler task.

PRP12. On a replica, `RequestLogBatcher` and `LastUsedBatcher` MUST keep identical buffering semantics, but their periodic flush MUST target the metering shipper (section 6) instead of a local database write. `ApiKeyCache` and `BalanceCache` eviction tasks run unchanged because they only evict in-memory entries.

PRP13. All other initialization order (stores, runtime snapshot build, router assembly, listener bind) MUST be identical between roles.

## 4. Configuration epoch and runtime refresh

E1. The persistent epoch is the single row of `state_records` with `tenant_id = 'monoize'`, `kind = 'config_epoch'`, `id = 'global'`. Its `value` is a base-10 unsigned 64-bit decimal text. A missing row MUST be read as epoch `0`.

E2. The primary MUST increment the epoch by exactly 1 inside the same database transaction that commits each of: a `SettingsStore::update_all` commit, and the pricing-profile-patterns point mutation. The increment MUST be one statement computing `value + 1` inside the transaction.

E3. A replica MUST poll the epoch with exactly one single-row, single-column `SELECT` (the epoch value only) every `MONOIZE_CONFIG_POLL_INTERVAL_SECONDS`. When the observed value differs from the last applied value, the replica MUST rebuild `MonoizeRuntimeConfig` from committed `system_settings` values using the same construction logic as primary publication, then swap it into `monoize_runtime` under the existing snapshot lock. The poll MUST fetch no other rows or columns, and the rebuild MUST run only when the epoch value changed. The rebuild itself performs reads only.

E4. A failed epoch poll (database error or unparseable stored value) MUST log at `warn` level, keep the previous snapshot, and retry on the next tick. It MUST NOT terminate the process. Idle replicas keep polling on the fixed interval; no traffic-adaptive backoff is permitted because it would make configuration propagation latency traffic-dependent.

E5. Provider/channel routing rows are not part of the epoch contract: replicas read them fresh from the shared database on demand, subject only to the existing cache TTLs.

## 5. Replica request surface

D1. A replica is an API-only node. It MUST NOT serve frontend static assets or mount dashboard read routes. It MUST mount only the Store mutation routes required by `store-billing.spec.md` SB-S-9 under `/api/dashboard/store`; those routes MUST return the repository write rejection without mutation. Any other request to `/api/dashboard/**` or to a non-API UI path MUST return HTTP 404 with JSON body code `replica_dashboard_disabled`.

D2. Forwarding endpoints (`/v1/**`), the metrics endpoint, and health paths MUST be served locally by the replica against the shared database's read path.

D3. Dashboard administration happens exclusively on the primary node.

## 6. Metering pipeline (replica → primary)

### 6.1 Data classes

M1. Four data classes ship from replica to primary:

1. request logs — the existing durable spool files produced by DPT-RL3*;
2. last-used updates — `{api_key_id, last_used_at}` pairs buffered by `LastUsedBatcher`;
3. plan terminals — durable settlement or release records defined by PRP-B24;
4. balance deltas — billing charge events produced on the replica.

M2. A balance delta record is `{delta_id, kind, user_id, api_key_id?, amount_nano_usd, meta_json, created_at}` where:

- `delta_id` is one UUID v4 generated at enqueue time;
- `kind` is `request_charge` or `api_key_charge` (sub-account);
- `user_id` identifies the owning user; `api_key_id` is present iff `kind = api_key_charge`;
- `amount_nano_usd` is a nonnegative charge magnitude representable as decimal signed-128 text;
- `created_at` is RFC 3339.

M2a. A negative `amount_nano_usd` MUST be rejected before durable enqueue, pending-counter mutation, or database mutation. HTTP ingest MUST return `422 metering_batch_invalid` and apply no entry from the batch. The shared apply routine, including PRP9 promotion drain, MUST enforce the same amount domain before opening its transaction. Replica startup MUST reject a persisted negative delta before reconstructing pending counters and MUST preserve its spool file. A rejected promotion drain MUST preserve every submitted spool file. A negative resulting balance under I5 remains valid; a negative charge magnitude does not.

M3. Before the charge path reports success on a replica, the delta MUST be durably published as one JSON file in `MONOIZE_REPLICA_METERING_SPOOL_DIR` using temporary-file write followed by same-directory atomic rename. If publication fails or the combined spool size would exceed `MONOIZE_REPLICA_METERING_SPOOL_MAX_BYTES`, enqueue MUST fail and terminal billing finalization MUST treat the request as a billing failure consistent with MB-C6. A successful enqueue MUST also atomically add `amount_nano_usd` to the in-memory pending-deduction counter keyed by `user_id` (kind `request_charge`) or `api_key_id` (kind `api_key_charge`).

M3a. Replica startup MUST create `MONOIZE_REPLICA_METERING_SPOOL_DIR` if it is absent and MUST write then delete one probe file in that directory. A create, write, or permission failure MUST stop startup with error `metering_spool_unwritable`. A bind-mounted spool directory MUST be writable by the process user; a root-owned mount that the non-root process cannot write MUST fail this probe rather than accept traffic.

M3b. Subtracting an acknowledged deduction and removing its zero-valued pending entry MUST be one atomic transition for that subject. An enqueue concurrent with acknowledgement MUST retain its added amount. For an initial pending amount of `a`, a concurrent `add(b)` and `subtract(a)` MUST leave pending amount `b` when both transitions finish, where `a` and `b` are nonnegative and `a + b` is representable as `i128`.

### 6.2 Ship loop

M4. The replica MUST run one ship loop that POSTs at most one JSON batch per iteration to `POST {MONOIZE_PRIMARY_INTERNAL_URL}/internal/replica/metering` with header `Authorization: Bearer {MONOIZE_REPLICA_TOKEN}`. The loop MUST iterate at least every `MONOIZE_METERING_SHIP_INTERVAL_SECONDS`. It MUST also iterate as soon as a request-log spool file, plan terminal, or balance delta is durably published (M4b), coalescing wakes that arrive while a POST is in flight into the next iteration. The batch is composed as:

1. the oldest durable request-log spool files, at most `MONOIZE_METERING_SHIP_BATCH_MAX_ENTRIES`, discovered from on-disk `.json` files even when the in-memory buffer is empty (same discovery as `db-performance-tuning.spec.md` DPT-RL4 `load_spool_batch`);
2. pending plan terminals in PRP-B26 order, filling remaining capacity;
3. pending deltas in existing oldest-first order, filling remaining capacity;
4. currently buffered last-used pairs, filling remaining capacity.

Total entries across the four arrays MUST be at most 2000 (I3). Entries that do not fit MUST remain buffered or spooled for the next tick. Every tick, including a tick whose four arrays are empty, MUST include a `replica` heartbeat object (M4a) and MUST POST.

M4b. Publishing a terminal request-log spool file, plan terminal, or balance delta MUST wake the ship loop without waiting for the next interval tick. The ship loop remains the only metering POST issuer.

### Plan Admission Domain Protocol

PRP-A1. The internal Primary issue operation MUST accept the SB-Q-14A input. It MUST return `Balance` when no current plan applies. It MUST return `Plan(IssuedAdmission)` with token ID, reservation ID, compact JWS, issue time, expiry, and duplicate state when a plan applies. Transport mapping is outside this phase.

PRP-A2. The Primary MUST treat `(Replica audience, request ID)` as the issue idempotency identity. An ambiguous client retry after a committed response loss MUST return the exact stored compact JWS and the original reservation.

PRP-A2A. PostgreSQL MUST serialize concurrent issue calls for one `(Replica audience, external request ID)` with the SB-Q-14C-1 transaction advisory lock before it reads admission-token state. SQLite MUST serialize them with `BEGIN IMMEDIATE`. The internal quota request key MUST use SB-Q-14C-2 and MUST NOT make the external request ID globally unique across Replica audiences.

PRP-A3. The internal Primary keyset operation MUST return the SB-Q-14F verifier-only projection. Replica keyset caching, refresh transport, and HTTP authentication are outside this phase.

PRP-A3A. Primary admission and keyset operations MUST parse persisted RFC3339 values to UTC instants before range or maximum comparison. They MUST NOT rely on lexical timestamp ordering in either supported database.

PRP-A4. The internal Primary terminal operation MUST accept token ID, reservation ID, request ID, Replica audience, terminal kind, optional actual nano USD, canonical digest, and applied time. It MUST return `applied` for the first commit and `duplicate` for one exact replay.

PRP-A4A. Terminal apply MUST lock the admission token before it checks an existing receipt. Receipt replay classification MUST precede binding mismatch classification as defined by SB-Q-15B. This lock order MUST serialize exact and conflicting concurrent replays on both supported databases.

PRP-A5. The Primary MUST reject an unknown token with `admission_token_not_found`, a token binding mismatch with `admission_binding_mismatch`, an invalid digest with `admission_terminal_digest_invalid`, and a changed replay with `admission_terminal_conflict`. Transport status mapping is outside this phase.

PRP-A6. Phase A MUST NOT add HTTP routes, AppState fields, Replica metering integration, request handlers, or spool shipping. Those integrations require later phases.

### Plan Admission Transport And Replica Spool

PRP-B1. A Primary with a configured `MONOIZE_REPLICA_TOKEN` MUST mount `POST /internal/replica/admission/issue`, `POST /internal/replica/admission/confirm`, `GET /internal/replica/admission/keyset`, and `POST /internal/replica/metering`. A Primary without that token MUST mount none of these routes. A Replica MUST mount none of these routes.

PRP-B2. Every request to a PRP-B1 route MUST carry `Authorization: Bearer <MONOIZE_REPLICA_TOKEN>` and `X-Monoize-Replica-ID: <replica_id>`. The Replica ID MUST equal the canonical lowercase hyphenated UUID resolved by M9. Authentication MUST execute before body parsing. A missing or invalid bearer token or Replica ID MUST return HTTP `401` with code `replica_auth_failed`.

PRP-B3. Bearer verification MUST compare SHA-256 digests in constant time. Dashboard sessions, API keys, and payment callback credentials MUST NOT authorize a PRP-B1 route.

PRP-B4. `MONOIZE_REPLICA_TOKEN` authenticates membership in one mutually trusted cluster. It does not distinguish two nodes that possess the same token. `X-Monoize-Replica-ID` prevents accidental audience mismatch but does not resist impersonation by another cluster member. A deployment that treats another Replica as hostile MUST use distinct per-Replica credentials in a later protocol version.

PRP-B5. `POST /internal/replica/admission/issue` MUST require `Content-Type: application/json`. Its decoded body MUST be `{"audience": string, "user_id": string, "request_id": string, "effective_groups": string[], "maximum_nano_usd": string, "pricing_revision": string}`. `maximum_nano_usd` MUST be a positive canonical decimal integer string. Unknown fields MUST be rejected.

PRP-B6. The issue body MUST NOT contain `issued_at`. The Primary MUST set `issued_at` from its UTC clock after authentication and before the admission transaction. `audience` MUST equal `X-Monoize-Replica-ID`; mismatch MUST return HTTP `403` with code `replica_audience_mismatch` and MUST create no reservation or token.

PRP-B7. An issue response for `AdmissionDecision::Balance` MUST be HTTP `200` with body `{"funding":"balance"}`. It MUST contain no token or reservation field.

PRP-B8. An issue response for `AdmissionDecision::Plan` MUST be HTTP `200` with body `{"funding":"plan","token_id":string,"reservation_id":string,"compact_jws":string,"issued_at":RFC3339,"expires_at":RFC3339,"duplicate":boolean}`. These fields MUST equal the persisted `IssuedAdmission`.

PRP-B9. An issue or confirmation request body larger than 65536 bytes MUST return HTTP `413` with code `admission_request_too_large`. An unsupported content type MUST return HTTP `415` with code `admission_content_type_invalid`. Invalid JSON or `admission_input_invalid` MUST return HTTP `422`. `admission_issue_conflict` MUST return HTTP `409`.

PRP-B10. `plan_quota_exhausted` and `plan_request_unbounded` MUST return HTTP `402`. `plan_payment_hold` and `plan_quota_violation_blocked` MUST return HTTP `423`. `admission_active_key_missing`, `admission_wrap_key_missing`, `admission_key_invalid`, `quota_gate_unavailable`, `quota_storage_error`, and `admission_storage_error` MUST return HTTP `503`. The handler quota-error mapper MUST preserve each listed code. Every error body MUST equal `{"error":{"code":string,"message":string}}`.

PRP-B10A. A Primary admission error response MUST derive `message` only from the returned error code. It MUST NOT include a database error, SQL text, URL, filesystem path, credential, or wrapped internal error text. The Primary MUST record the original error text only in an internal log field. A Replica admission error exposed through a public API MUST use the same code-only public-message rule. The resulting `AppError` MUST store the original transport, decode, verification, claim, terminal, or remote detail in `internal_message` and MUST NOT store that detail in its public `message`.

PRP-B10B. The code-only public message map MUST return `plan quota exhausted` for `plan_quota_exhausted`, `plan request has no finite billing bound` for `plan_request_unbounded`, `plan admission is blocked by a payment hold` for `plan_payment_hold`, `plan admission is blocked by a quota violation` for `plan_quota_violation_blocked`, `admission token is not found` for `admission_token_not_found`, `admission request conflicts with stored state` for every admission conflict code, and `plan admission is unavailable` for every other admission code. Unknown remote codes MUST use the caller's operation-specific fallback code and `plan admission is unavailable`.

PRP-B11. `GET /internal/replica/admission/keyset` MUST accept no request body. A successful response MUST be HTTP `200` with body `{"keys":[{"key_id":string,"public_key_base64":string,"state":"active"|"retired","activated_at":RFC3339,"verify_until":RFC3339|null}]}`.

PRP-B12. The keyset response MUST satisfy SB-Q-14F. It MAY contain zero active keys. It MUST set `Cache-Control: no-store`. A keyset storage or validation failure MUST return HTTP `503` with the corresponding admission error code.

PRP-B13. Replica token verification MUST use a verifier-only key ring. This ring MUST store `ed25519_dalek::VerifyingKey` values only. It MUST expose no token issue, key publication, key activation, or signing operation. It MUST never contain an encrypted seed or `SigningKey`.

PRP-B14. A keyset snapshot MUST be rejected as a whole when a key ID is duplicated, a public key is not canonical base64url without padding, decoded key length is not 32 bytes, a state is invalid, more than one key is active, an active key has `verify_until`, or a retired key lacks a future `verify_until`.

PRP-B15. A successful keyset fetch MUST atomically replace the prior verifier snapshot. It MUST NOT merge snapshots. Expired retired keys MUST be excluded from each verification. A failed refresh MUST retain the prior snapshot unchanged.

PRP-B16. A Replica MUST refresh the keyset before its first plan issue and whenever snapshot age reaches `MONOIZE_CONFIG_POLL_INTERVAL_SECONDS`. Concurrent refresh requests MUST share one in-flight HTTP request.

PRP-B17. When a Plan response uses an unknown `kid`, the Replica MUST perform one immediate keyset refresh and retry verification exactly once. If the key remains unknown or refresh fails, the Replica MUST return HTTP `503` with code `plan_admission_verification_unavailable`. It MUST NOT create a claim marker or route upstream.

PRP-B18. Verification MUST require issuer `lynshen-primary`. It MUST bind audience, token ID, reservation ID, external request ID, maximum nano USD, pricing revision, issue time, and expiry to the issue request and response. Entitlement ID, generation, and reserved CNY fen MUST come from the verified claims. Verification failure MUST occur before claim creation and upstream routing.

PRP-B18A. The only valid admission issuer MUST equal `lynshen-primary`. Primary construction MUST reject another issuer with `admission_issuer_invalid`. Token issue and Replica verification MUST use the same constant.

PRP-B19. `AdmissionService` MUST accept an optional `PaymentKeyRing`. It MUST determine whether an applicable current plan exists before reading that option. Balance issue, `public_keyset`, confirmation, unconfirmed-token recovery, and terminal apply MUST work without a wrap key. Plan issue without a wrap key MUST return `admission_wrap_key_missing` and MUST write no reservation, token, bucket change, or key update.

PRP-B19A. A newly issued Plan token MUST be provisional. `store_admission_tokens.confirmed_at` MUST be null in the issue transaction. A Replica MUST verify the token and durably publish its claim marker before it sends confirmation. It MUST NOT route upstream before confirmation succeeds.

PRP-B19B. `POST /internal/replica/admission/confirm` MUST require the PRP-B2 headers and JSON content type. It MUST accept only `{"audience":string,"token_id":string,"reservation_id":string,"request_id":string}`. Unknown fields or invalid identifiers MUST return HTTP `422` with `admission_input_invalid`. A missing token MUST return HTTP `404` with `admission_token_not_found`. Body audience mismatch with `X-Monoize-Replica-ID` MUST return HTTP `403` with `replica_audience_mismatch`. The Primary MUST set confirmation time from its UTC clock. In one write transaction it MUST lock the token, match every supplied binding, require no terminal receipt, require the bound reservation state `reserved`, require confirmation time earlier than `expires_at + 5 seconds`, and set `confirmed_at` only when it is null. First success MUST return HTTP `200` with `{"confirmed":true,"duplicate":false}`. An exact replay of a prior confirmation MUST return the same status with `duplicate:true`. A binding conflict MUST return HTTP `409` with `admission_binding_mismatch`. An expired, released, or terminal token MUST return HTTP `409` with `admission_confirmation_expired`. Storage failure MUST return HTTP `503` with `admission_storage_error`.

PRP-B19C. On a confirmation transport error or ambiguous response loss, the Replica MUST retry the same confirmation exactly once before it changes local claim state. A non-200 response, malformed response, or failed second attempt MUST prevent upstream routing. The Replica MUST durably transition the claim to `release_pending` before it publishes a release terminal. Once that transition begins, no later confirmation result, including `duplicate:true`, MAY authorize routing. If release publication fails, `release_pending` MUST remain. PRP-B19E MUST later publish the release.

PRP-B19D. The Primary MUST scan at most 100 unconfirmed tokens without a terminal receipt whose `expires_at + 5 seconds <= now`, ordered by `(expires_at ASC, token_id UTF-8 byte order ASC)`, at least once every five seconds. The candidate query MUST compare `expires_at_unix` with `now.timestamp() - 5`, exclude rows matched by `store_admission_terminal_receipts`, order by `(expires_at_unix ASC, token_id ASC)`, and apply `LIMIT 100` in the database. It MUST NOT compare RFC3339 text in SQL. Application code MUST parse every returned `expires_at` value and require its Unix-second value to equal `expires_at_unix`; malformed or inconsistent persisted values MUST return `admission_storage_error` before recovery mutation. For each row, one write transaction MUST lock the token, recheck `confirmed_at IS NULL`, and apply the canonical release through SB-Q-15B. The transaction MUST insert the normal terminal receipt. An exact concurrent Replica release MUST resolve through normal duplicate semantics. This recovery MUST require no signing or wrap key.

PRP-B19E. Claim lifecycle MUST be `claimed`, `confirmed`, `routed`, or `release_pending`. Claim publication creates `claimed`. A successful Primary confirmation MUST be followed by a durable `claimed -> confirmed` replacement before routing. Failure of that replacement MUST enter `release_pending`. Immediately before the first physical upstream dispatch, the Replica MUST durably replace `confirmed` with `routed` and set `routed_at`; a marker write or directory-sync failure MUST prevent dispatch and enter `release_pending`. `release_pending` is irreversible. A normal in-process ship tick MUST publish a release for `release_pending` without a terminal. It MUST NOT synthesize a terminal for `claimed`, `confirmed`, or `routed` merely because token expiry passed.

PRP-B19F. On Replica startup, every unacknowledged claim without a terminal belongs to an abandoned prior process. Startup MUST publish release for `claimed`, `confirmed`, or `release_pending`. Startup MUST publish settlement with `actual_nano_usd = maximum_nano_usd` for `routed`. Marking `routed` before dispatch creates a conservative crash boundary: a crash before network acceptance MAY consume the reserved maximum, but a crash after network acceptance MUST NOT release potentially consumed quota. A claim with a terminal MUST retain and ship that terminal unchanged.

PRP-B19G. A request that fails before physical dispatch after reaching `routed` MUST durably publish release. A routed request with measured usage MUST publish settlement with actual usage. A routed request whose handler explicitly selects zero charge MAY publish release under SB-Q-4E. The normal request owner MUST publish exactly one terminal before it reports terminal billing success.

PRP-B19H. After a Replica verifies a Plan response, one detached admission owner MUST own claim publication, confirmation, confirmed-marker publication, and active-map insertion. Cancellation or drop of the caller waiting for `issue` MUST NOT cancel that owner. If the caller cannot accept the completed Plan handoff, the owner MUST durably enter `release_pending`, publish one release terminal, remove the request from the active map, and wake the ship loop. An error after claim publication MUST produce the same release state. These transitions MUST complete in the running process and MUST NOT depend on startup recovery.

PRP-B19I. Every Replica Plan handler funding scope MUST share one completion flag across all clones. Successful explicit `finish` MUST set that flag only after terminal publication or after it observes that the active admission already has a terminal. When the last unfinished scope is dropped, including cancellation while `finish` is pending, it MUST start one detached cleanup that durably enters `release_pending`, publishes one release terminal, removes the active-map entry, and wakes the ship loop. Repeated cancellation cleanup MUST produce at most one terminal file for the token.

PRP-B20. Primary `AppState` MUST contain an `AdmissionService` and MUST NOT contain a Replica claim store or verifier cache. Replica `AppState` MUST contain no `AdmissionService`; its existing `ReplicaMetering` MUST own the verifier cache and claim store.

PRP-B21. Replica admission files MUST live under `<MONOIZE_REPLICA_METERING_SPOOL_DIR>/plan-admission/claims` and `<MONOIZE_REPLICA_METERING_SPOOL_DIR>/plan-admission/terminal`. Delta-spool cleanup MUST preserve both directories and every valid file in them.

PRP-B22. A claim filename MUST be `claim-<digest>.json`. A terminal filename MUST be `terminal-<digest>.json`. `<digest>` MUST equal the 64-character lowercase hexadecimal SHA-256 digest of the UTF-8 token ID.

PRP-B23. A claim file MUST contain `{"version":1,"token_id":string,"reservation_id":string,"request_id":string,"audience":string,"maximum_nano_usd":string,"expires_at":RFC3339,"state":"claimed"|"confirmed"|"routed"|"release_pending","routed_at":RFC3339|null,"acknowledged_at":RFC3339|null,"terminal_reserved_bytes":4096}`. `maximum_nano_usd` MUST be a positive canonical decimal integer string. `routed_at` MUST be nonnull exactly when state is `routed`. An unknown version, invalid state shape, or filename digest that does not match `token_id` MUST stop startup with `admission_spool_corrupt`. `terminal_reserved_bytes` MUST always equal `4096`; it is a persisted capacity constant, not mutable state.

PRP-B24. A terminal file MUST contain `{"version":1,"token_id":string,"reservation_id":string,"request_id":string,"audience":string,"kind":"settlement"|"release","actual_nano_usd":string|null,"canonical_digest":string,"created_at":RFC3339}`. Settlement requires a nonnegative canonical decimal amount. Release requires null. `canonical_digest` MUST satisfy SB-Q-15A.

PRP-B25. Claim and terminal publication MUST create a unique same-directory temporary file, write the complete JSON bytes, and fsync that file. A write or file-sync failure MUST best-effort remove the temporary file and return the original failure. Publication MUST create the final path without replacing an existing path. A same-filesystem hard link from temporary path to final path satisfies this rule. A hard-link failure MUST best-effort remove the temporary file and return the original hard-link failure. After hard-link success, temporary-file deletion is best-effort: deletion failure MUST NOT change the publication to failure. The caller MUST fsync the directory and treat the final path as successfully published after every hard-link success. A crash-residual temporary file MUST NOT count as a claim or terminal record. An existing claim final path MUST return `admission_token_replay`, including when its decoded content is identical. An existing terminal final path with identical decoded content is idempotent only after the terminal directory fsync succeeds. Different terminal content at that path MUST return `admission_terminal_conflict`.

PRP-B26. Pending terminal records MUST be selected in `(created_at ASC, token_id UTF-8 byte order ASC)`. One selection MUST contain at most `MONOIZE_METERING_SHIP_BATCH_MAX_ENTRIES` records. A record omitted by the limit MUST remain unchanged.

PRP-B27. One shared metering-capacity counter MUST include top-level balance-delta files, admission claim files, admission terminal files, and the unused terminal reservation defined by PRP-B27A. It MUST exclude `replica-identity`, admission temporary files, and the separately limited request-log spool.

PRP-B27A. Capacity reconstruction and mutation MUST use exactly these states. A claim file always contributes its encoded file length. An unacknowledged claim without a terminal file contributes an additional 4096 reserved bytes. An unacknowledged claim with a terminal contributes the terminal file length and no reserved bytes. An acknowledged claim with a crash-residual terminal contributes the terminal file length and no reserved bytes. An acknowledged claim without a terminal contributes no reserved bytes. A terminal without its claim is corrupt and MUST stop startup with `admission_spool_corrupt`.

PRP-B28. Claim publication MUST atomically reserve 4096 bytes for its future terminal record in addition to the encoded claim-file bytes. Terminal publication MUST reject an encoded terminal record larger than 4096 bytes. Successful terminal publication MUST replace the 4096-byte reservation with the terminal file's actual byte count. An idempotent existing terminal MUST make no capacity change.

PRP-B29. Delta enqueue, claim publication, terminal publication, terminal acknowledgement, and file cleanup MUST mutate shared capacity under one process-wide lock. They MUST reject a new publication with `metering_spool_quota_exhausted` when accounted bytes would exceed `MONOIZE_REPLICA_METERING_SPOOL_MAX_BYTES`.

PRP-B29A. Every durable claim-state replacement MUST execute under the shared capacity lock and apply `(new claim file length - old claim file length)` after atomic replacement. Confirmation, route marking, release-pending recovery, acknowledgement, and startup recovery are recovery mutations and MUST remain allowed while accounted usage exceeds the configured maximum. They MUST NOT reserve another 4096 bytes.

PRP-B30. Startup MUST remove residual admission temporary files before it reconstructs accounted bytes or serves traffic. It MUST remove only regular files whose name exactly matches `.<kind>-<digest>.json.<uuid>.tmp`, where `<kind>` is `claim` in the claims directory or `terminal` in the terminal directory, `<digest>` is 64 lowercase hexadecimal characters, and `<uuid>` is a canonical lowercase hyphenated UUID version 4. It MUST preserve final files, directories, symlinks, and every unknown or partial name. It MUST fsync a directory after removing at least one residual file. Startup MUST validate final files and reconstruct accounted bytes with PRP-B27A before serving traffic. A malformed admission final file or invalid claim/terminal pairing MUST stop startup with `admission_spool_corrupt`. Existing valid data above the configured limit MUST remain shippable, but every new metering or admission publication MUST fail until accounted bytes are within the limit.

PRP-B31. `MeteringBatch` MUST add `plan_terminals`, defaulting to an empty array. Each entry MUST use the terminal schema in PRP-B24. The total-entry limit in I3 MUST count request logs, last-used pairs, balance deltas, and plan terminals; the heartbeat remains excluded.

PRP-B32. One ship tick MUST select request logs first, plan terminals second, balance deltas third, and last-used pairs fourth. Each durable class MUST preserve its own oldest-first order. Entries that do not fit the 2000-entry hard cap MUST remain pending. Publishing a plan terminal MUST wake the existing single ship loop.

PRP-B33. Every metering request MUST carry the PRP-B2 `X-Monoize-Replica-ID` header, including heartbeat-only requests and batches without plan terminals. Every terminal audience and the optional heartbeat `replica.id` MUST equal that header. A mismatch MUST return HTTP `403` with code `replica_audience_mismatch` before mutation.

PRP-B34. Before opening the metering transaction, the Primary MUST validate only terminal JSON shape, canonical scalar encodings, terminal-kind amount shape, and the SB-Q-15A digest. Token existence, receipt state, binding, replay classification, reservation mutation, and receipt insertion MUST be checked while holding the SB-Q-15B locks inside the single I4 transaction. The transaction MUST commit before any acknowledgement is returned.

PRP-B34A. Primary settlement MUST require `store_admission_tokens.confirmed_at IS NOT NULL`. Settlement for an unconfirmed token MUST return `admission_terminal_conflict`. Release MAY apply to a provisional or confirmed token. The PRP-B19D reaper MUST do nothing when the locked token is confirmed or already has any terminal receipt. A provisional token with an existing release receipt is recovered and MUST NOT be retried.

PRP-B35. An unknown token MUST return HTTP `404` with code `admission_token_not_found`. A binding mismatch or changed replay MUST return HTTP `409` with code `admission_binding_mismatch` or `admission_terminal_conflict`. An invalid digest MUST return HTTP `422` with code `admission_terminal_digest_invalid`. Such a response MUST acknowledge no entry, MUST roll back the complete metering batch, and the Replica MUST retain the submitted batch.

PRP-B36. `MeteringAck` MUST add `plan_terminal_acks`. Each acknowledgement MUST equal `{"token_id":string,"canonical_digest":string,"result":"applied"|"duplicate"}`. A successful transaction MUST return one acknowledgement per submitted plan terminal in submission order.

PRP-B37. A Replica MUST treat a plan-terminal acknowledgement as valid only when exactly one acknowledgement has the same token ID and canonical digest as the submitted terminal and its result is `applied` or `duplicate`. An unexpected, duplicate, missing, or mismatched acknowledgement MUST NOT release that terminal.

PRP-B38. A malformed HTTP `200` acknowledgement body MUST release no request log, last-used pair, balance delta, or plan terminal. A structurally valid HTTP `200` MUST release the existing three data classes under M5. Each plan terminal MUST independently satisfy PRP-B37 before that terminal is released.

PRP-B38A. Each Primary response consumed by a Replica for keyset, issue, confirmation, or metering acknowledgement MUST contain at most 65536 encoded body bytes. The Replica MUST inspect `Content-Length` when present and MUST otherwise read body chunks only until byte 65537 is observed. It MUST NOT call an unbounded whole-body `bytes`, `text`, or `json` reader for these responses. A body of exactly 65536 bytes is within the limit. An oversized keyset response MUST retain the previous verifier snapshot. An oversized issue response MUST create no local claim. An oversized confirmation response MUST follow PRP-B19C. An oversized or malformed metering acknowledgement MUST retain every submitted durable record and buffered entry.

PRP-B39. For an exact terminal acknowledgement, the Replica MUST hold the shared capacity lock. It MUST serialize the complete updated claim marker to a unique same-directory temporary file and fsync that file. It MUST atomically replace the final claim path with that temporary file on Unix and Windows. It MUST NOT truncate or rewrite the final claim path in place. Immediately after replacement it MUST add `(new claim file length - old claim file length)` to accounted capacity, then fsync the claims directory. This recovery mutation MUST be allowed while usage exceeds the configured maximum. It MUST then delete the matching terminal file. Immediately after successful deletion it MUST subtract the terminal file length, then fsync the terminal directory. A crash before deletion is reconstructed as the acknowledged-claim-with-terminal state in PRP-B27A.

PRP-B40. A claim marker MUST remain while `acknowledged_at` is null, regardless of token age. After acknowledgement, it MUST remain until `now >= expires_at + 300 seconds`. Only when both conditions hold MAY cleanup delete and directory-sync the claim file.

PRP-B41. A crash after claim acknowledgement but before terminal deletion MUST cause the terminal to ship again. The Primary MUST return `duplicate`; the Replica MUST repeat PRP-B39. A crash after terminal deletion but before claim-retention expiry MUST leave the acknowledged claim marker available for replay rejection.

PRP-B42. A non-200 response, transport failure, invalid acknowledgement, terminal-file deletion failure, claim-file replacement failure, or acknowledgement directory-sync failure MUST increment the existing consecutive shipment-failure counter. A claims-directory sync failure after marker replacement MUST leave the acknowledged marker and terminal available for retry, with capacity equal to both visible files. A terminal-directory sync failure after terminal deletion MUST leave the acknowledged marker, keep the terminal absent, and return failure; an exact retry MUST sync that directory and succeed without subtracting capacity again. At every returned failure, capacity MUST equal the files visible to the running process.

PRP-B43. Tests MUST cover Balance and Plan issue responses, provisional confirmation, exact issue and confirmation retry, confirmation response loss followed by irreversible release, unconfirmed Primary recovery, all claim lifecycle transitions, long-running routed requests beyond token expiry, crash recovery before route, crash recovery after route, settlement rejection before confirmation, every confirm status mapping, bearer rejection, audience mismatch, fixed issuer rejection, keyset secret exclusion, verifier-only construction, scheduled refresh, one unknown-`kid` refresh, corrupt spool startup failure, every PRP-B27A capacity state, shared-capacity concurrency, deterministic terminal order, exact acknowledgement deletion, missing and mismatched acknowledgement retention, duplicate Primary apply, both PRP-B41 crash points, and marker removal only after acknowledgement and `expires_at + 300 seconds`.

M4a. The `replica` heartbeat object is not counted toward the 2000-entry cap. Its schema is:

```json
{
  "id": "uuid v4 string, resolved per M9; stable across process restarts of one replica deployment",
  "hostname": "string",
  "listen": "the replica MONOIZE_LISTEN value",
  "version": "CARGO_PKG_VERSION",
  "started_at": "RFC 3339 process start",
  "uptime_seconds": 0,
  "spool_pending_count": 0,
  "spool_pending_bytes": 0
}
```

A primary that authenticates an ingest request containing `replica` MUST upsert that replica into a process-local heartbeat map keyed by `id`, recording `last_seen_at = now`, before applying the batch. Heartbeat recording MUST NOT require a non-empty data array. The map is not persisted across primary restarts. Each time the map is read for the admin overview response, every entry with `now - last_seen_at > 360 * MONOIZE_METERING_SHIP_INTERVAL_SECONDS` MUST first be removed from the map; removed entries MUST NOT appear in that response.

M5. Spool files, buffered last-used pairs, pending deltas, and their pending-deduction counters MUST only be released after an HTTP 200 response. Any non-200 response or transport error MUST retain everything unchanged for the next tick. After 3 consecutive failed ticks the replica MUST log a `warn` naming the consecutive-failure count, repeated per subsequent failure. A heartbeat-only tick that receives HTTP 200 is a successful tick.

M6. At graceful shutdown the replica MUST make one best-effort final ship attempt; leftover data persists on disk and ships after restart.

### 6.3 Replica-side balance preflight

M7. Balance preflight on a replica MUST compute `effective_balance = persisted_balance - pending_deductions[subject]` where `persisted_balance` comes from the existing cache/read path and `subject` follows M2 keying. The insufficient-balance decision and HTTP mapping (402 `insufficient_balance`) MUST match `ensure_user_can_spend` / `ensure_sub_account_can_spend`. Unlimited balances bypass subtraction.

M8. Because preflight subtracts locally unshipped charges, overspend during a primary outage is bounded by in-flight concurrency, not by shipment delay.

### 6.4 Replica identity

M9. A replica MUST resolve exactly one identity UUID at startup, before the first ship-loop tick, in this order:

1. When `MONOIZE_REPLICA_ID` is set to a non-empty value, the value MUST parse as a UUID whose RFC 4122 version field equals 4 (hyphenated or simple hex form; case-insensitive). On parse success, the canonical lowercase hyphenated form is the identity; the identity file of step 2 is neither read nor written. On parse failure or version mismatch, startup MUST stop with error `replica_id_invalid`.
2. Otherwise the replica MUST read the identity file `{MONOIZE_REPLICA_METERING_SPOOL_DIR}/replica-identity`. When the file exists and its whitespace-trimmed content parses as a UUID with version 4, the canonical lowercase hyphenated form is the identity and the file is left unchanged.
3. Otherwise (file absent, unreadable, or content not a version-4 UUID) the replica MUST generate one new UUID v4, persist it to the identity file by writing a temporary file in the same directory, fsyncing it, then atomically renaming it onto `replica-identity`, and use the generated value as the identity. A directory-create, write, sync, or rename failure MUST stop startup with error `replica_identity_unwritable`.

M9a. The identity file content written by M9 step 3 is exactly the 36-character lowercase hyphenated UUID followed by one `\n` (37 bytes). Readers tolerate surrounding ASCII whitespace per M9 step 2.

M9b. The spool-directory startup cleanup (the M3a construction path that deletes non-`.json` leftovers) MUST NOT delete a file named `replica-identity`.

M9c. The M4a heartbeat `id` MUST equal the resolved identity. Consequently the `id` is stable across process restarts for one replica data directory (or for one `MONOIZE_REPLICA_ID` value), and a restarted replica upserts its existing heartbeat map entry on the primary instead of creating an additional entry.

M9d. Replica application startup MUST call the M9 resolver exactly once. It MUST pass the resulting canonical identity explicitly to `ReplicaMetering`; `ReplicaMetering` MUST pass the same value to `AdmissionClient` and the heartbeat source without another file read, file write, UUID generation, or override. When `MONOIZE_REPLICA_ID` is configured, startup MUST NOT create `replica-identity`.

## 7. Metering ingest API (primary)

I1. Route: `POST /internal/replica/metering`, mounted iff PRP6 conditions hold. It is outside dashboard-session auth.

I2. Authentication: bearer token compared by SHA-256 digest equality (constant-time comparison). Mismatch MUST return HTTP 401 code `replica_auth_failed`.

I3. Body schema:

```json
{
  "replica": { "id": "...", "hostname": "...", "listen": "...", "version": "...", "started_at": "...", "uptime_seconds": 0, "spool_pending_count": 0, "spool_pending_bytes": 0 },
  "request_logs": ["SpoolRequestLog objects per DPT-RL3"],
  "last_used": [{"api_key_id": "...", "last_used_at": "RFC 3339"}],
  "plan_terminals": ["one PRP-B24 object"],
  "balance_deltas": [one object per M2]
}
```

All four arrays MAY be empty. `plan_terminals` MUST default to an empty array when omitted by an older Replica. `replica` MAY be omitted by older replicas; when present it MUST be recorded per M4a and MUST NOT count toward the entry cap. If total entries across the four arrays exceed 2000, the endpoint MUST return HTTP 413 code `metering_batch_too_large` without partial apply. Any per-entry schema violation MUST return HTTP 422 code `metering_batch_invalid` without partial apply.

I4. The entire batch MUST apply inside one database transaction:

1. request logs via the existing chunked multi-row insert with `ON CONFLICT(id) DO NOTHING` (chunk rules of DPT-RL4);
2. plan terminals in submitted order through the SB-Q-15B token lock, receipt, binding, and quota mutation path;
3. last-used via the existing bulk `UPDATE ... CASE` statement;
4. each balance delta via one `INSERT INTO billing_ledger (..., idempotency_key, ...) VALUES (...) ON CONFLICT(idempotency_key) DO NOTHING` with `idempotency_key = delta_id`, then, iff that statement inserted one row, the balance update of I5.

Commit MUST precede the HTTP 200 response. The response body MUST be `{"applied_request_logs":N,"applied_last_used":N,"applied_balance_deltas":N,"plan_terminal_acks":[PRP-B36 objects]}`. Counts MUST include actually inserted rows and accepted pairs. `plan_terminal_acks` MUST contain one exact acknowledgement per submitted plan terminal in submission order. Any transaction error MUST roll back every statement of the batch and return HTTP 500 code `metering_apply_failed`; the replica retains and retries the batch unchanged.

I4a. After a successful ingest commit whose batch contained one or more request logs, the primary MUST broadcast those request-log entries on the process-local request-log SSE stream used by `request-logs.spec.md` RL1c-0. Dashboard clients MUST observe replica-originated terminal rows through that stream without waiting for a later list fetch. Name snapshots on that broadcast MAY be empty; the next `GET` list query still JOINs names per `request-logs.spec.md` section 1.2.

I5. Balance update per newly-inserted delta:

- kind `request_charge`: decrement `users.balance_nano_usd` by `amount_nano_usd`, allowing a negative result; an unlimited owner MUST receive no balance update while the delta still counts as applied;
- kind `api_key_charge`: decrement `api_keys.sub_account_balance_nano` for a sub-account-enabled key, allowing negative; if the key is not sub-account-enabled, the update falls back to the owning user row exactly like `charge_sub_account_balance_nano`.

Delta application MUST NOT fail due to insufficient funds; synchronous overdraft rejection belongs exclusively to the replica preflight (M7).

I6. Idempotency window is permanent: `billing_ledger.idempotency_key` values persist under DBO3.1 retention. Replaying an already-applied batch MUST change nothing and return the same success shape.

## 8. Schema change

SC1. Migration `m20260823_000033_billing_ledger_delta_dedupe` MUST add nullable TEXT column `idempotency_key` to `billing_ledger` plus one partial unique index over it restricted to rows where it is not null, identically on SQLite and PostgreSQL. Pre-existing rows keep NULL. Writers other than ingest apply leave the column NULL. The down migration MUST drop the index then the column.

SC2. `state_records` gains no schema change; the config epoch row (E1) is created lazily by the first settings mutation.

SC3. Migration `m20260823_000034_channel_egress_proxy` MUST add nullable TEXT column `proxy_url` to `monoize_channels`, defaulting to NULL (follow-global) for all existing rows, identically on SQLite and PostgreSQL. The down migration MUST drop the column.

## 9. Manual failover

F1. Promotion = stop the replica process, set `MONOIZE_NODE_ROLE=primary`, start. PRP9 drains leftover deltas before the listener accepts requests; the node then operates as the sole writer.

F2. Demotion = stop the primary process, set `MONOIZE_NODE_ROLE=replica` plus the PRP4/PRP5 variables, start; PRP10 gates startup on schema currency.

F3. While the primary is unavailable, replicas MUST continue serving `/v1/**` traffic; charges accumulate durably (M3) and preflight follows M7–M8.

## 10. Observability

O1a. Monoize MUST serve `GET /healthz` and `GET /readyz` on both the primary and the replica role. They MUST be registered on the forwarding router, which is merged before the SPA fallback, so that an unknown path continues to reach the SPA and `/healthz` does not.

O1b. `GET /healthz` MUST answer HTTP `200` with the exact body `ok` followed by a newline. It MUST NOT read the database and MUST NOT depend on the SPA bundle, so it reports the process rather than the database or the frontend. The container healthcheck MUST use it: until this release the healthcheck probed `/`, which the SPA fallback answers with HTTP `200` HTML, so the check passed for a process whose frontend was not yet embedded.

O1c. `GET /readyz` MUST answer HTTP `200` with a JSON object containing `status`, `database_backend`, `database_reachable`, and `role`, and MUST answer HTTP `503` with the same shape when the database read fails or exceeds its bound. It MUST perform at most one trivial read with a bounded wait. It MUST NOT be the container healthcheck, because it reports a dependency rather than this process, and the deploy workflow asserts the container is healthy as a post-condition.

O2. Every replica MUST export Prometheus counter `monoize_replica_metering_shipped_total{result="ok"|"error"}` and gauge `monoize_replica_metering_pending_entries`. The primary MUST export counter `monoize_primary_metering_applied_total`.

## 11. Cross-specification revisions

XR1. `unified_responses_proxy.spec.md` C6 writer exclusivity applies to the primary role; replicas are non-writing processes whose telemetry reaches business tables only through section 7.

XR2. `database-configuration.spec.md` DB16 runs on the primary role only; replicas follow PRP10 read-only verification. DB23b publication happens on the primary; replicas obtain equivalent snapshots via E3.

XR3. `db-performance-tuning.spec.md` DPT-LU3/DPT-LU6 flush-to-database behavior and DPT-RL4 flush-to-database behavior apply to the primary role; on replicas they are replaced by M4–M5 with buffering semantics preserved (PRP12).

XR4. `user-billing-and-model-metadata.spec.md` LC5 single-attempt semantics apply to the primary synchronous charge path; the replica charge path is enqueue-or-fail (M3) without retry loops inside the request lifecycle.

## 12. Test matrix

T1. Config validation: each error code in PRP1/PRP3–PRP7/PX2 has one unit test asserting the exact code.

T1a. Delta spool construction: a writable directory accepts `DeltaSpool::new`; a directory the process cannot write MUST return an error whose text begins with `metering_spool_unwritable`.

T2. Ingest idempotency (SQLite in-memory): replaying one batch twice yields exactly one ledger row per delta, one net balance effect, identical response counts; duplicate `request_logs` ids are no-ops; last-used keeps the later timestamp.

T3. Ingest semantics: unlimited owner skips balance update but counts applied; sub-account delta updates `sub_account_balance_nano`; negative result allowed; batch >2000 returns 413 without partial state.

T4. Shipper against a mock primary: HTTP 200 deletes shipped spool files and clears buffers/counters; HTTP 500 retains everything; transport error retains everything; consecutive-failure warn appears at the third failure.

T4a. Request-log shipment discovers on-disk `.json` spool files even when the in-memory buffer is empty and deletes them only after the sink reports success.

T5. Epoch: primary mutation increments epoch within its transaction; replica poll observes change and swaps snapshot; failed poll keeps prior snapshot.

T6. Replica surface: `/api/dashboard/**` and `/` return 404 `replica_dashboard_disabled` on a replica except the SB-S-9 Store mutation routes; `/v1/**` and `/metrics` are served locally. Each Store mutation route returns the repository write rejection and changes no business table. `/metrics` MUST enforce `security-access-control.spec.md` SAC-1 through SAC-5 instead of returning `replica_dashboard_disabled`.

T7. Promotion drain: a data directory with leftover delta spool entries started as primary applies them before serving and then serves with empty spool.

T8. PostgreSQL parity: SC1 migration and T2/T3 scenarios run against `MONOIZE_TEST_POSTGRES_DSN` when provided and skip otherwise (DB-T1 rules).

T9. Replica identity (M9): first resolution in an empty spool directory creates `replica-identity` containing one version-4 UUID plus `\n`; a second resolution over the same directory returns the identical identity; `DeltaSpool` construction over the same directory preserves the file and a subsequent resolution still returns the identical identity; a corrupt identity file is replaced by a newly generated identity; a valid `MONOIZE_REPLICA_ID` yields its canonical lowercase hyphenated form without creating the file; a non-UUID or non-version-4 `MONOIZE_REPLICA_ID` yields an error whose text begins with `replica_id_invalid`.

T10. Heartbeat eviction (M4a): with ship interval `s`, a map entry with `now - last_seen_at > 360 * s` is removed by the overview read path while an entry with `now - last_seen_at <= 360 * s` is retained.

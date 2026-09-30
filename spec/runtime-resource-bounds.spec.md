# Runtime Resource Bounds Specification

## 0. Status

- **Purpose:** Bound process-local maps and buffers that accept attacker-influenced identities or payloads.
- **Scope:** Applies to `src/bounded_response.rs` and the resource-bound APIs referenced by the database, capture, WebSocket, provider-discovery, and image-transform specifications.

## 1. Configuration parsing

RRB-C1. Each resource-bound environment value in this specification and the linked subsystem specifications MUST accept only positive base-10 integers. Missing, zero, invalid, or overflowing values MUST use the documented default.

RRB-C2. All limits are process-local. This version requires no multi-instance cache coherence or distributed quota coordination.

## 2. Upstream discovery response bodies

RRB-UD1. `MONOIZE_UPSTREAM_DISCOVERY_MAX_BYTES` MUST select the maximum response-body byte length yielded by the HTTP client for every upstream provider-model discovery and model-metadata discovery request. Its default MUST be `16777216`. Parsing MUST follow RRB-C1.

RRB-UD2. Before reading a discovery response body, Monoize MUST compare a valid HTTP `Content-Length` value with the selected limit. If `Content-Length` exceeds the limit, Monoize MUST reject the response without reading a body chunk.

RRB-UD3. Monoize MUST read a chunked response or a response without `Content-Length` incrementally. Before appending each chunk, Monoize MUST reject the response if the accumulated byte length plus that chunk would exceed the selected limit. Monoize MUST stop polling the body after this rejection.

RRB-UD4. A response whose yielded body length equals the selected limit MUST be accepted. Empty response bodies MUST be accepted by the byte reader.

RRB-UD5. Discovery code MUST parse JSON or construct error text only from bytes returned by the bounded response reader. Discovery code MUST NOT call `reqwest::Response::json`, `reqwest::Response::text`, or `reqwest::Response::bytes` directly.

RRB-UD6. A body rejected by RRB-UD2 or RRB-UD3 MUST produce an error whose message states the configured byte limit. Dashboard provider-model discovery MUST return HTTP `502` with code `upstream_discovery_response_too_large`. A body transport failure within the limit MUST return the subsystem's existing upstream-fetch error.

## 3. Registered environment limits

RRB-R1. The following process-local limits parse per RRB-C1 with the listed defaults:

| Env | Default | Controls |
|---|---|---|
| `MONOIZE_SQLITE_CACHE_KIB` | 16384 | SQLite per-connection page cache (KiB) |
| `MONOIZE_SQLITE_MMAP_BYTES` | 67108864 | SQLite `mmap_size` |
| `MONOIZE_SQLITE_READ_CONNECTIONS` | 8 | SQLite read pool size |
| `MONOIZE_SQLITE_JOURNAL_SIZE_LIMIT_BYTES` | 268435456 | SQLite `journal_size_limit` (WAL file ceiling after checkpoint) |
| `MONOIZE_TOKIO_WORKER_THREADS` | 6 | async runtime worker threads |
| `MONOIZE_TOKIO_MAX_BLOCKING_THREADS` | 64 | async runtime blocking pool bound |
| `MONOIZE_UPSTREAM_RESPONSE_MAX_BYTES` | 67108864 | bounded read of non-stream upstream response bodies |
| `MONOIZE_REQUEST_LOG_RETENTION_DAYS` | 365 | request-log retention |
| `MONOIZE_REQUEST_CAPTURE_MAX_TOTAL_BYTES` | 1073741824 | capture dumps aggregate cap |
| `MONOIZE_SSE_MAX_CONNECTIONS_TOTAL` | 64 | global dashboard SSE connection cap |
| `MONOIZE_IMAGE_TRANSFORM_JXL_THREADS` | 4 | libjxl encoder native threads per encode |
| `MONOIZE_ACTIVE_PROBE_TICK_SECONDS` | 5 | active-probe scheduler tick |
| `MONOIZE_HTTP_CLIENT_POOL_IDLE` | 90 | reqwest idle-connection timeout (seconds) |
| `MONOIZE_HTTP_CLIENT_POOL_MAX_IDLE_PER_HOST` | 32 | reqwest idle connections per host |
| `MONOIZE_SPEND_WINDOW_CACHE_TTL_MS` | 5000 | spend-window preflight cache TTL (`0` disables) |
| `MONOIZE_SPEND_WINDOW_CACHE_CAPACITY` | 10000 | spend-window cache entry capacity |
| `MONOIZE_REGISTRY_SNAPSHOT_TTL_MS` | 60000 | routing registry snapshot TTL (`0` disables) |
| `MONOIZE_DASHBOARD_AGG_CACHE_TTL_MS` | 10000 | dashboard aggregate cache TTL (`0` disables) |
| `MONOIZE_DASHBOARD_AGG_CACHE_CAPACITY` | 256 | dashboard aggregate cache entry capacity |
| `MONOIZE_FORWARD_INFLIGHT_LIMIT` | 0 | global forwarding admission slots (`0` disables) |
| `MONOIZE_FORWARD_QUEUE_TIMEOUT_MS` | 5000 | forwarding admission queue budget |
| `MONOIZE_STUDIO_BRIDGE_*` | — | see `studio-bridge.spec.md` §1 |

## 4. Forwarding admission control

RRB-FA1. When `MONOIZE_FORWARD_INFLIGHT_LIMIT` is a positive integer, every forwarded
generation request (`/v1/chat/completions`, `/v1/messages`, `/v1/responses`, and their
protocol variants) MUST acquire one slot from a process-global counting semaphore
before the routing phase begins.

RRB-FA2. A request that cannot acquire a slot within `MONOIZE_FORWARD_QUEUE_TIMEOUT_MS`
MUST receive HTTP `503` with code `gateway_saturated` and header `Retry-After: 2`. It
MUST NOT reach the upstream dispatch phase.

RRB-FA3. The slot MUST be released when the downstream response reaches a terminal
state: response body fully sent, stream closed, or request aborted. A dropped future
(client disconnect) MUST release the slot via guard drop.

RRB-FA4. `MONOIZE_FORWARD_INFLIGHT_LIMIT` equal to `0` (the default) MUST disable the
semaphore entirely; requests then proceed without admission control.

RRB-FA5. Dashboard, auth, and admin endpoints MUST NOT consume forwarding slots.

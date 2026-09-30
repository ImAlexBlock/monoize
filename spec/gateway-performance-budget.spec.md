# Gateway Performance Budget Specification

## 0. Status

- **Purpose:** State the throughput and latency budget the Monoize forwarding gateway
  MUST meet on the production node, and the benchmark that proves it.
- **Scope:** The `monoize` process between downstream request arrival and upstream
  dispatch, and between upstream completion and downstream response completion. Upstream
  generation time is excluded. Database, spool, and runtime configuration are in scope.

## 1. Load model

GPB1. The budget load is `R` requests per minute, `1 <= R <= 2000`, mixed as:
- streaming generation requests (the reference benchmark drives 100% streaming; a
  non-streaming minority does not add gateway-side database work per request),
- at most 200 distinct API keys,
- at least one key contributing 90% of the volume (burst concentration).

GPB1a. The reference benchmark MAY additionally drive dashboard aggregate polling
at 1 request per 10 seconds; when it does not, the dashboard aggregate cache TTL
(DPT-DA2) still bounds the same queries' read-pool footprint in production.

GPB2. The benchmark upstream responds with a fixed SSE body at a constant 200 tokens
per second and 300 ms time-to-first-byte. The benchmark upstream MUST NOT be the
bottleneck; its idle capacity MUST exceed the offered load by at least 10x.

## 2. Latency budget

GPB3. Gateway overhead is `request_duration - upstream_duration` measured at the
benchmark client. For requests served at any load in GPB1, gateway overhead MUST
satisfy: p50 <= 50 ms, p90 <= 150 ms, p99 <= 500 ms, max <= 2000 ms.

GPB4. Time-to-first-byte observed by the client MUST satisfy: p50 <= upstream TTFB +
50 ms, p99 <= upstream TTFB + 500 ms.

GPB5. At the load boundary `R = 2000`, for a 60-second window: requests that fail
with `gateway_saturated` (RRB-FA2) MUST be less than 0.1% of offered requests, and
requests that fail with any other 5xx MUST be zero.

## 3. Resource bounds under budget load

GPB6. At `R = 2000`, process CPU MUST remain below 70% of the configured worker
capacity (worker threads x 100%).

GPB7. At `R = 2000`, sqlx connection-pool acquire waits logged by the slow-acquire
threshold MUST be zero in any 60-second steady-state window.

GPB8. The WAL file MUST NOT exceed `MONOIZE_SQLITE_JOURNAL_SIZE_LIMIT_BYTES` in
steady state, measured 10 minutes after the benchmark starts.

## 4. Benchmark

GPB9. The benchmark MUST run the production binary with a SQLite database seeded
with at least 500000 `request_logs` rows attributed to the concentrated key of GPB1,
so spend-window aggregates exercise realistic history depth.

GPB10. The benchmark MUST ramp `R` through 100, 500, 1000, and 2000, holding each
step for at least 60 seconds, and report the GPB3/GPB4/GPB5 metrics per step.

GPB11. The benchmark result is reproducible: a script in the repository builds the
seed database, starts the process, drives the load, and prints one JSON summary.

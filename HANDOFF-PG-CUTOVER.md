# HANDOFF: Gateway 2000-RPM Capacity Work + PostgreSQL Cutover

Date: 2026-09-30. Branch `Monoize-Claude` at `9efeaee7` (pushed to origin).
Everything below is the exact state and the remaining runbook. Read fully before touching production.

## 1. What is already done (code, pushed)

| Item | Where | Note |
|---|---|---|
| Spend-limit preflight cache (5s TTL, single-flight) | `src/db_cache.rs`, `src/users/org_limits.rs` | Kills the per-request full-history `request_logs` SUM (2x per request). Settlement still reads directly; invalidates after charge. Spec DPT §7. |
| Routing registry snapshot | `src/monoize_routing.rs` | Kills 3 full-table scans per request. Generation bump on provider/group writes + 60s TTL. Spec DPT §8. |
| Dashboard aggregate cache (10s) | `src/dashboard_agg_cache.rs`, handlers | Stops dashboard polls from pinning the hot-path read pool. Spec DPT §9. |
| Forwarding admission control | `src/app.rs` | `MONOIZE_FORWARD_INFLIGHT_LIMIT` (default 0 = off), queue budget 5s, `503 gateway_saturated` + `Retry-After: 2`. Spec RRB §4. |
| SQLite read pool default 8, WAL journal_size_limit 256MB | `src/db/mod.rs` | Helps even before PG. |
| PG pool 48 connections (env `MONOIZE_PG_POOL_CONNECTIONS`) | `src/db/mod.rs`, spec DB9 | Per operator review. |
| PG full-migration fixes (3 real bugs) | `src/migration/m20260827_000051`, `m20260914_000077`, `m20260922_000121` | Verified: empty PG database applies all 95 migrations; runtime smoke (t8) passes. |
| Migration tool | `src/bin/sqlite-to-pg.rs` | Idempotent upsert by PG PK; incremental watermarks for `request_logs` (`created_at_unix_ms`) and `billing_ledger` (`created_at`). End-to-end tested against a real PG. Spec DB §12. |
| Tests | `tests/pg_migration_smoke.rs`, `tests/gateway_benchmark.rs`, unit tests in `db_cache.rs`/`dashboard_agg_cache.rs` | Benchmark: `cargo test --release --test gateway_benchmark -- --ignored --nocapture` (env `GATEWAY_BENCH_STEP_SECONDS`, `GATEWAY_BENCH_SEED_ROWS`). Bench client must use `.no_proxy()` (macOS system proxy hijacks 127.0.0.1). |

Dialect audit per operator note: every `instr(`/`strftime(`/`datetime(`/`json_valid` site in
migrations and runtime has a Postgres branch (verified by inspection; grep list in session log).

## 2. Production state RIGHT NOW (64.90.22.212)

- **Serving**: old container `monoize` (image `monoize:72d970c9`) on SQLite, port 8081 (Caddy active). PigCode key `70132be1` still flooding (~90% of traffic); `sqlx` slow-acquire warnings ongoing until cutover.
- **PG ready**: container `monoize-postgres` (postgres:18-alpine, `--network host`, listening `127.0.0.1:5433`, user `postgres`, password `MonoPGx7K2vQ9wE4t`, database `monoize`, data at `/opt/monoize/pgdata`). Clean init done; database is EMPTY (schema applied by cutover script step 2).
- **Build tree**: `/opt/monoize/build-93576b7e` contains rev `9efeaee7` sources (db/mod.rs patch applied). An incremental build was dispatched (`nohup ... > build2.log`, builder container `monoize-build-9efeaee7`) — **completion NOT confirmed; check `tail build2.log` for `INSIDE-BUILD-OK`**.
- **One-shot cutover script**: `/opt/monoize/monoize-pg-cutover.sh` (in repo copy at `HANDOFF` time also in git as `/tmp` copy; re-upload from this repo if missing). NOT yet executed.
- Disposable leftovers to delete when done: container `monoize-pg-migtest` (127.0.0.1:15432), `/tmp/monoize-snapshot.db`.

## 3. Remaining steps (in order)

1. Confirm build: `tail -2 /opt/monoize/build-93576b7e/build2.log` → expect `INSIDE-BUILD-OK`.
   If the builder container died, re-run:
   `cd /opt/monoize/build-93576b7e && nohup docker run --rm --network host --name monoize-build-9efeaee7 -v /opt/monoize/build-93576b7e:/src -v monoize-cargo:/usr/local/cargo -v monoize-buildcache:/src/target monoize-builder:30dca059 bash /src/build-inside.sh > build2.log 2>&1 &`
2. Execute `nohup /opt/monoize/monoize-pg-cutover.sh > /dev/null 2>&1 &` then watch
   `tail -f /opt/monoize/cutover-9efeaee7.log`. The script does, in order:
   docker image `monoize:9efeaee7` → disposable container applies all migrations to the real
   `monoize` PG database (gate: ≥95 rows in `seaql_migrations`) → online SQLite snapshot to
   `/tmp/monoize-snapshot.db` → bulk `sqlite-to-pg` migrate → row-count gate on 14 tables →
   writes `/opt/monoize/env-extra.txt` with the PG DSN (this is the swap script's official
   injection point; keep the file after cutover) → `blue-green-swap.sh 9efeaee7` → final
   `--incremental 1` pass from the live (now quiesced) SQLite → health checks.
3. The swap itself enforces BG1..BG17 (SIGHUP lease handover at BG11, connection drain at BG12).
   **Never skip/force-stop the drain** — MONOIZE_SWAP_DRAIN_MAX_SECONDS is an alert threshold only.
   Long-lived streams may make the drain take a while; that is by design.
4. Post-cutover verification:
   - `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8095/healthz` and `/readyz` → 200.
   - `docker exec monoize-postgres psql -U postgres -p 5433 -d monoize -t -c "SELECT count(*) FROM request_logs;"` → grows with live traffic.
   - `docker logs --since 5m monoize` → no `sqlx::pool::acquire` slow-threshold WARNs (the original incident signature).
   - TTFB from request_logs: `SELECT percentile via created_at_unix_ms window, avg(ttfb_ms)` — should drop from ~30s to sub-second.
5. Archive, do not delete, the SQLite file (PGMS6): keep `/opt/monoize/data/monoize.db*` read-only evidence.
6. Cleanup: `docker rm -f monoize-pg-migtest monoize-pg-verify monoize-pg-init 2>/dev/null`; remove `/tmp/monoize-snapshot.db`.
7. Locally (Mac): collect the two pending validation results when they finish:
   `cargo test --release --test gateway_benchmark -- --ignored --nocapture` (2000-RPM budget)
   and the full `cargo test` (both were still compiling/queued at handoff time).

## 4. Rollback (if PG misbehaves after swap)

The old SQLite file is untouched by cutover (bulk pass reads a snapshot; final pass reads it too).
Rollback = redeploy previous image `monoize:72d970c9` via the same blue-green swap, and remove
`MONOIZE_DATABASE_DSN` from `/opt/monoize/env-extra.txt`. Data written to PG after cutover
(request_logs/balances) would need manual reconciliation from PG back to SQLite — avoid
lingering: decide rollback fast or not at all.

## 5. Access & environment gotchas

- SSH from Chad's Mac: password auth, via mihomo proxy only. Working wrappers were
  `/tmp/sshy` (base64 command), `/tmp/scpy` (scp), `/tmp/mz` (ControlMaster auto-rebuild).
  The link was extremely flaky on 2026-09-30 (fail2ban + proxy resets). Direct TCP to :22
  opens but SSH handshake gets cut. Expect retries; batch commands; never assume a session survives.
- If PG ever reports `role "postgres" does not exist`: the data dir is stale —
  `docker rm -f monoize-postgres && rm -rf /opt/monoize/pgdata && mkdir -p /opt/monoize/pgdata`
  then re-run the original docker command (in cutover script comments / session log).
- env-extra.txt survives as the DSN injection point for FUTURE swaps too (swap script appends
  it after the captured env, last value wins).

## 6. Open items not blocking cutover

- Ghost-drain (client disconnect keeps draining upstream, `src/handlers/streaming.rs:249`,
  ~10.3M tokens/day wasted) — known, deferred, billing semantics undecided.
- PigCode (key 70132be1) rate limiting decision (business action, not code).
- `MONOIZE_FORWARD_INFLIGHT_LIMIT` tuning after observing PG-mode capacity.

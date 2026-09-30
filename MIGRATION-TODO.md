# Server migration TODO

Date: 2026-09-30 (Asia/Shanghai).
Source: 64.90.22.212. Target: 40.160.141.21.
Status: final databases restored; target applications running on loopback only.
Source applications are stopped. Public ingress and end-to-end acceptance remain pending.

## Verified

- [x] Clone `Monoize-Claude` at `f405eb2e`.
- [x] Read `AGENTS.md` and `HANDOFF-PG-CUTOVER.md`.
- [x] Authenticate to both servers.
- [x] Confirm source runs `monoize:9efeaee7`, not the handoff's old image.
- [x] Confirm source has `monoize-postgres` with `/opt/monoize/pgdata`.
- [x] Confirm target has 8 CPUs, 22 GiB RAM, and approximately 188 GiB free.
- [x] Confirm target has no Docker executable and no application in `/opt`.

## Data and deployment

- [x] Confirm active database backend without printing credentials: PostgreSQL.
- [x] Verify cutover completion marker and final incremental log: 83 tables; SQLite archived.
- [ ] Inventory databases, roles, extensions, files, logs, spools, secrets, certificates, scheduled jobs, and proxy dependencies.
- [ ] Inspect actual blue-green scripts and connection ownership.
- [ ] Record health, errors, traffic, pool wait, and latency baselines.
- [ ] Select a cross-host cutover that preserves active streams and prevents divergent writes.
- [ ] Create consistent backups and verify restoration on the target.
- [x] Preserve original images and restricted deployment configuration.
- [x] Install Docker on target; candidate PG port binds to localhost only.
- [ ] Validate all table counts, critical balances, keys, settings, sequences, and constraints.
- [ ] Verify target health and authenticated streaming with controlled traffic.
- [ ] Complete final synchronization and route new traffic to the target.
- [ ] Drain old streams without a forced deadline.
- [ ] Reconcile final writes and document rollback after target writes begin.
- [ ] Verify public DNS, TLS, proxy routing, and restart persistence.

## Capacity and acceptance

- [ ] Run bounded concurrency tests without exhausting paid upstream accounts.
- [ ] Measure p50/p95/p99 queue time, first content token, completion time, errors, and throughput.
- [ ] Tune admission control and database pools from measured capacity.
- [ ] Verify disconnect, timeout, backpressure, and stream flushing behavior.
- [ ] Record tests and outstanding upstream limitations.
- [ ] Commit and push sanitized TODO and implementation changes.

## Safety constraints

Do not execute historical cleanup instructions against the live PG data directory.
Do not assume an older SQLite copy contains writes committed after the PG cutover.
Preserve unrelated source services, including apeiron and traework2api.
Never commit server passwords, DSNs, database dumps, or private keys.
Do not stop production instances with active streams.

## Progress and remaining gates

Both database dumps, original images, restricted configuration, archived SQLite,
and a Redis RDB snapshot reached the target with matching SHA-256 checksums.
Backups reside in `/opt/migration-20260930` on both hosts.
Logical dump archives passed `pg_restore --list`.
Both target PostgreSQL restores completed with `pg_restore --exit-on-error`.
The isolated monoize candidate returns HTTP 200 for `/healthz` and `/readyz`.
Snapshot checks: 87 public tables, 106 users, 95 migration entries,
660258 request logs, and 529212 billing ledger entries.
This is not final synchronization or a production traffic cutover.
Source remains the only production system.

The active Caddy configuration depends on UID-restricted NAT rules.
Do not copy Caddy configuration alone and assume its upstream ports are correct.
`MONOIZE_APEIRON_URL` uses `https://apeiron.lynshen.org`; keep that dependency reachable.

GitHub collaborator access now works. Migration TODO commits were pushed with
author and committer `ImQianji`.

Cross-host final synchronization needs an approved traffic admission window or
a reviewed shared-database transition. Existing streams must drain before stopping
their instances. Do not allow the old and new databases to accept independent
production writes. DNS ownership and the separate `/tw2a` route remain unverified.
No upstream concurrency benchmark or first-token improvement has been validated.

## Current Verification Evidence

- User intentionally disabled DNS resolution; do not restore DNS automatically.
- Supplementary archive includes the actual `/etc/sub2api-release-proxy` configuration,
  retained frontend assets, Caddy storage, and the source Caddy binary.
- Supplementary archive transferred with matching SHA-256.
- Target full-content manifest covers 87 Monoize tables and 100 trae2api tables.
- Source online manifest: 667398 request logs and 536287 billing ledger rows.
- These online manifests are not a final consistency proof.
- Source spool was empty at 14:56 UTC on 2026-09-30.
- One remaining accepted Monoize connection had approximately 2.5 MB queued for transmission.
  Preserve that connection; an empty spool does not prove a completed response.
- A read-only drain observer runs on the source; it cannot stop services or switch traffic.
- The source's standard swap script still reads archived SQLite.
  Do not execute it unchanged against the current PostgreSQL deployment.

Source baseline, 15-minute completed-request window:
59 successful `deepseek-v4.1-flash` requests; average input 25408 tokens;
logged TTFB p50 4685 ms, p95 9105.6 ms, p99 10425.12 ms.
The same window contained 42 client-disconnect records and 6 errors.
No pool-acquire warning appeared in the sampled five-minute application log.
Logged TTFB is not yet verified as first-content-token latency.

## Authorized Connection Closure

At 23:08 Asia/Shanghai on 2026-09-30, the operator authorized closing the stalled stream.
The exact source socket was `127.0.0.1:8080 -> 127.0.0.1:38510`.
Ownership and its unchanged 2549509-byte send queue were checked before closure.
Only this socket was closed. Caddy was not restarted or reloaded.
The queued response bytes were not delivered; this was an operator-approved interruption.

At 23:09 Asia/Shanghai, all monitored application ports had zero accepted connections.
The active Monoize spool contained zero files and zero admission records.
Request logs remained at 667398 rows.
Application containers still run. Background writes are not fenced.
Final backups, final database comparison, target production activation, and DNS restoration
remain outstanding.

## Current State: 2026-10-01

This section supersedes the intermediate status notes below.

- Source application containers exited with code 0 and remain retained with restart disabled.
- Source PostgreSQL, Redis, Caddy, Apeiron, and traework2api remain available.
- Final stopped-writer backups were checksum-verified on the target.
- Final databases restored separately as `migration_final` in the two target PostgreSQL containers.
- Monoize table hashes, indexes, constraints, and sequences match the final source manifest.
- All 855 Monoize columns match after normalizing dropped-column ordinal gaps; relative order matches.
- All 1423 trae2api columns match after relative-order and UTC normalization.
- All 29 differing trae2api CHECK definitions reparse identically in PostgreSQL temporary tables.
  No business tables were changed by this comparison; the validation transaction rolled back.
- The raw manifests retain these metadata differences rather than concealing them.
- Target Monoize is healthy on `127.0.0.1:8080`.
- Target trae2api primary and backup are healthy on `127.0.0.1:7883` and `:7882`.
- Their pool limits are 32 open and 8 idle connections per instance.
- Redis runs on `127.0.0.1:16379`; AOF is active and enabled in the persisted configuration.
- The systemd-managed local proxy runs on `127.0.0.1:7869` with request/response buffering disabled.
- Exactly three TRAE channel URLs now use that local proxy; original URLs are preserved in the final backup directory.
- A real, 64-token-budget TRAE stream returned HTTP 200, first content at 3674.9 ms, and `[DONE]`.
  This is a single short-request sample, not a capacity benchmark or a latency improvement claim.

Target applications have started background writes. Do not restart source writers as a rollback shortcut.
Before a reverse cutover, drain target requests, stop target writers, and reconcile or migrate target changes.
The source pre-cutover data no longer represents a guaranteed current rollback database.

Remaining: public TLS and routing, authenticated Monoize end-to-end inference, bounded concurrency tests,
restart persistence tests, role/grant verification, complete file verification, and updated deployment scripts.
Keep DNS disabled until ingress tests pass and the operator restores it.

### TLS and Bounded Readiness Checks

The target `migration-ingress.service` now serves TLS on loopback port 443.
`www.lynshen.org`, `api.lynshen.org`, and `trae.joinreso.com` each returned HTTP 200
with successful certificate verification using explicit loopback resolution.
No public DNS change occurred. Public ingress is not yet open.
The `/tw2a` route forwards to the retained source service with certificate verification;
that forwarding path still requires its own acceptance test.
Certificates are loaded manually; automatic renewal is not yet configured.
The earliest certificate expiry is 2026-11-23 for `www.lynshen.org`.

Authenticated `/v1/models` returned HTTP 200 and 19 models.
Bounded `/readyz` tests completed 256 requests at each concurrency level:
8, 32, and 64. All 768 responses were HTTP 200.
At concurrency 64, p95 was 80.03 ms and p99 was 310.69 ms.
These Python-client measurements are readiness-endpoint evidence, not inference capacity.
Full authenticated inference, streaming concurrency, public access, and renewal remain unverified.

## Final Synchronization Progress

The source applications subsequently exited gracefully with code 0.
An application-scoped admission rule rejects new loopback connections.
The original containers, restart policies, firewall snapshot, and databases remain available.
Apeiron, traework2api, Caddy, Redis, and PostgreSQL remain running.

Final backups are under `/opt/migration-20260930/final` on both hosts.
Both final database dumps, roles, Redis snapshot, and application data archives
transferred with matching SHA-256 checksums.
The backup process confirmed zero other database clients before each dump.

The final Monoize dump restored into target database `migration_final`.
All table-content hashes, constraints, indexes, extensions, and sequence metadata matched.
The raw column metadata digest differed because historical dropped columns left ordinal gaps.
A separate comparison covered all 855 columns: 45 physical ordinal differences,
zero type/default/nullability differences, and identical relative live-column order.
The manifest tool still needs ordinal normalization before issuing an unqualified equality report.

The final trae2api restore has not yet run because the first comparison stopped the job.
Target production applications remain stopped.
Do not restore DNS or restart old application writers while completing this migration.

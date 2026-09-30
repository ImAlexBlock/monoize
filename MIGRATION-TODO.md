# Server migration TODO

Date: 2026-09-30 (Asia/Shanghai).
Source: 64.90.22.212. Target: 40.160.141.21.
Status: discovery. No traffic or database cutover has occurred in this task.

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

GitHub rejected pushes with HTTP 403 for the configured account `ImQianji`.
Local TODO commit exists; remote synchronization requires repository write access.

Cross-host final synchronization needs an approved traffic admission window or
a reviewed shared-database transition. Existing streams must drain before stopping
their instances. Do not allow the old and new databases to accept independent
production writes. DNS ownership and the separate `/tw2a` route remain unverified.
No upstream concurrency benchmark or first-token improvement has been validated.

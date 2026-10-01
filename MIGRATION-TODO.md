# Server migration TODO

## Dashboard Regression Correction (2026-10-01)

The previous full-site completion claim was premature: Store and revenue pages
were not covered by the inference and readiness acceptance checks.
The operator reported two failing dashboard pages, reproduced as HTTP 500:
Store catalog and current-day revenue.

The stored token columns were BIGINT. PostgreSQL SUM(BIGINT) returned NUMERIC,
which the revenue reader tried to decode as i64.
Store payment-channel revision was INTEGER while the reader expected i64.
Fixes explicitly project the query results as BIGINT, including historical
revenue call counts. No production table or business record was rewritten.

The unmodified code failed a fresh PostgreSQL regression on INT4 channel revision.
The fixed code passed the PostgreSQL regression, five revenue tests, and thirteen
Store tests. The PG fixture covers large token counts, NULL tokens, empty days,
channel decoding, and persisted revenue readback.
Four deployment-adapter tests and the isolated-network routing test also passed.

Candidate image `monoize:20261001-dashboard-pg` passed eight authenticated GET
checks against the real database before routing new Caddy connections to port 8081.
Public CDN checks subsequently returned 200 for catalog, exchange rate, entitlement,
orders, revenue daily, exclusions, and Store primary status.
Temporary diagnostic sessions were removed.

At the last check, the old instance retained three accepted connections.
The supervised deployment is waiting without a force-stop deadline.
Both instances are healthy; old Store-primary ownership remains until the
forwarding pause, empty-connection recheck, and lease handover complete.
Do not run another swap or stop the old instance while this drain is pending.

## Current Authoritative State (2026-10-01)

The target is serving migrated traffic through the CDN and direct TRAE DNS.
`www.lynshen.org` uses the CDN with origin `40.160.141.21`.
`api.lynshen.org` and `trae.joinreso.com` resolve to the target.
API, TRAE, and WWW certificates are enrolled in Certbot and have deploy hooks.
The active target containers are healthy.
The source application writers remain stopped.
The source host remains available for retained services and recovery evidence.
The operator must manage DNS and CDN changes; this agent does not revert them.

The historical notes below preserve the migration trail.
Where they say DNS, public ingress, or certificate work is pending,
use this section as the current state and retain the historical qualification.

## Latest Ingress Evidence (2026-10-01)

### Direct Certificate Renewal Accepted

The API/TRAE production certificate is now deployed through a lineage-scoped hook.
Its expiry is 2026-12-30. Both live TLS fingerprints match the validated certificate.
The hook checks certificate lifetime, hostnames, key match, and system trust before deployment.
It stages versioned certificate files, validates the candidate Caddy configuration,
retains the previous configuration, and loads through the local administration API.
No application or host restart was performed.

`certbot renew --cert-name migration-direct-ingress --dry-run --run-deploy-hooks`
passed, including the actual deploy hook and subsequent live fingerprint verification.
The Certbot timer is enabled and the ingress remains active.
Monoize readiness confirmed PostgreSQL reachable after the test.
This accepts renewal for `api.lynshen.org` and `trae.joinreso.com` only.
`www.lynshen.org` is now also enrolled as `migration-www-ingress`.
Its production certificate expires 2026-12-30.
Its certificate deployment hook passed live fingerprint verification.
The Certbot renewal dry-run with the deploy hook passed.
Earlier notes stating the WWW certificate was manual or pending are historical.

### ACME Validation Update

Certbot is installed on the target.
A separate nginx HTTP configuration serves only the three named service hosts.
The challenge directory is `/var/lib/migration-acme/.well-known/acme-challenge`.
Existing HTTPS Caddy and application processes were not reloaded for this change.
External normal-DNS requests to API and TRAE returned the exact random challenge content.
Certbot's staging and production enrollment for API, TRAE, and WWW succeeded.
The installed Certbot timer is enabled.
The deploy hook validates each allowed lineage and loads it through Caddy's admin API.

The same external WWW challenge request initially failed before the Caddy challenge route was added.
After the route was added, the exact random token passed through the CDN and matched.
The CDN continues to serve normal WWW requests from the configured HTTPS origin.

The operator retained the CDN and changed the WWW HTTPS origin to the new host.
Normal DNS requests through the CDN returned 200 for `/`, `/readyz`, and `/healthz`.
Unauthenticated `/v1/models` returned 401.
A real authenticated stream through `www.lynshen.org` returned content, `[DONE]`, and EOF.
CDN headers reported `BYPASS`, and the target database recorded the same request ID:
`86200bf0-a5bd-45df-957b-f7a4b38ea6bb`, status `success`.
Client first-content latency was 4283.36 ms for this single request.
This proves the CDN request reached the target, not merely a cached health page.
It does not establish CDN latency percentiles or buffering behavior for long streams.

The target-host DNS check resolves `api.lynshen.org` to the new host.
After the operator's subsequent TRAE DNS update, an external source-host check
resolved `trae.joinreso.com` to `40.160.141.21` without overrides.
Its HTTPS homepage and `/health` returned 200 with certificate verification enabled;
unauthenticated `/v1/models` returned 401.
The earlier old-address/502 observation is superseded by this external check.
WWW retains its CDN configuration. Old application writers remain stopped.

ACME path audit used random tokens before enrollment.
The final WWW challenge path returned the exact token through the CDN.
API and TRAE staging dry-runs passed.
WWW production enrollment and deploy-hook dry-run passed.
The earliest current certificate expiry is 2026-12-30.

Date: 2026-09-30 (Asia/Shanghai).
Source: 64.90.22.212. Target: 40.160.141.21.
Status: target applications running; public ingress and direct-domain renewal validated.
Source applications are stopped. DNS/frontend ownership remains operator-managed.

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

### End-to-End Acceptance Update

The initial loopback channel URL failed the existing SSRF guard.
Do not enable `MONOIZE_ALLOW_PRIVATE_UPSTREAM` as a workaround.
All three TRAE channels now use their original `https://trae.joinreso.com` URL.
The target Monoize container has persistent Docker ExtraHosts mapping that hostname
to `40.160.141.21`; no public DNS record was modified.
TLS ingress binds both loopback and the target public address.
External Linux probes verified all three domain certificates and HTTP 200 health responses.

One request followed by four concurrent requests passed the complete
Monoize-to-TRAE streaming path with real credentials and a 64-token output budget.
All five received content, `[DONE]`, and EOF; server logs recorded `success`.
The four concurrent first-content times were 3024.85, 3175.48, 3410.79, and 4254.68 ms.
These are short-request measurements, not proof of sustained high-concurrency capacity.
An earlier client stopped at `[DONE]` and produced `client_gone` records;
the corrected verifier reads through EOF and rejects missing completion.

The container was gracefully recreated only after verifying zero accepted sockets
and an empty spool. ExtraHosts is now part of its Docker configuration.
Readiness after recreation confirmed PostgreSQL reachable and primary role.
`/tw2a/` returns the same 404 at source and target; its complete API behavior is not verified.
The independent tw2a service remains on the old host.
Certificate renewal, higher inference concurrency, restart recovery, filesystem/role audits,
and deployment/runbook consolidation remain outstanding.

### Capacity Failure and Persistence Audit

The larger probe did not meet acceptance and must not be described as successful:

- Requested budget 64: eight concurrent requests returned content in 8/8 cases;
  sixteen concurrent requests returned content in 14/16 cases.
- Requested budget 256: eight concurrent requests returned content in 6/8 cases.
  The empty responses ended with `stop`, `[DONE]`, and EOF.
- The test stopped on the first failing stage. No 32-concurrent stage was run.
- Direct TRAE-proxy isolation using the same internal channel returned content in 8/8 cases.
- No key, provider, or global custom transform was found for the tested path.
- The root cause remains unproven. Successful server accounting records do not prove visible output delivery.

The source TRAE cluster's missing `monoize` login role is now restored without superuser privilege.
Persistent mounts, restart policies, systemd enablement, and two immutable encryption files were checked.
See `MIGRATION-RUNBOOK.md` for active resource names, data locations, checks, and reverse-cutover safeguards.

### Private Repository and Empty-Answer Fix Acceptance

The canonical remote is now the private repository `Libra1337/monoizeovo`.
All three source branches migrated with identical commit IDs and complete history.
The current GitHub account has push permission but cannot delete the old public repository.
The latest API check returns 404 for the old repository; deletion versus access removal is not independently confirmed.
Exposed-credential rotation remains outstanding.
Do not push deployment information to the previous public remote.

An exact replay of the synthetic upstream request reproduced reasoning-only empty answers
directly at TRAE, without Monoize. Reasoning was preserved by Monoize as `reasoning_details`.
The earlier inference that reasoning disappeared was incorrect.
The TRAE fix rejects empty `stop` answers and bounds unpublished reasoning before first useful output.
All TRAE service tests passed; the new tests also passed three race-detector repetitions.
The immutable candidate image was validated and promoted as a healthy primary/backup pair.
The proxy now targets ports 17884 and 17885; previous instances remain retained.

Post-promotion full-path acceptance completed one request plus batches of 8, 16, and 32:
all 57 returned visible text, `[DONE]`, EOF, and server-side `success`.
At concurrency 32, first-content p50 was 2717.85 ms, p95 8051.99 ms, and p99 8092.75 ms.
At concurrency 16, p95 was 10476.61 ms; do not omit this slower stage from comparisons.
The proxy's actual descriptor limit is 65536.
No diagnostic key remains enabled.
These finite short-request batches do not establish sustained throughput or long-output efficiency.
Preserve pre-fix failed reports. Continue credential rotation, renewal, boot recovery,
long-output testing, and sustained-load acceptance before marking the full goal complete.

### Credential Rotation and Output Test

The active target Monoize PostgreSQL password was rotated.
Tests on the application's host-network authentication path accepted the new password
and rejected the previous password. A random wrong password was rejected before rotation.
The updated Monoize container is healthy and its database is reachable.
External Linux TLS probes for all three service domains returned HTTP 200.
No secret values were printed or committed; SSH credentials were not changed.
The source retained cluster and obsolete configuration archives still require separate remediation.

Old target TRAE instances exited with code 0 after drain checks and now have restart disabled.
The active repaired instances use ports 17884/17885 and pools of 16 open / 4 idle connections each.

Four concurrent longer-output probes produced 1010-1060 visible characters and 46-48 content chunks each.
All received `stop`, `[DONE]`, and EOF. First content took 6056.9-7214.5 ms.
Inter-chunk p95 was 64.62-68.25 ms; largest observed inter-chunk gap was 97.15 ms.
Three responses did not meet the requested final-marker check.
Report `long-output-acceptance-46b06dfd.json` therefore remains failed.
Do not equate protocol completion with compliance with the requested answer format.
Reported completion tokens exceeded the requested 1024 budget on three responses;
that field can include reasoning and must not be represented as visible-output tokens per second.

### Source Credential Revocation and Bounded Continuous Run

The source retained Monoize PostgreSQL password has also been rotated.
There were zero client backends and the source Monoize application was stopped before the change.
Only the `postgres` role's loopback TCP authentication was changed from trust to SCRAM.
The new password succeeds; the previous password and a random wrong password are rejected.
Local Unix-socket administration remains available.
No SSH password or unrelated service was changed.
Original HBA and protected recovery material remain on the source in `final/source-credential-revocation`.
Old application environments and role dumps contain obsolete credentials and are not restart instructions.

Report `sustained-stream-3639212e.json` completed 60 short requests over 120.21 seconds.
Submissions were two seconds apart with at most eight in flight.
All 60 returned text, successful stream termination, EOF, and server-side success records.
First-content p50/p95/p99 were 2638.52/6876.46/12893.57 ms.
This verifies a bounded 30-RPM sample, not long-duration endurance or maximum system capacity.

The active TRAE backup was gracefully stopped after verifying zero accepted sockets.
It restarted with the same image, became healthy, and passed a real streaming request.
The active primary was not restarted.
Full-host reboot recovery remains untested.

Final target health inspection found all three application containers healthy.
The target host's resolver still returned a separate frontend address for `www.lynshen.org`,
no addresses for `api.lynshen.org`, and the old host for `trae.joinreso.com`.
This observation does not override the operator's intentional DNS withdrawal.
Public DNS and any separate frontend's origin routing require operator action or confirmation.
If `www` intentionally remains behind a frontend, update that frontend's origin instead of replacing its DNS blindly.
Manual TLS currently works, but automatic certificate renewal remains disabled and unverified.

### Post-Cutover Restore Drill

After target writes began, the current databases were backed up independently at
`/opt/migration-20260930/postcutover-backups/20260930T222838Z-997dd8`.
The Monoize dump restored into a temporary database with 87 tables, 106 users,
5 public functions, and 6 non-internal triggers.
The trae2api dump restored into a temporary database with 100 tables, 1 user,
40 public functions, and 9 non-internal triggers.
Both temporary databases were dropped after verification.
The active `migration_final` databases remained reachable.
The backup and manifest contain credentials and remain only on the target.
The drill does not prove Redis, mutable file, full-host boot, or reverse-cutover recovery.

### Final Runtime Audit

On the latest audit, Monoize and both repaired TRAE instances were running and healthy.
The local proxy targets `127.0.0.1:17884` with backup `127.0.0.1:17885`.
The ingress and local proxy systemd units were active.
Monoize readiness and both TRAE health endpoints returned 200.
Current database counts were 667572 request logs and 536460 billing ledger rows.
The migrated trae2api database contained 114 accounts and 16 API keys.

Public DNS was not changed:
`www.lynshen.org` resolved to `51.81.222.39`,
`trae.joinreso.com` resolved to `64.90.22.212`,
and `api.lynshen.org` did not resolve on the target.
Do not claim public cutover until the operator confirms the frontend origin and DNS plan.

The local trae2api worktree contains an unpushed documentation update.
The GitHub repository currently reports `public`.
Do not push deployment or credential documentation there.
Push it only after the owner makes the repository private or supplies a private replacement.

### Repository Privacy Gate

A fresh authenticated GitHub API check reports `Libra1337/trae2api` as public.
Earlier claims that this repository was private were not adequately verified.
Its visibility-change time is unknown.
The current collaborator cannot change repository visibility.
Do not push further deployment documents or credentials to that repository.
The current local TRAE documentation updates remain unpushed.
Store operational evidence only in verified-private `Libra1337/monoizeovo`.
Ask the owner to make TRAE private or supply a private replacement, then verify through the API.

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

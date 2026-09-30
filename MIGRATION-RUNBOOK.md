# Target Migration Runbook

Updated: 2026-10-01, Asia/Shanghai.

## State and Scope

The target is `40.160.141.21`.
The source is `64.90.22.212`.
Source application writers are stopped and retained.
Target applications have already written data.
Do not restart source applications against the old databases.

Public DNS was intentionally withdrawn by the operator.
No migration script restored it.
Public-IP HTTPS probes pass with explicit domain resolution.
The old host still serves Apeiron and the separate `/tw2a` application.
Do not decommission the old host.

Sustained high-concurrency acceptance is incomplete.
The TRAE empty-answer fix passed short-request bursts through 32 concurrency.
Preserve earlier failed reports; finite bursts do not establish sustained capacity.

## Target Components

- `monoize`: image `monoize:9efeaee7`, host network, UID/GID 1000, loopback port 8080.
- `trae2api-empty-answer-candidate`: image `sub2api:20261001-empty-answer-90e3235`, loopback port 17884; active primary.
- `trae2api-empty-answer-backup`: the same image, loopback port 17885; active backup.
- `trae2api-primary` and `trae2api-backup`: old images retained, stopped with restart disabled.
- `final-redis`: loopback port 16379; persistent RDB and AOF data.
- `migration-monoize-postgres`: loopback port 5433; application database `migration_final`.
- `migration-sub2api-dev-postgres`: loopback port 5434; application database `migration_final`.
- `final-trae-proxy.service`: nginx on loopback port 7869.
- `migration-ingress.service`: Caddy on loopback and `40.160.141.21`, port 443.

The names beginning with `migration-` include active final databases.
Do not remove them as temporary resources.
The earlier databases `monoize` and `sub2api` are test restores, not active production databases.

## Persistent Paths

The common root is `/opt/migration-20260930`.
The final backup and configuration directory is `/opt/migration-20260930/final`.

- Monoize data: `final/runtime/opt/monoize/data`.
- TRAE data: `final/runtime/opt/sub2api-dev/data`.
- Redis data: `final/runtime/redis`.
- Redis configuration: `final/redis-production.conf`.
- Monoize PostgreSQL: `migration-monoize-postgres-data`.
- TRAE PostgreSQL: `migration-sub2api-dev-postgres-data`.
- TLS configuration, certificates, and binary: `/opt/migration-ingress`.
- Local proxy configuration: `/opt/migration-20260930/final-trae-proxy.conf`.

Protect the environment files, role archives, dumps, and certificates.
Never place their contents in Git or public logs.
The final archives predate target writes. Create new consistent backups before further replacement.

## Health Checks

Run these commands on the target:

```sh
systemctl is-active docker migration-ingress final-trae-proxy
docker ps --format '{{.Names}} {{.Status}}'
curl -fsS http://127.0.0.1:8080/readyz
curl -fsS http://127.0.0.1:17884/health
curl -fsS http://127.0.0.1:17885/health
curl -fsS http://127.0.0.1:7869/health
curl --resolve www.lynshen.org:443:40.160.141.21 https://www.lynshen.org/readyz
curl --resolve api.lynshen.org:443:40.160.141.21 https://api.lynshen.org/readyz
curl --resolve trae.joinreso.com:443:40.160.141.21 https://trae.joinreso.com/health
```

Do not add `-k` to acceptance checks.
HTTP 200 alone does not prove streaming success.
Require nonempty visible output, `[DONE]`, EOF, and a matching successful request record.

## Routing and Safety

The three TRAE provider URLs remain `https://trae.joinreso.com`.
The Monoize container has Docker ExtraHosts `trae.joinreso.com:40.160.141.21`.
Include that mapping when recreating the container.
Do not replace the URLs with loopback addresses; the SSRF guard rejects them.
Do not enable the global private-upstream override to bypass this restriction.

The local proxy disables request and response buffering.
Its primary is port 17884; port 17885 is backup.
Keep database pools within the PostgreSQL connection budget.
Current TRAE pools allow 16 open and 4 idle connections per active instance.
The Monoize pool allows 48 connections in its separate PostgreSQL instance.

Container restart policies and systemd enablement were inspected.
A host reboot has not been tested.
Do not report boot recovery as verified until a controlled recovery test passes.

## Backups and Evidence

The source `final/QUIESCED` record follows empty-socket and empty-spool checks.
All source application containers exited with code 0.
Each final database dump began with zero other client backends.
Database dumps, Redis snapshot, application files, and role definitions transferred with matching SHA-256.

Per-table manifests cover all row content, indexes, constraints, extensions, and sequences.
Keep the raw comparison reports, including metadata mismatches.
Dropped-column ordinal gaps were checked separately using relative live-column order.
TRAE timestamp defaults were compared under UTC.
Twenty-nine CHECK expressions were reparsed in temporary tables and matched the target definitions.
These normalizations must not conceal unrelated schema differences.

The missing source `monoize` role was subsequently restored in the TRAE PostgreSQL cluster.
It is a login role without superuser privilege.
The original `sub2api` role remains unchanged.
The Monoize comparison key and payment-key file match the final file archive by SHA-256.
A complete post-activation filesystem comparison is not meaningful for mutable logs and caches;
classify immutable secrets separately from live generated files.

## Reverse Cutover

There is no safe one-command rollback to the old data.

1. Keep DNS unchanged until the reverse-cutover plan is validated.
2. Prevent new requests on the target without dropping admitted streams.
3. Drain every active application connection and durable request spool.
4. Stop target application writers gracefully.
5. Confirm no application client sessions remain in either target database.
6. Create and verify fresh target database, role, Redis, and file backups.
7. Restore those current backups on the intended reverse target.
8. Compare complete data manifests, schemas, sequences, and required role grants.
9. Validate credentials, health, streaming, and accounting before admitting traffic.
10. Enable exactly one writer location.

The source saved its original container configuration in `final/stopped-applications.json`.
Its admission rule is recorded in `final/admission-rule.json`.
Remove only that exact rule when reverse-cutover acceptance succeeds.
Do not flush shared iptables chains.
Do not restart old writers merely to restore an old homepage.

## Certificates and Remaining Gates

Certificates are currently loaded manually.
Automatic renewal is not configured or tested.
The earliest expiry is 2026-11-23 for `www.lynshen.org`.
DNS ownership or a working ACME challenge path is required before renewal acceptance.

Remaining gates include sustained inference concurrency,
long-response throughput, disconnect accounting, boot recovery, and certificate renewal.
The historical source blue-green script still reads SQLite.
Do not run it unchanged against this PostgreSQL deployment.

## Target Credential Rotation

The active target Monoize PostgreSQL password was rotated after the public-history exposure.
A host-network client verified that the new password succeeds and the previous password fails.
Monoize was recreated after empty-connection and empty-spool checks.
Its environment file and the database initialization environment file were updated.
External certificate-verified health probes passed after rotation.
No SSH credentials were changed.

Protected state and recovery material reside in `final/credential-rotation`.
Do not print or commit these files.
Retained containers and archived dumps can contain obsolete credentials.
The PostgreSQL container's captured initialization environment can also contain an obsolete value;
the database role and updated environment file are authoritative.
Do not recreate an initialized database from an old container environment.
Source-cluster credential rotation and historical-secret cleanup remain separate outstanding tasks.

# Deployment readiness for 40.160.141.21

Use this checklist after the final revision passes the Project self-check workflow.
Do not infer deployment completion from a successful build.

## Release artifact

1. Select the successful workflow run for the exact deployment commit.
2. Download `monoize-linux-x64-<full-commit-sha>`.
3. Verify `REVISION`, `monoize.sha256`, and `runtime-smoke.json` before staging the image.
4. Confirm the target reports `x86_64`. This artifact does not support ARM64.
5. Stage the artifact under `/opt/monoize/build-<revision>` without modifying the serving container.
6. Build `monoize:<revision>` with the staged Dockerfile and executable.
7. Record the built image ID and its `org.opencontainers.image.revision` label.

The executable builds on Ubuntu 24.04. The runtime image uses Ubuntu 24.04 with
`ca-certificates`, `curl`, and `libstdc++6`. The runtime smoke test executes that
combination as UID/GID `1000:1000`. It uses temporary SQLite storage with no network.
It checks readiness, liveness, embedded frontend modules, and graceful exit.
It does not establish PostgreSQL production compatibility or production readiness.

## Migration evidence

No migration source changed between baseline `db37dbc18aa7c07205448494696395369a8a15e0`
and reviewed revision `b9c50e08f611c0e2d38a9c51f2d58d1239fda609`.
The committed tree contains 97 files. Its raw Git blob digest (the Linux CI
artifact form) is:

```text
ad6b14f1805fb53501543608547b1577c85378d5032932668d97f7c1dbccd97b
```

The deployed PG2 function hashes files after filesystem extraction. With this
checkout's `core.autocrlf=true`, a Windows `git archive` extracts CRLF files and
produces the equivalent tree digest:

```text
0d581e605b0e72f2c8ac5ed43a75147626a5e263470104b2bb58fb7e5c627181
```

Both revisions `b48f4289` and `2c2236b0` produce `0d581e...` from the same CRLF
archive and `ad6b14...` from a raw LF archive. Compare the digest of the files
actually staged under `/opt/monoize/build-<rev>/src/migration`; the manifest MUST
record that exact byte-level value.

Use the final Linux artifact's raw migration files for the deployment comparison.
Windows checkout newline conversion can change the digest.
Compare the candidate tree with the source tree for the actual serving image.
The local baseline alone does not establish that the serving migration tree matches.
If either tree is missing, follow the protected image-ID manifest procedure in PG2.
Do not invent a serving digest or bypass a mismatch.

### Serving image provenance

`BACKEND-SELFCHECK.md`, under `Unified Recovery Build`, records source revision
`b48f4289b14a1ba809a894136af12cec17b02f02` for the unified recovery build.
Under `Unified Image Transfer Verified`, it records the transferred Docker archive
and loaded image. The current target inspection independently confirmed these values:

| Evidence | SHA-256 |
| --- | --- |
| Docker image archive | `da627a82c6a0fc33848ba45e33534c2e07f81f5743c7073a8ef36cd26f80f615` |
| Serving image configuration | `71023c43bc3a808ca9d43ee97cb11112fd3701f14f8e4b5e6933709181d7d6d9` |
| Serving executable | `7c9bb827f40541ec9999f2e05495cee2be7986ae3e445bc6d814893af2762420` |

The historical build record associates the source revision with the image archive.
The verified archive checksum associates the retained target archive with that record.
The image configuration digest identifies the loaded serving image.
The executable checksum records the inspected running artifact.
Do not use the inherited `9efeaee7` image label as the executable's source revision.

The 97 raw Git migration files at `b48f4289b14a1ba809a894136af12cec17b02f02`
produce the same byte-level migration tree digest as the reviewed candidate tree
after applying the same line-ending conversion.
Use this recorded provenance when preparing the root-owned, mode-`0600`
`/opt/monoize/migration-manifest.json` required when the serving source tree is absent.
Map the inspected serving image ID and the final candidate image ID to that digest.
Verify the final candidate's retained migration tree before adding its manifest entry.
Preserve the build record and archive-checksum evidence with the manifest.
This checklist does not write or install a production manifest.

## PostgreSQL entrypoint

Inspect the existing `/opt/monoize/blue-green-swap.sh` before installation.
The repository's `scripts/blue-green-swap.sh` is the archived SQLite implementation.
It does not dispatch to PostgreSQL. Do not copy it over the PostgreSQL entrypoint.

Install the reviewed `scripts/blue-green-pg-swap.py`, `scripts/blue-green-probe.py`,
and `scripts/blue-green-route.sh` under `/opt/monoize` as root-owned files.
For this PostgreSQL host, the project entrypoint must invoke the adapter:

```sh
#!/bin/sh
set -eu
exec /usr/bin/python3 /opt/monoize/blue-green-pg-swap.py "$@"
```

Preserve a copy of the previous entrypoint. Set the wrapper and routing helper mode
to `0755`. Verify Python 3, Docker, curl, iptables, ss, and the routing restore unit.
Do not execute an archived recovery script with a hardcoded image revision.

## Target settings and preflight

Use these non-secret probe settings for the requested host:

```sh
export MONOIZE_SWAP_PUBLIC_URL=https://www.lynshen.org/
export MONOIZE_SWAP_READY_URL=https://api.lynshen.org/readyz
export MONOIZE_SWAP_PUBLIC_IP=40.160.141.21
export MONOIZE_SWAP_DRAIN_MAX_SECONDS=14400
export MONOIZE_SWAP_CADDY_SERVICE=migration-ingress.service
export MONOIZE_SWAP_CADDYFILE=/opt/migration-ingress/Caddyfile
export MONOIZE_SWAP_CADDY_ADMIN_URL=http://127.0.0.1:2020
```

The three Caddy settings above identify the inspected service on this target.
Recheck them before deployment. Do not substitute the defaults `caddy`,
`/etc/caddy/Caddyfile`, and `http://127.0.0.1:2019`.
These settings select checks; they do not reload or alter Caddy.

Set `MONOIZE_SWAP_PG_CLIENT_IMAGE` to an inspected local PostgreSQL client image.
Alternatively, set `MONOIZE_SWAP_PG_CONTAINER` to the inspected PostgreSQL container.
The selected image supplies client binaries. The serving DSN selects the database.
Confirm the client version supports the serving PostgreSQL version.
Do not print the DSN, environment file contents, authentication tokens, or session data.

Verify the serving container uses host networking, UID/GID `1000:1000`, and the
expected `/app/data` mount. Verify the actual domain routing and TLS certificate.
Compare the active listen port, persisted routing state, running Caddy configuration,
and UID-restricted routing rule. Preserve all existing connection mappings.
Confirm the spare port is free and neither `monoize-next` nor `monoize-prev` exists.
Confirm the serving binary supports the SIGHUP Store lease handover marker.
Inspect available disk space for the custom-format backup and candidate image.
Review `env-extra.txt`; leave the serving DSN and deployment-controlled variables intact.

## Swap and acceptance

Run `/opt/monoize/blue-green-swap.sh <revision>` under a persistent supervisor.
Use a private timestamped output log. Preserve its backup and probe evidence.
The adapter verifies the backup directory with `pg_restore --list` and records its checksum.
This check does not perform a database restore.

Require candidate readiness and dashboard GET checks before switching new connections.
Require successful public TLS probes against `40.160.141.21` through the configured domains.
Wait for old connections to finish naturally before pausing forwarding and handing over the lease.
Treat four hours as an alert threshold. Do not force connection termination.
Do not reload Caddy or restart the serving container.

After finalization, verify the running image ID and revision, Primary lease ownership,
public readiness, route consistency, and the old instance's clean exit.
Retain the old instance's request-log spool and deployment evidence.
If a switch was attempted and the script fails, retain both instances for inspection.
Do not perform an automatic reverse swap or delete retained containers.

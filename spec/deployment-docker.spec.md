# Docker Deployment Specification (production host 64.90.22.212)

## 0. Scope

For the migrated PostgreSQL target, `scripts/blue-green-pg-swap.py` supplies the
project-owned `/opt/monoize/blue-green-swap.sh` implementation.
It uses the running container's image, environment, data mount, and host-network
settings, not the archived SQLite deployment paths.
It creates a PostgreSQL custom-format backup before candidate startup.
Candidate dashboard GET checks MUST pass before the UID/SYN-only route switch.
The same BG9-BG12 forwarding, drain, lease-handover, and no-force-stop rules apply.
Existing BG14-BG16 routing helpers remain responsible for connection preservation
and reboot routing. The live Caddy configuration MUST remain unchanged.

This specification defines `/opt/monoize/blue-green-swap.sh <rev>` on the
production host. The repository sources are `scripts/blue-green-*.sh`,
`scripts/blue-green-probe.py`, and `scripts/monoize-routing.service`.
It supersedes the PM2 procedure in `deployment-watchdog.spec.md` on this host.
SQLite is `/opt/monoize/data/monoize.db`, mounted at `/app/data`.
Release sources are `/opt/monoize/build-<rev>`. Backups are in
`/opt/monoize/backups`. Deployment may write these paths and install the
project routing unit at `/etc/systemd/system/monoize-routing.service` and its
Caddy dependency at `/etc/systemd/system/caddy.service.d/monoize-routing.conf`.

## 1. Definitions

- `stable`: the single port in `{8080, 8081}` referenced by Monoize upstreams
  in `/etc/caddy/Caddyfile`. It remains unchanged across updates.
- `active`: the last `MONOIZE_LISTEN` value of the container named `monoize`.
- `candidate`: the other port in `{8080, 8081}`.
- `proxy_uid`: the numeric UID of the running Caddy service account.
- `route state`: `/opt/monoize/blue-green-route.state`, containing three decimal
  integers: `stable active proxy_uid`.
- `accepted connection`: a TCP socket whose local port equals the application's
  listening port, excluding LISTEN, TIME-WAIT, and CLOSED states. Retain
  ESTAB, CLOSE-WAIT, SYN-RECV, FIN-WAIT, and other nonterminal states. Query
  once with `ss -tanH` and filter only after its successful exit. Outgoing
  sockets on other local ports do not count.

## 2. Invariants

BG1. Both application ports MUST bind `127.0.0.1`. Candidate startup MUST NOT
use `0.0.0.0`. Application UID is 1000 and MUST differ from `proxy_uid`.

BG2. Before deployment, the active container, persisted route state, Caddy
upstream, and installed routing rule MUST agree. A missing route state is
allowed only on initial installation when `active = stable` and the project
routing chain does not exist. Reject ambiguous state without stopping either
instance. Containers `monoize-next` and `monoize-prev` MUST not preexist.

BG3. The order MUST be: online backup; candidate startup; candidate readiness;
new-connection route switch; public readiness check; old connection drain;
Store forwarding pause; old connection recheck; lease handover; candidate
lease ownership confirmation; old container stop. Candidate readiness MUST NOT
wait for old connections to become zero. Old connections MUST NOT receive a
stop signal or a lease handover signal before the drain and recheck succeed.

BG4. Before startup, make a SQLite online backup with Python `Connection.backup`.
Run `PRAGMA quick_check` on the backup and require `ok`. Require identical
migration source trees for the serving and candidate revisions. Any difference
requires a separate compatibility assessment before this script can deploy it.

BG5. Capture the serving container environment in a mode-0600 file. Remove all
entries for `MONOIZE_LISTEN`, `MONOIZE_BOOT_STANDBY_LEASE`,
`MONOIZE_REQUEST_LOG_SPOOL_DIR`, `MONOIZE_DEPLOYMENT_PREVIOUS_URL`, and
`MONOIZE_DEPLOYMENT_CONTROL_TOKEN` before supplying exactly one replacement
for each. Reuse the serving restart policy, host network, UID 1000, and data
mount. Do not print the deployment control token or other environment values.
After the replacements, append `KEY=VALUE` lines from `/opt/monoize/env-extra.txt`
to the environment file when that file exists. An absent file changes nothing.
`env-extra.txt` lets an operator add or override candidate environment without
editing the swap script; entries never persist back into the serving container
environment.

BG6. Each candidate MUST have its own persistent request-log spool, under
`/app/data/request-log-spool-<rev>-<timestamp>`. Create its host directory with
UID/GID 1000 and mode 0700 before candidate startup. Do not copy, replay, move,
or delete a live previous instance's spool. Retain directories after a swap.

BG7. Before a route switch, failure may remove only the candidate created by
this invocation. After attempting the route switch, failure MUST retain both
containers. Do not reload Caddy, restart either instance, or revert a live
route as an automatic error handler. Keep the probe evidence and exit nonzero.

BG8. Successful finalization requires: one running container named `monoize`
with image `monoize:<rev>`; no `monoize-next` or `monoize-prev`; persisted and
live routing target the candidate port; public `/readyz` returns 200; and the
candidate owns the Store lease. Stop the old instance with SIGTERM only after
its accepted connection count is zero. Use a grace period of at least 15 seconds.

BG9. Candidate startup MUST set `MONOIZE_BOOT_STANDBY_LEASE=1` and configure
the temporary forwarding mode defined in `standby-deployment.spec.md`.
While the previous primary retains its lease, the candidate executes inference
locally and delegates lease-dependent Store and Replica operations exactly once.
Singleton primary background workers MUST defer until candidate lease ownership.

BG10. Keep `/etc/caddy/Caddyfile` and the running Caddy configuration unchanged.
The local entry `http://127.0.0.1:8095` remains the stable endpoint for same-host
consumers. Do not restart or reload Caddy during an application swap: its old
configuration may own long-lived HTTP, SSE, or WebSocket connections.

BG11. The old runtime MUST support SIGHUP lease handover. Probe the binary for
its `handing over the store_primary lease` marker before deployment. Before
SIGHUP, pause candidate Store forwarding with its authenticated control endpoint.
Pause completion MUST mean all admitted forwarded response bodies have completed.
Do not impose a total timeout on the pause call. On any pre-signal failure or
supervisor cancellation after attempting pause, attempt authenticated resume.
After signaling, retain both instances and let lease acquisition release the gate;
do not forward operations back to a primary that may have relinquished ownership.
Recheck old accepted connections while forwarding is paused. If nonzero, resume
forwarding and continue draining. Otherwise send SIGHUP, require candidate status
`mode = local` and `lease_owned = true`, then stop the old instance. A missing
capability, failed pause, failed connection query, or handover timeout MUST retain
both containers. Never mistake an empty owner result for successful handover.

BG11a. If the handover flag becomes true during a lease renewal retry round,
process handover before reacting to that round's failure. That failure MUST NOT
set the application shutdown flag. This rule applies to the updated runtime;
BG11 still protects an older runtime without this fix by draining before signal.

BG12. Poll old accepted connections every 15 seconds. The default
`MONOIZE_SWAP_DRAIN_MAX_SECONDS=14400` is an alert threshold only. Crossing it
MUST log the remaining connections and continue waiting without a stop signal.
A failed socket query MUST NOT be interpreted as an empty result.

BG13. Hold exclusive `/opt/monoize/blue-green-swap.lock` throughout the swap.
Run under a host supervisor that survives SSH disconnection. Do not launch
another deployment while old instances from a previous swap remain.

BG14. Route only TCP SYN packets from `127.0.0.1` to `127.0.0.1:stable` whose
socket owner is `proxy_uid`. Use one OUTPUT jump to the dedicated `MONOIZE_BG`
NAT chain. The chain MUST contain exactly one REDIRECT rule to `active`.
Existing flows retain their connection-tracking mapping. Application-originated
forwarding and direct administrator probes bypass this UID-restricted rule.
Do not flush shared firewall tables or modify another application's rules.

BG15. Every route transition MUST atomically replace the single REDIRECT target,
including when `active = stable`. Do not replace it with RETURN or remove the
last NAT expression: unregistering NAT hooks can break translated connections.
On initial setup, create the complete target chain before installing its OUTPUT
jump. SYN-only matching prevents retroactive redirection of existing untracked
connections. Validate rule shape before replacement and verify it afterward.

BG16. Write route state atomically before changing the live target. Restore it
on host boot using `monoize-routing.service`, ordered before Caddy and after
Docker/network startup. Caddy MUST want and follow the routing unit. The unit
MUST retry failure every five seconds without a start-rate limit. Failure MUST
NOT prevent Caddy from serving unrelated sites. A command failure may leave persisted/live targets
different; retain both containers for inspection. Neither target may be stopped
until the state and connection evidence establish the final serving role.

BG17. The route switch affects new Caddy upstream connections. An existing
HTTP keepalive connection may carry additional requests on its original instance
until it closes naturally. No deployment timer may close that connection.

## 3. Procedure

S1. Validate the revision, exclusive lock, images, source migration identity,
container names, active listener, Caddy UID, routing state, and handover support.

S2. Complete BG4 backup. Capture environment and create the independent spool.
Generate a random control token containing at least 32 ASCII characters.

S3. Start `monoize-next` on the candidate port, with BG5 overrides and its own
health command. Poll direct `/readyz` at most 120 times, at one-second intervals
and with a three-second timeout. Require HTTP 200 and authenticated deployment
status `forwarding` with `lease_owned = false`.

S4. Start the public HTTPS availability probe. Require its first sample to pass.
Read and retain the previous lease owner. Rename `monoize` to `monoize-prev`.
Persist and apply the candidate routing target according to BG14..BG16.

S5. Require public `/readyz` HTTP 200. Verify the live routing rule and candidate
status. Keep the cutover probe running for another two seconds, then finish it
and require zero failures. Stop public probe repetition before the drain: probes
may otherwise keep an old pooled upstream active indefinitely. New connections
now use the candidate. Keep the previous instance running.

S6. Apply the unbounded BG12 drain and BG11 forwarding pause/recheck loop.
Send SIGHUP only after the successful paused recheck. Poll authenticated candidate
status at most 60 times with one-second intervals for local lease ownership.

S7. Require old accepted connections to remain zero. Stop/remove `monoize-prev`,
rename `monoize-next` to `monoize`, and verify BG8. The cutover probe MUST have
recorded zero failed samples before reporting a clean deployment.

## 4. Verification

V1. `tests/blue_green_route.py` MUST run inside an isolated Linux network
namespace. It creates connections before initial routing setup, switches new
connections to the candidate, alternates targets 50 times, and verifies every
retained connection still reaches its original server. It also verifies UID
bypass, other-loopback-source bypass, and accepted-socket drain counts.

V2. `tests/blue_green_drain.sh` MUST verify unbounded waiting beyond the alert
threshold, failed socket-query rejection, empty-drain completion, and invalid
input rejection. Gate tests MUST prove that new guarded requests wait while
paused and cannot enter the previous primary after pause completion.

V3. From before the switch through two seconds after public verification, issue
HTTPS probes to
`https://www.lynshen.org/` every 400 ms and record status, exit code, and timestamp.
A health probe is evidence of endpoint availability, not proof about every user
request. During the drain, use direct candidate health checks instead of
repeated public requests. Verify public readiness again after finalization.
Preserve probe output when a swap fails or continues draining.

# Co-located CPA performance review — 2026-10-03

## Scope

CPA v8.0.12 (`2044a01f422998de79a5da8015141b878886534d`) and Monoize run on `40.160.141.21`.
The checks covered cache accounting, request latency, stream stability, and deployment continuity.

## Changes applied

1. Disable CPA's full successful-request log through the management API.
   Usage statistics, error diagnostics, routing, and file logging remain enabled.
   The change was hot-applied at `2026-10-03T03:05:23Z`.
   CPA's container ID, process ID, start time, and configuration inode stayed unchanged.
   A configuration backup is stored at `/opt/cliproxy/performance-audit-20261003/config-before-request-log-disable.yaml`.
2. Enable `session_affinity_auto=true` for the two CPA Channels that did not have it.
   Channel generations changed from `30` to `31` and from `7` to `8`.
   The complete channel snapshots were verified after each update.
   Monoize prunes existing health and affinity entries when a Channel is updated, so those mappings must warm again.
   The update did not restart CPA or cancel an in-flight request.

No global proxy, account pinning, service tier, retry budget, or reasoning effort was changed.
Those changes had no measured benefit and could alter account selection, cost, or cache identity.

## Measurements

The final production sample from `2026-10-03T02:29:00Z` to `2026-10-03T03:32:40Z` contained 151 successful requests.
The sample excludes the six synthetic benchmark requests.
For `gpt-6-astra`, 106 of 107 requests had positive cached tokens.
The cached-token coverage was `7,703,936 / 7,909,122 = 97.41%`.
Those requests used one account and one normalized session, and they predate both live changes.
They are a health baseline, not an attribution of the improvement to the changes.

Six controlled CPA requests used one account, one normalized session, 3,938 input tokens, and 5 output tokens.
The first three requests were cold and the next three reused the same cache key.
The warm requests reported 2,816 cached tokens (`71.5%` of the input).
All six returned HTTP 200 and matched CPA usage records.

The first substantive content times were:

| Protocol and effort | Cold | Warm |
| --- | ---: | ---: |
| Chat, `max` | 11.716 s | 9.425 s |
| Responses, `max` | 11.309 s | 11.415 s |
| Chat, `medium` | 21.436 s | 11.319 s |

Responses emitted an initial SSE event in about `0.75–0.88 s`, but substantive content still arrived around `11.3 s`.
The remaining wait occurs after CPA has accepted the upstream stream.
One cold request and one warm request per protocol/effort pair do not establish a stable ranking.

The benchmark connected directly to CPA's loopback listener.
It validates CPA cache and stream timing, but it does not prove that Monoize Channel affinity is active for every client.

## Stability checks

At the final check, CPA and both Monoize instances were running with `RestartCount=0` and `OOMKilled=false`.
CPA request logging was disabled without a process restart.
The production release `387c29a0` was built, pushed, and queued behind one existing port-8080 stream.
The queue waits for the natural drain, revalidates the Caddy and lease baselines, and then calls `/opt/monoize/blue-green-swap.sh 387c29a0` once.
It does not force-stop the old process or reload Caddy.

The measured bottleneck is upstream generation or scheduling after the first event, rather than CPU, CPA loopback networking, or cache lookup.

## Live first-event timing

Commits `f18a11de` and `dce86455` update the live request-log row when the first complete upstream SSE event arrives.
The row remains `pending` until the request ends.
Start, usage, error, and terminal events can set TTFB before visible text arrives.
Heartbeat comments do not set TTFB.
The timing update performs no database, spool, or network I/O.
Final usage and charges remain part of terminal settlement.

The frontend now applies buffered snapshots in reception order.
A later snapshot with the same row ID replaces earlier timing and status fields.
This fixes updates lost while a request-log tooltip is open.
The existing tooltip pause and table layout remain unchanged.

These changes expose timing earlier. They do not reduce the measured upstream generation delay.

## Remaining connection during deployment

At `2026-10-03T05:03:54Z`, one accepted connection remained on the old port 8080.
Its peer was Caddy, and its incoming byte count had not changed for about 13 hours.
Two samples, 6,864 seconds apart, showed 3,656 additional outgoing bytes in 457 data segments.
This equals eight bytes every 15 seconds and matches the default request-log SSE heartbeat.
The endpoint URL was not inspected, so the connection type is inferred.
The connection does not match an idle Store-forwarding pool: its peer is Caddy, and forwarding disables idle pooling.

The deployment process waits for this connection to close naturally.
An open request-log subscription has no maximum lifetime, so the queue has no guaranteed completion time.
Closing the corresponding log page lets its subscription end.
The connection closed naturally at `2026-10-03T05:28:13Z`.
The waiting drain then completed, and the queued `387c29a0` swap started at `2026-10-03T05:28:27Z`.

The `387c29a0` candidate received new connections at `2026-10-03T05:28:58Z` and acquired the Store lease at `05:31:05Z`.
The previous instance received SIGTERM after connection drain and lease handover.
It exited with code zero at `05:38:40Z`, about 7.5 minutes after SIGTERM.
The old deployment script stopped waiting after 120 seconds and reported a deployment failure.
The candidate continued serving requests.
After verifying the clean exit, runtime identity, route, readiness, and lease ownership, finalization completed at `05:40:51Z`.

Commit `4c9da6fb` changes the post-SIGTERM wait to continue until the old instance exits.
It emits one alert after 120 seconds and sends no additional stop signal.
This wait accommodates detached upstream work and background tasks after accepted connections reach zero.
The specific task that delayed this shutdown was not identified.

## Final verification

The final runtime revision is `4c9da6fbd82b69b6ff3fb13cb2634873b331ca6c`.
Its CI run is [37100536050](https://github.com/Libra1337/monoizeovo/actions/runs/37100536050).
The backend passed 2,022 ordinary Rust tests and the PostgreSQL checks.
The frontend, documentation, deployment, Apeiron, and rehearsal jobs passed.
The first-event tests verify pending timing before text, unchanged terminal timing, one billing settlement, and preserved upstream errors.
The frontend fix passed 47 request-log tests, type checking, and lint.
The strengthened local deployment suite passed 37 tests.
It includes a 301-second graceful shutdown and refusal of a nonzero container exit.

All seven CI jobs, including the release runtime check, passed.
The verified binary SHA-256 is `d04da07f645671b60fa14588bd9f12e96bb771634fa73d1c9dc2b2b28023ec45`.
The production image ID is `sha256:22cdc8efd4e9f5230a97c0b75b6657f281c292c189eb04a89371e4becc14e595`.
The release preserves the migration tree from the preceding runtime.
The official swap began at `2026-10-03T06:14:34Z`; its candidate started at `06:15:03Z`.
New connections reached the candidate after its readiness checks passed.

A bounded production log subscription observed seven requests in 5.6 seconds.
Six pending rows already contained TTFB.
One observed pending row then reached a terminal state with the same TTFB.
The check sent no model request and removed its temporary diagnostic session.
Evidence is stored at `/opt/monoize/build-4c9da6fb/production-live-timing.json`.

At `2026-10-03T06:16:53Z`, both instances were ready, with zero restarts and no OOM kill.
One connection remained on the old instance.
The supervised official swap continues waiting for natural drain and clean shutdown.
Its status is stored at `/opt/monoize/build-4c9da6fb/deployment-status.json`.

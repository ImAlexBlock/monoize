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
   CPA prunes existing health and affinity entries when a Channel is updated, so those mappings must warm again.
   The update did not restart CPA or cancel an in-flight request.

No global proxy, account pinning, service tier, retry budget, or reasoning effort was changed.
Those changes had no measured benefit and could alter account selection, cost, or cache identity.

## Measurements

The production baseline from `2026-10-03T02:29:00Z` to the first configuration change contained 151 successful requests in the sampled usage database.
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
These two samples per cell do not establish a stable protocol or effort ranking.

The benchmark connected directly to CPA's loopback listener.
It validates CPA cache and stream timing, but it does not prove that Monoize Channel affinity is active for every client.

## Stability checks

At the final check, CPA and both Monoize instances were running with `RestartCount=0` and `OOMKilled=false`.
CPA request logging was disabled without a process restart.
The production release `387c29a0` was built, pushed, and queued behind one existing port-8080 stream.
The queue waits for the natural drain, revalidates the Caddy and lease baselines, and then calls `/opt/monoize/blue-green-swap.sh 387c29a0` once.
It does not force-stop the old process or reload Caddy.

The measured bottleneck is upstream generation or scheduling after the first event, rather than CPU, CPA loopback networking, or cache lookup.

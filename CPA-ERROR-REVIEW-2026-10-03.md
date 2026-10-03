# Monoize and CPA error review — 2026-10-03

Inspect the four reported traces in the CPA usage database. Compare matching Monoize request logs with the request converter and stream decoders.
Read request metadata and redacted failure messages. Do not export request bodies, API keys, account names, or credentials.

| Trace | CPA result | Finding |
| --- | --- | --- |
| `874e01f5-e88e-487a-ab33-0380c093501a` | 400 at 09:41:46 UTC | A converted custom tool result retained its `ctco_...` item ID. The upstream required a function item ID. |
| `b08008aa-b400-4361-9ede-ebbcc8bed118` | 503 at 10:22:20 UTC | The upstream could not verify Daybreak Blue access. The same account and model later recorded 320 successful requests. |
| `59d4b3e0-123d-4770-94ea-85e1407f2c05` | 502 at 10:26:00 UTC | The upstream reported overload. The same account and model later recorded 292 successful requests. |
| `37f48718-119e-42b2-9653-b3f31784de7c` | 502 at 10:26:14 UTC | The upstream reported `server_error`. Monoize incorrectly recorded this SSE error as 400. |

The recovery counts end at the audit snapshot near 11:52 UTC. These counts describe requests after each trace, not controlled retry tests.
The three server errors used the same CPA account. Their subsequent successes support transient failure, not a persistent account-access failure.
The `NO_MORE_RETRY` metadata came from the upstream error. It does not establish how many CPA retries occurred.

Clear optional item IDs only when converting a custom call or result to a function item.
Keep `call_id`, content, namespace, and extension fields unchanged. Keep native function IDs and unconverted custom IDs unchanged.
Do not generate replacement IDs. This avoids injecting random values into replayed history.

For SSE terminal diagnostics, preserve valid explicit HTTP error statuses.
Without an explicit status, map structured overload and service-unavailable signals to 503.
Map `server_error` and `internal_server_error` to 502. Preserve other protocol fallbacks.
Use the resulting status in request logs and Channel health classification.
Do not replay a request after streaming starts. Do not change the HTTP status of an already committed response.

The review also found these separate failures:

- 57 custom/native `exec` identity collisions between 09:41:27 and 09:43:05 UTC. CTF-11a requires rejection when conversion would make identities ambiguous.
- 74 missing-tool-output errors across two models between 19:38 and 20:00 UTC on October 2. Request history is required to locate the missing correlation.
- 85 GLM and 55 GLM Flash premature stream endings in the reviewed 24-hour groups. These are incomplete upstream streams.
- Requests combining `image_gen` with `reasoning.effort=minimal` received explicit upstream 400 errors.
- CPA recorded rate-limit failures as well as service errors. Remaining displayed quota does not guarantee instantaneous capacity.

Do not treat `cf-cache: DYNAMIC` as model prompt-cache evidence. It describes the Cloudflare HTTP response cache.
Keep the current CPA session affinity and bootstrap-buffering settings. This change does not modify CPA accounts or process lifecycle.

## Validation and serving release

Commits `16077a66` and `ae74a160` contain the application fix and the Python 3.12 test harness fix.
All seven jobs passed in [CI run 37120417242](https://github.com/Libra1337/monoizeovo/actions/runs/37120417242).
The backend passed 2,027 ordinary Rust tests and the separate PostgreSQL checks.
The five new regressions passed, including streaming and nonstreaming Responses history conversion.
Documentation built for all four locales. The deployment wrapper passed eight local checks.

The serving release is `ae74a16053a7b081594b1624fce3ce0788d37b73`.
Its binary SHA-256 is `5241a1288628bb2c8e6e044a23ebfb791102201aa74b56e4049330d7b0fbf473`.
Its production image is `sha256:0330b24c831fabb9a2aba197e4e76724e4082df923ca7bef1651d7781b77fbbd`.
The migration tree matches the preceding release.

The official blue-green swap started at 12:15:58 UTC.
New Caddy connections switched to the candidate at 12:16:30 UTC.
All six public cutover probes passed. The database backup checksum and serving binary checksum matched.
Public static assets matched the release on both production domains.
Caddy configuration and PID remained unchanged. CPA was not restarted.

Three synthetic requests passed through the deployed Monoize and CPA chain:

| Request | Result | First SSE event | Total duration |
| --- | --- | ---: | ---: |
| Custom history, gpt-6.1-sol, nonstream | HTTP 200, success log | Not applicable | 11.624 s |
| Custom history, gpt-6.1-sol, stream | HTTP 200, success log | 0.801 s | 11.533 s |
| Custom result at input[156], gpt-6-luna, stream | HTTP 200, success log | 1.926 s | 2.450 s |

The last probe used `ctco_01a101a3-fc31-7522-8531-178d73452311` at the reported position.
It used synthetic content, not the user's original request history.
The matching new trace `1eaf2b8c-0d7e-42af-9262-37aa23727528` occurred at 12:06:59 UTC, before cutover.
All temporary probe credentials were deleted. Provider configuration generation remained unchanged.
The timing values describe these probes only. They do not establish a general latency improvement.

Between cutover and 12:22:00 UTC, the main CPA Provider recorded 44 successful requests and six parameter errors.
No custom item ID error appeared in that short window.
All six parameter errors rejected `image_gen` with `reasoning.effort=minimal`.
Use an upstream-supported reasoning setting when image generation is enabled.
The fix does not silently remove image tools or change the requested reasoning effort.

At the last drain check, one accepted connection remained on the previous instance.
The new instance was ready and served new connections. Both application instances had zero restarts and no OOM kill.
The official supervised swap continues waiting for natural connection drain and clean process shutdown.
This state is serving cutover, not completed old-instance finalization.
Inspect `/opt/monoize/build-ae74a160/deployment-status.json` and `swap.log` for the final state.

Local evidence is in `local-test/audit/error-fixes-final-ci.json`, `custom-id-live-probe-result.json`,
`custom-id-long-live-probe-result.json`, `error-fixes-runtime-evidence.json`, and `error-fixes-deployment-status.json`.


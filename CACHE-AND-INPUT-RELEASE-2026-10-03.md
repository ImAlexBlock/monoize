# Cache and input validation release: 2026-10-03

## Scope

Use `7e08a92ababecf3e3562a501c1b9f29e56d80762` as the release candidate on `Monoize-Claude`.
Include cache usage correction `4bbcb2cc4c655494e337a36d002d32fda6b71fe5`.
Retain the upstream comparison and prior performance evidence in `CUSTOM-TOOL-RELEASE-2026-10-02.md`.
Retain the project self-check evidence in `PROJECT-SELFCHECK-2026-10-02.md`.

## Cache findings

Read both Monoize request logs and CPA request/response usage on the deployed hosts.
Inspect CPA v8.0.10 at commit `6fecc6e5567912661654a4eaf9b8f5436facd1c2`.

The Monoize stream parser selected the first numeric cache counter before checking whether it was positive.
A zero counter could hide a later positive alias.
The non-stream Chat decoder did not cover the same aliases.
Both paths now select the first positive unsigned counter in the documented order.
Accept numeric strings. Skip missing, null, zero, negative, and invalid counters.
Keep inclusive input totals unchanged.
Cover 15 counter combinations through stream and non-stream decoders and downstream encoding.

CPA used round-robin account selection with session affinity disabled.
Requests in one observed session alternated among three Codex accounts.
The Monoize CPA Channel also lacked automatic session affinity headers.
Some incoming requests changed `prompt_cache_key` on every request despite matching prefix and tool hashes.
CPA received those changed keys from Monoize; CPA did not introduce those changes.
The original client request was not captured, so do not assign their origin to a specific client.

CPA upstream Responses and downstream Chat cache counters matched in the inspected failures to obtain cache hits.
Zero upstream cache usage in those requests was not a Monoize display loss.
Treat requests without usage as unmeasured, not as cache misses.

## Configuration and cache acceptance

Enable CPA session affinity with a one-hour TTL through its management API.
Apply this update at `2026-10-02T16:23:42Z` without restarting the container.
Preserve unrelated configuration and the process identity.
Retain the prior configuration at `/opt/cliproxy/cache-audit-20261003/config-before-affinity.yaml`.
Its SHA-256 is `ebde8ae343403c4fa3210fbd9e195250c90156fe0a5d40da79a8f146d4475459`.

Enable `session_affinity_auto` for the existing Monoize CPA Channel at `2026-10-02T16:28:01Z`.
Advance its Provider generation from 6 to 7.
Preserve models, Channel bindings, credentials, and transform rules.
Retain its prior setting at `/opt/monoize/build-dfd07196/cpa-affinity-before.json`.

Run three synthetic requests per model with identical payloads and stable per-model cache/session keys.
Use 4467 input tokens and five output tokens per request.
Run this configuration acceptance on the serving `dfd07196` binary.
Record candidate-binary acceptance separately after deployment.

| Model | First cached tokens | Second cached tokens | Third cached tokens |
| --- | ---: | ---: | ---: |
| gpt-6-astra | 0 | 3840 | 3840 |
| gpt-6-luna | 0 | 0 | 3840 |

All six requests completed with HTTP 200.
CPA selected one account within each three-request sequence.
CPA usage, downstream usage, and Monoize request-log counters matched after treating null log cache counters as zero.
Cached requests received the configured billing discount.
The total recorded charge was 999630 nano-USD, or USD 0.000999630.
Remove the temporary API key and dashboard session after verification.

These samples prove that the tested path can read cache after warm-up.
They do not establish a fleet-wide hit rate or guarantee hits on every repeated request.
Account availability, model identity, exact prefix, cache key changes, and upstream retention still affect hits.
CPA removes `prompt_cache_retention` from this Codex request path; a 24-hour cache cannot be guaranteed by that field.
Preserve caller-provided cache keys instead of replacing them globally.

At `2026-10-02T16:59:43Z`, inspect successful CPA requests since `16:28:02Z` with at least 1024 input tokens.
The sample includes all CPA clients and an uncontrolled workload.
Record 658 of 677 gpt-6.1-sol requests, 39 of 42 gpt-6-astra requests, and 4 of 20 gpt-6-luna requests with positive cache usage.
These counts do not establish the improvement caused by configuration changes.
The real gpt-6-luna path still has low observed cache reuse.
Read the relevant Monoize API Key transforms and global transforms; both are empty.
The CPA Provider contains only the previously configured custom-tool transforms.
No cache-key generation transform is active on this path.

## Empty input finding and behavior

Inspect request `c2b28f0d-3ad5-4444-aeaf-15aca1af1d96` at `2026-10-02T16:26:24Z` and its model fallback.
CPA received `messages: []` for gpt-6.1-sol and gpt-5.6-luna.
CPA then sent `input: []` with empty instructions to its upstream.
Neither incoming request carried a state reference, saved prompt, or instructions.
Monoize request capture was disabled. The original client body is unavailable.

Do not infer that the client originally sent an empty body.
Responses-only items and unmatched incremental tool results can become empty under existing cross-protocol rules.
No confirmed ordinary-text loss was found in this audit.

Validate the final encoded request before dispatch.
Skip a Chat Channel whose `messages` array is empty.
Skip a Responses Channel whose `input` array is empty unless a non-null state reference or saved prompt remains.
Continue eligible Channel attempts from the original request.
Preserve a later compatible Responses Channel and its native state.
If all candidates are empty, return `empty_input_after_conversion` and request non-empty input or complete history.
Do not dispatch, retry the same Channel, affect upstream health, or trigger model fallback for this local error.
Use the existing error event path for streaming requests.
Preserve existing routing failure behavior when a real upstream call fails.

This change prevents invalid empty upstream requests and clarifies the error.
It cannot reconstruct missing or protocol-specific server history.
Update the specification and troubleshooting pages in all four locales.

## Validation and production acceptance

Workflow [37039282868](https://github.com/Libra1337/monoizeovo/actions/runs/37039282868) passed all seven jobs for the final candidate.
The ordinary Rust all-target suite passed 2015 tests with zero failures.
Coverage includes four empty-input API tests, one state-preservation encoding test, and the cache-alias regression with 15 cases.
The PostgreSQL SQL checks and six live-database regressions passed.
Frontend tests, browser fixtures, lint, production build, four-locale documentation build, operational checks, rehearsal, and Apeiron checks passed.
The Linux release artifact passed its isolated Docker runtime check and exited with code zero.

Verify the binary SHA-256 as `7009b09b74f1a65d552c0bdbaae42eb6eb9f2f2fdc166625ad1f84e36b0e8163`.
Verify the unchanged migration tree as `ad6b14f1805fb53501543608547b1577c85378d5032932668d97f7c1dbccd97b`.
Build production image `sha256:6a76b5c8f13f289e4875f4e24684fe7b0f00d41a9cc1b75c240cf4def780ad15` from that artifact.
Run `/opt/monoize/blue-green-swap.sh 7e08a92a` under `monoize-swap-7e08a92a.service`.
Retain its log at `/opt/monoize/build-7e08a92a/swap.log`.

Switch new Caddy connections to port 8081 at `2026-10-02T18:03:50Z`.
Keep the original port 8080 available for its accepted connections.
Record route `8080 8081 999` and six successful availability probes with zero failures.
Keep Caddy PID 76263 and its configuration digest unchanged.
Pass the candidate dashboard and organization checks before switching the route.

Run six synthetic requests on the new candidate between `18:07:30Z` and `18:08:07Z`.
All six completed successfully with 4467 input tokens and five output tokens.
The gpt-6-astra cache sequence was `3840, 3840, 3840`.
The gpt-6-luna cache sequence was `0, 3840, 3840`.
CPA confirmed one account per model sequence and matching cache counts.
Monoize returned the same counters and recorded matching usage after normalizing null cache counters to zero.
The total synthetic charge was 476046 nano-USD, or USD 0.000476046.

Test four empty-input shapes in streaming and non-streaming mode: missing input, empty input, state reference only, and orphan tool result.
All eight returned `empty_input_after_conversion` on the selected Chat Channel.
Non-streaming requests returned HTTP 400; streaming requests returned an error event over the existing HTTP 200 stream.
All eight error logs had no charge and no model-fallback request.
Remove the temporary API key and dashboard session after the probes.

At `2026-10-02T18:14:31Z`, verify the candidate binary against the release artifact on the production host.
Both instances are healthy and use separate persistent request-log spools.
The PostgreSQL backup is `/opt/monoize/backups/pg-7e08a92a-1790964198391930083/database.dump`.
Its SHA-256 is `640f51e4658957db30f0215b854a9c6f330575bbf3bec7f3b569cc318d24c43f`.
Backup validation is not a full restore test.

The candidate serves new inference connections in `forwarding` deployment mode.
The previous instance still owns the Store lease and retains three accepted connections.
The supervised swap remains active and checks the old connection count every 15 seconds.
It will pause forwarding, recheck connections, transfer the lease, and stop the previous instance after the drain completes.
Do not force-close these connections or treat the alert threshold as a shutdown deadline.
This record certifies the candidate and route switch. Final lease handover and old-container shutdown remain pending at this timestamp.

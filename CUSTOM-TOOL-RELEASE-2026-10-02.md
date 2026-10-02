# Custom tool adapter release: 2026-10-02

## Scope and upstream comparison

The deployed application revision is `dfd0719676ba6ba718c05cf6fceb29ae86456543` on `Monoize-Claude`.
The later report-only commit does not change the deployed application.
The comparison used upstream `Ikaleio/monoize` at `5442a571832fdf70518ce001ca095384892ceada`.
Retain the previous full self-check and deployment evidence in `PROJECT-SELFCHECK-2026-10-02.md`.

The CPA Channel rejected `tools[0].custom` on its Chat Completions endpoint.
Its Provider had no custom-to-function transforms.
The previous adapter also restored unrelated native functions when configured with a wildcard.
It processed fragmented wrapped arguments before receiving a complete string.

Record actual conversions in a trusted context for each Channel attempt.
Share that context between request and response phases, including buffered responses.
Preserve native functions, namespaces, history, correlated results, and tool selectors.
Wrap each original custom input string once, including strings containing JSON.
Restore only calls identified by the recorded conversions.
Buffer converted arguments until completion; preserve usage and extension metadata.
Delay unnamed tool events only when the current rule has matching conversions.
Replay ordinary function events after identification.
Discard incomplete buffered arguments after a stream error.
Reject conflicting custom and native function identities before upstream submission.
Custom grammar constraints cannot be retained by a generic string function schema.

Update the converter specification and all four documentation locales together.
Retain the Provider, Channel, billing, organization, PostgreSQL, and deployment architecture.

## Performance changes

Skip SSE diagnostic frame formatting when no task-local capture exists.
Skip request and upstream-body snapshot copies when request capture is disabled.
Preserve emitted response bytes and enabled capture results.
Collect selected custom tool identities without cloning complete tool definitions or constructing unused schemas.

Do not import upstream `ca3dc581` as a whole.
Durable spool reservations, settlement ordering, and routing types differ in this branch.
The isolated capture benchmark measures frame construction only.
It does not establish gateway throughput or production latency.

## Validation

[Workflow `37019977895`](https://github.com/Libra1337/monoizeovo/actions/runs/37019977895) passed all seven jobs for this revision.
The release job built and verified the Linux artifact in an isolated Docker runtime.
The smoke instance exited with code zero.

| Check | Result |
| --- | --- |
| Rust ordinary all-target suite | 2009 passed; zero failed. |
| PostgreSQL SQL construction | 14 passed. |
| PostgreSQL migration, dashboard, organization, request logs, aggregates, and Replica ingest | Six dedicated regressions passed. |
| Converter coverage within the Rust suite | 20 unit tests and four API integration tests passed. |
| Frontend | 342 unit tests, 28 browser fixture cases, nine launcher tests, lint, and production build passed. |
| Documentation | All four locales built; 706 pages exported. |
| Deployment and packaging | 47 Python operational tests, four release-package tests, Bash drain checks, and isolated routing checks passed. |
| Rehearsal | 148 tests passed. |
| Apeiron | Frontend build, Rust server compilation, and Go worker tests passed. |

The Rust jobs executed 2031 passing tests, including repeated migration preparation and one capture microbenchmark.
Do not interpret this execution count as distinct coverage.
The image-format and gateway-capacity benchmarks were not executed.

The debug-profile capture microbenchmark used 100000 iterations and a 1024-byte payload.
Eager construction created 100000 strings totaling 104500000 bytes in 68517102 nanoseconds.
Lazy construction created zero capture strings in 9951454 nanoseconds.
These byte counts describe cumulative constructed data, not peak memory.
This measurement excludes network, routing, billing, and upstream model latency.

## Production acceptance

The deployment used `/opt/monoize/blue-green-swap.sh dfd07196` on `40.160.141.21`.
New connections switched to the healthy candidate while the original instance drained.
One long-running Caddy connection remained and continued to receive data.
The user then explicitly requested immediate disconnection and recovery.
At `2026-10-02T15:39:51.245982+00:00`, the agent closed only the established socket `127.0.0.1:8081 <-> 127.0.0.1:42584`.
That in-flight request was interrupted and requires a client retry.
Do not interpret the successful cutover probes as evidence of uninterrupted service for that request.
The swap script completed at `2026-10-02T15:40:09Z`, approximately 18 seconds after the disconnection.
The script transferred the Store lease, confirmed zero old connections, and gracefully stopped the original instance.
The original instance exited with code zero and remains available as a retained container.
Caddy retained its process ID and configuration hash throughout the deployment.

| Production check | Result |
| --- | --- |
| Acceptance time | `2026-10-02T15:40:54Z` |
| Application revision | `dfd0719676ba6ba718c05cf6fceb29ae86456543` |
| Image ID | `sha256:ed14b66410ac3f1ac36afb140e89f5705a7f7045ae96f23daa39e392dc559be4` |
| Binary SHA-256 | `94476194afe66bf072b01c8e6f33194eee8f90018cad1437e0fe03288ac199d3`; matches the CI artifact and isolated runtime smoke. |
| Readiness | PostgreSQL reachable; primary role; deployment mode `local`; Store lease owned. |
| Public domains | `www.lynshen.org` and `api.lynshen.org` passed TLS readiness and asset checksum checks. |
| Routing | Stable port 8080 selects candidate port 8080 for Caddy UID 999; routing check passed. |
| Original instance | `/monoize-before-dfd07196-1790955609387437723`; exit code zero; zero old connections. |
| Caddy | PID `76263` and configuration unchanged. |
| Cutover probes | 6 samples; 0 failures. |
| Dashboard | 14 authenticated GET checks returned HTTP 200. |
| Organization usage | 1 authenticated member-usage GET check returned HTTP 200. |
| Diagnostic sessions | Removed; cleanup queries returned zero. |

The verified PostgreSQL backup is `/opt/monoize/backups/pg-dfd07196-1790953750637242650/database.dump`.
Its SHA-256 is `0bb8402949a5386f0cf3afeea17b3e2f68298c914dabaf5d79a8f61f41ddc86b`.
The migration digest remains `ad6b14f1805fb53501543608547b1577c85378d5032932668d97f7c1dbccd97b` for 97 migration files.

## Provider configuration and real upstream acceptance

The dashboard API updated Provider `oai-代理` (`a96403a1-fa46-440d-a166-56d8b41b6b82`).
Its Channel remains `37392bac-5e90-45f1-9f50-781a1a0982d4` with the `chat_completion` protocol.
The update added paired request and response `field_custom_tools_to_function` rules with `names: ["*"]`.
Both rules apply only to these exact existing models:

- `gpt-5.6-luna`
- `gpt-5.6-sol`
- `gpt-5.6-terra`
- `gpt-6-astra`
- `gpt-6-luna`
- `gpt-6-sol`
- `gpt-6.1-sol`

The configuration generation increased from 5 to 6.
Readback confirmed that the transform rules were the only configuration change.
The previous transform configuration is retained at `/opt/monoize/build-dfd07196/cpa-transform-before.json` with root-only access.

Three synthetic requests reached the real CPA upstream through `/v1/responses` using `gpt-6-astra`.
The ephemeral API key was scoped to the target Group and model, with a USD 1 total spending limit.
Request capture remained disabled, and no returned tools were executed.

| Case | Result | Input tokens | Output tokens | Charge, nano-USD |
| --- | --- | ---: | ---: | ---: |
| Custom tool, non-streaming | HTTP 200; type, name, and exact input verified. | 395 | 20 | 74250 |
| Custom tool, streaming | HTTP 200; type, name, and exact input verified. | 395 | 20 | 74250 |
| Native function control | HTTP 200; type, name, and exact input verified. | 391 | 21 | 74400 |

Streaming delta content matched the completed custom call input.
The native function remained a native function with its original JSON argument shape.
All three persisted logs recorded the expected Provider and Channel, success status, and no error code.
The total recorded charge was 222900 nano-USD (USD 0.0002229).
The temporary API key and session were deleted after acceptance; cleanup queries confirmed zero matching records.
Public readiness remained healthy after the real upstream checks.
These real upstream checks cover `gpt-6-astra` only; the other six configured models were not individually exercised.
Generic function conversion preserves the raw input string but cannot enforce the original custom grammar.

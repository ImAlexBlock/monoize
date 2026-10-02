# Project self-check: 2026-10-02

## Upstream review

Compare the local `Monoize-Claude` baseline `db37dbc18aa7c07205448494696395369a8a15e0` with `Ikaleio/monoize` revision `5442a571832fdf70518ce001ca095384892ceada`.
The common ancestor is `f1918d3c00af01dc1285bda7a224b0ae22d90b6a`.
The baseline contains 496 local-only commits and 239 upstream-only commits after that ancestor.
Several commit pairs implement equivalent changes with different hashes.
Commit counts therefore do not measure missing functionality.

| Upstream change | Decision | Adaptation or reason |
| --- | --- | --- |
| `4d4a38d9`: Responses function-tool strict defaults | Adapt | Emit `strict: false` for cross-protocol function tools without an explicit value. Preserve native Responses defaults, explicit values, and parameter schemas. |
| `477cbc52`: Anthropic cache TTL | Adapt | Accept `5m` and `1h`. Preserve client markers. Allow later API key rules to retune verified Monoize markers without consuming another cache slot. |
| `798f43d2`: automatic Anthropic caching and advancing tool breakpoint | Already adapted | The local branch already supports request-level automatic caching and marks the final trailing tool result. Retain the shared four-slot limit. |
| `b0d456c8`, `37fc1d23`: Codex Responses route aliases | Already adapted | Retain the existing aliases and endpoint-equivalence regression coverage. |
| `611fcc64`, `12d8b9b1`, `373b8441`: image MIME, tool-image compression, Safari composer | Already adapted | Preserve the local image and composer corrections. Do not replace their extended regression coverage. |
| `c00c5c41`, `533154fe`: upstream response model observations | Already adapted | Retain mismatch-only request-log observations and local billing safeguards. |
| `5442a571`: raise default Anthropic output cap to 128000 | Exclude | A universal higher cap can exceed older models' limits. Retain 64000 and preserve explicit client output caps. |
| `ca3dc581`: request-path performance changes | Exclude from this release | Local caches and admission control already address overlapping paths. Upstream routing, pricing, and subscription types differ from this branch. |
| `ca3dc581`: smaller spool reservations and unlocked publication | Defer | Changes affect durable fallback reservations and crash recovery. Require separate concurrency, capacity, and crash tests before adaptation. |
| Upstream model-pricing, recharge, and sliding-window subscription migrations | Exclude | Preserve local Provider pricing, Store settlement, organizations, PostgreSQL schema, and migration history. |
| Upstream management-page and settings-page replacements | Exclude | Preserve local organization, billing, and dashboard recovery behavior. Adapt individual verified fixes separately. |
| Upstream nightly release workflow | Exclude | Release destinations and build assumptions differ. Use the repository's own self-check and deployment workflows. |
| Upstream test-suite removal | Exclude | Retain local protocol, billing, routing, deployment, and database regression suites. |

The strict-default adaptation additionally records Gemini function origins and recursively records nested function-tool origins.
The cache adaptation rejects unknown configuration fields and unsupported TTL values.
It does not change account denomination, settlement calculations, Channel selection, or deployment behavior.

The upstream cache implementation identifies ownership by target name alone.
That approach can overwrite a client marker after another transform deletes or reorders the marked node.
The local adaptation stores the target index, input node count, and SHA-256 digest of the complete marked node.
Request-level ownership stores the marker digest.
A later rule must verify the saved snapshot before changing the TTL.
Changed targets lose ownership and retain their existing markers.
Equal input nodes have ambiguous identity; discard ownership before recording or verifying their snapshots.
This prevents equal nodes from transferring ownership when they exchange positions.
Run transforms that edit nodes before the cache rules.

Update `unified_responses_proxy.spec.md`, `auto-cache-transforms.spec.md`, `urp-v2-rust-core-mapping.spec.md`, and `upstream-protocol-sync.spec.md` with these contracts.
Update all three Anthropic cache transform pages in `en`, `zh`, `zh-TW`, and `ja`.
Describe cache charges through the configured Provider billing Profile; do not import upstream fixed-price assumptions.

Add four protocol tests in `src/urp/cross_protocol_tool_strict_tests.rs`.
Cover Chat legacy and modern tools, Messages, Gemini, explicit strict values, native Responses defaults, and nested namespaces.
Add nine cache tests in `src/transforms/anthropic_cache_tests.rs`.
Cover configuration validation, TTL precedence, client ownership, context isolation, four-slot retuning, attempt isolation, node deletion, reordering, marker replacement, and indistinguishable nodes.
Run the existing `upstream_sync_tests` with the new tests in the Rust validation gate.
The adaptation passed `git diff --check`; record completed runtime validation in the remaining self-check sections.

Preserve local protocol safeguards during future synchronization.
These include bounded Responses SSE parsing, late terminal usage, cross-family custom-tool conversion, tool-result image relocation, and cache-breakpoint rejection retries.
Preserve native reasoning, citations, local transform identifiers, and the existing downstream WebSocket bridge.
Do not replace the protocol directories wholesale with upstream files.

## Implementation fixes

| Area | Implemented change | Required behavior |
| --- | --- | --- |
| API key authorization | Distinguish an unrestricted selection from an empty key-plan intersection. | Reject disjoint nonempty Group selections. Never expand an empty intersection to all accessible Groups. |
| Billing Profile cache | Scope mutations to the affected Profile cache and refresh dependent pricing consumers. | Preserve other cached Profiles. Restore optimistic cache values when a mutation fails. |
| Frontend verification | Restore unit and browser test scripts. Resolve browser fixture file URLs with `fileURLToPath`. | Run the intended suites on Windows and Linux. Include user-load, organization-limit, and composer fixtures. |
| npm launcher | Detect the `.pnpm` directory with Windows or Unix path separators. | Select the pnpm reinstall command without matching directory-name substrings such as `.pnpm-backup`. |
| PostgreSQL deployment | Derive backup and lease-query connections from the serving container's database DSN. | Use a private environment file. Require a valid dump, SHA-256 evidence, migration compatibility, and verified public readiness. |
| Deployment configuration | Reject protected overrides and preserve restart policy retry counts. | Retain the serving database, independent request-log spool, and deployment control settings. |
| Stream continuity | Preserve pause, drain, lease handover, and cancellation boundaries. | Never stop an instance with remaining accepted connections. Never force termination at the drain alert threshold. |
| Public probes | Require explicit HTTPS URLs and target IP for PostgreSQL deployments. Disable automatic curl configuration loading. | Verify certificates even when the operator's curl configuration contains `insecure`. Preserve status and exit-code evidence. |
| CI memory | Set the frontend and release Node heap limit to 4096 MiB. | Address the observed Node heap exhaustion during the frontend build. Require a successful subsequent build. |
| Apeiron layout | Remove the incomplete duplicate Go module and misplaced worker test from `apeiron/server`. | Test the Rust server and the Go worker in their actual project directories. |
| Deployment credentials | Remove the historical PostgreSQL password from tracked handoff text and the cutover script. Require an operator-supplied DSN. | Keep credentials out of tracked operational instructions and script defaults. |
| Cache capacity | Release DashMap iterator guards before removal in dashboard aggregates, spend windows, last-used retries, and custom-proxy clients. | Insertion at capacity must not acquire a write lock while retaining its own shard read lock. Preserve clients held by active requests. |
| Routing regression fixtures | Increment the registry generation after direct SQL corruption or repair. | Exercise decoding through a fresh snapshot. Preserve the installed snapshot on consecutive failed rebuilds and recover after repair. |
| Provider reorder | Increment the registry generation after committing new priorities. | Refresh order and priorities in every routing store in the same process on its next read. |
| Migrated ingress | Select the existing Caddy service, configuration file, and loopback admin origin through explicit settings. | Preserve UID, static/live route, and routing-unit checks without restarting or reloading ingress. |

The frontend dependency audit decreased from 112 advisory entries to zero.
The documentation dependency audit decreased from 34 entries to zero.
The SDK helper audit decreased from one entry to zero. Apeiron web also reported zero.
Use compatible dependency versions; retain the verified ESLint rule version and SWC binding override.
Update the documentation `llms.txt` handler for the patched Fumadocs asynchronous API.
The candidate CI rebuilt and retested the frontend and documentation with these lockfiles.

Removing the database password from current files does not remove it from Git history.
This self-check has not rewritten Git history or performed another database-password rotation.
The host contains credential-rotation evidence from earlier work.
This report does not claim that the historical password remains valid or that credentials were never rotated.
Confirm that earlier evidence covers the exposed credential before closing credential recovery.
Historical Git copies still contain the removed value.

## Validation known

Record each result against the source and dependencies that the check actually used.
The following table records revision-specific validation and its limits.

The final candidate is `09c47423502ff64bae50a0d02ae5781e0aeddcd6` on `Monoize-Claude`.
GitHub Actions run `36994966932` verifies this candidate.
All seven jobs passed, including the release job.
The frontend CI confirms 342 unit tests, 28 browser cases, nine npm launcher tests,
ESLint, TypeScript, and the production build. The docs CI exported 706 pages.
The deployment CI confirms 47 Python operational tests, four release-package tests,
Bash drain tests, and connection-preserving routing tests in an isolated network namespace.
The rehearsal CI passed 148 tests, including disposable PostgreSQL checks.
Apeiron's frontend build and two Go worker tests passed. Its Rust server compiled, but defines zero tests.
The backend job compiled every test target and passed the complete ordinary suite, all six PostgreSQL regressions, and 14 SQL-construction tests.
The ordinary suite passed 1983 tests. The six PostgreSQL suites passed one test each.
Together with 14 SQL-construction tests, these account for 2003 distinct passing tests.
The request-log schema preparation repeats the migration test and does not add coverage.
The two unexecuted tests are `benchmark_image_formats` and `gateway_benchmark_ramp`.
The organization aggregate test is ignored in the broad run but passed in its dedicated PostgreSQL step.
The release artifact passed isolated Docker readiness, embedded-resource, and graceful-exit checks.
The downloaded archive checksum matches the successful upload step.

The baseline Rust suite exposed a capacity-eviction deadlock, a Provider reorder cache-invalidation defect,
and a direct-SQL corruption fixture that omitted cache invalidation.
An iterator retained its DashMap shard read lock while removal requested that shard's write lock.
Source review found four instances of this pattern. Each fix has a bounded capacity regression test.
Run `36985587061` at `b9c50e08` passed all four capacity tests without hanging.
Its library run passed 1128 tests, failed one, ignored one, and filtered 16 PostgreSQL tests.
The remaining failure exposed a missing cache invalidation after Provider reorder.
The final fix invalidates after commit and checks a second routing store's independent snapshot.
The corruption fixture also invalidates explicitly and verifies failed-rebuild recovery.
The following run then exposed a workflow fixture defect: the request-log database had
no schema before a query that joins Provider tables. The workflow now applies the full
migration chain before the request-log query and supplies its four missing temporary columns.
The organization aggregate regression retains its empty-database guard and applies migrations itself.
Run `36994966932` includes these final changes. All six verification jobs and its release job passed.

| Check | Known result | Limit |
| --- | --- | --- |
| Python deployment and operational tests | 47 passed locally and in candidate CI. | Includes Caddy configuration validation and mocked deployment failure paths. It does not prove a production swap. |
| Release-package tests | 4 passed. | Packaging tests do not establish that the new release binary has been built. |
| Deployment CI at `09c47423` | Python 47, package 4, Bash drain checks, and isolated Linux network-routing tests passed. | Verify deployment commands separately on the target server. The routing test preserves existing connections through forward and reverse swaps. |
| Rehearsal CI at `09c47423` | 148 tests passed across 30 test-result groups; zero failed or ignored. | Includes a disposable PostgreSQL database. Short benchmark profiles do not establish production capacity. |
| Apeiron CI at `09c47423` | Frontend build, Rust server compilation, and both Go worker tests passed. | The Rust server currently contains zero tests. No live video or payment provider was invoked. |
| npm launcher tests at `09c47423` | 9 passed. | Re-run if the launcher or its dependencies change. |
| Frontend unit tests at `09c47423` | 342 passed after adding two rollback tests and applying dependency patches. | Re-run if further code or dependency changes affect this result. |
| Frontend browser fixtures at `09c47423` | 28 tests passed with the patched dependencies. | These fixtures use synthetic loopback data. They do not establish acceptance of a deployed revision. |
| Frontend build and lint at `09c47423` | Production build, both TypeScript configurations, and ESLint passed with the patched dependencies. | Bundle size remains approximately 4 MB before compression. |
| Documentation build at `09c47423` | 706 pages exported with the patched dependencies. | Four locales and the generated `llms.txt` were checked. The existing metadataBase warning remains. |
| SDK and mock helpers | Frozen SDK installation, TypeScript checks, and SDK help invocation passed. Mock TypeScript checks passed. | No live paid inference requests were made. |
| Independent HTTPS probe test | A local self-signed certificate was rejected with and without an `insecure` curl configuration after the fix. | This verifies certificate checking and curl configuration isolation. It does not exercise production routing. |
| Upstream adaptation whitespace check | `git diff --check` passed for the changed protocol, transform, specification, and documentation files. | This is not a Rust compilation result. |
| Baseline CI | Commit `6e50eeda` compiled all backend targets. Its test run exposed a cache deadlock, a Provider reorder invalidation defect, and a corruption fixture missing invalidation. | These failures triggered the additional fixes above. The baseline cannot validate the final candidate. |
| Final Rust and PostgreSQL verification at `09c47423` | 2003 distinct tests passed: 1983 ordinary, 14 SQL-construction, and six PostgreSQL tests. | Two performance benchmarks remain unexecuted. The repeated migration preparation is excluded from this count. |
| Release artifact and container smoke checks at `09c47423` | Artifact `11222972234` passed checksum, isolated Docker startup, readiness, embedded-resource, and graceful-exit checks. | The smoke check uses temporary SQLite storage. Verify the deployed PostgreSQL instance separately. |
| Production deployment to `40.160.141.21` | Blue-green swap and final acceptance passed at `2026-10-02T11:26:19Z`. | Six cutover availability probes passed. This is sampled availability evidence, not a record of every user response. |
| JavaScript dependency audits | Frontend, docs, SDK, and Apeiron web each reported zero known advisories. | This result reflects the registry advisory data available during this check. |
| Rust dependency audits | Patched available vulnerabilities and removed the unused PDF parser dependency. The RSA advisory has no patched release. | Record enabled feature paths and unresolved advisories in `DEPENDENCY-REVIEW-2026-10-02.md`. Final locked Rust verification passed in run `36994966932`. |

The candidate CI results belong to [workflow run 36994966932](https://github.com/Libra1337/monoizeovo/actions/runs/36994966932).
The job snapshot is retained in `local-test/audit/jobs-09c47423.json`.
Earlier logs under `local-test/audit/ci-a65171bb-*.log` describe the earlier candidate only.

Keep the Linux socket-routing test inside an isolated network namespace.
Keep PostgreSQL regressions on disposable databases with test credentials.
Do not report production capacity, paid inference, payment processing, or long-stream acceptance from these local checks.

## Deployment acceptance

Deploy revision `09c47423502ff64bae50a0d02ae5781e0aeddcd6` to `40.160.141.21`.
The project entrypoint `/opt/monoize/blue-green-swap.sh 09c47423` completed successfully at `2026-10-02T11:25:10Z`.
Final application acceptance passed at `2026-10-02T11:26:19Z` (19:26 UTC+8).
The subsequent runtime check confirmed one healthy serving container named `monoize`.

| Release evidence | Value |
| --- | --- |
| Source | `09c47423502ff64bae50a0d02ae5781e0aeddcd6` on `Monoize-Claude` |
| Workflow | [36994966932](https://github.com/Libra1337/monoizeovo/actions/runs/36994966932), all seven jobs passed |
| Artifact | `11222972234`, 19,952,578 bytes |
| Artifact ZIP SHA-256 | `d5696c2735c863e2ed6f47e6f55f703d482c136f8fd80e722c1ae39d68cb33c4` |
| Running executable SHA-256 | `356995ff48913004c29f86f8ee884cc618f41b42d3893ed210b10aca9fbdd26b` |
| Target image | `monoize:09c47423` |
| Target image ID | `sha256:5cc8b731122cfc666783c417c308289fd673c78de99891dd7f44621e81947128` |
| Migration tree SHA-256 | `ad6b14f1805fb53501543608547b1577c85378d5032932668d97f7c1dbccd97b` |

Use the target-specific ingress settings recorded in `DEPLOYMENT-READINESS-2026-10-02.md`.
The serving ingress remains `migration-ingress.service`; its PID remains `76263`.
Its configuration checksum is unchanged. No Caddy reload occurred.
Persisted and live routing both select stable port `8080`, active port `8081`, and ingress UID `999`.
The candidate uses an independent persistent request-log spool and application UID/GID `1000:1000`.

The route changed at `11:12:48Z`. Twelve accepted old connections then drained naturally.
The final connection continued sending data until it ended; no deadline terminated it.
The swap completed 12 minutes and 22 seconds after the route change.
Store forwarding paused only after the old connections reached zero.
The lease owner changed, and the new instance reports `mode = local` and `lease_owned = true`.
The old instance exited with code `0` and remains as `monoize-before-09c47423-1790940310731728907`.
Its spool remains available. No `monoize-next` or `monoize-prev` remains.

The cutover probe recorded six samples and zero failures.
Direct TLS checks targeted `40.160.141.21` through `www.lynshen.org` and `api.lynshen.org`.
Both public `/readyz` responses reported PostgreSQL, reachable database, Primary role, and ready status.
The embedded frontend module hash matched the release artifact locally and through both public domains.
The running executable checksum matched the release executable.

After lease handover, the new Primary passed 14 authenticated dashboard GET checks.
The current organization's member-usage GET also returned `200` with the expected arrays.
The checks covered Store catalog, entitlement, orders, administration, revenue, billing, metadata, firewall, and announcements.
Temporary diagnostic sessions were deleted in `finally` blocks, and follow-up queries confirmed their removal.
No paid inference, payment, refund, or withdrawal was invoked.

The pre-swap PostgreSQL backup is:
`/opt/monoize/backups/pg-09c47423-1790939538048152693/database.dump`.
Its SHA-256 is `d29e4538e2a18a89856fd0c3926e054167d5bc31ded50be825dcfc2901b27615`.
The swap verified the custom-format archive with `pg_restore --list`; acceptance rechecked its checksum.
This procedure did not perform a full database restore.
Previous deployment scripts remain under `/opt/monoize/backups/scripts-09c47423-1790939523573313249`.

Retain local evidence in `local-test/audit/acceptance-09c47423.json`, `final-runtime-09c47423.json`,
`deployment-log-09c47423.txt`, `release-09c.log`, and `jobs-09c47423.json`.
Retain server evidence in `/opt/monoize/build-09c47423/swap.log` and the stated backup directory.
The source revision above identifies the deployed binary. Later report-only commits do not change that binary.

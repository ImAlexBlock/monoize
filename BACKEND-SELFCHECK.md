# Backend Self-Check

Status: active. Do not infer page acceptance from health checks.

## Initial Public GET Matrix

Fifty authenticated fixed-route GET checks completed through the CDN.
The temporary diagnostic session was removed afterward.

Confirmed HTTP 500:
- Billing profile summaries: PostgreSQL INT4 flag decoded as i64.
- Firewall stats and events: historical INTEGER timestamp decoded as i64.
- User announcements: PostgreSQL BOOL read-state decoded as i32.

The metadata endpoint returned 1882 records and billing rates returned 5991.
The profile error prevents model selection; the workbench presented it as no models.
Never run metadata synchronization to conceal this read failure.

Firewall inspection found 2905 wrapped negative millisecond values.
The original RFC3339 `created_at` remains present for all inspected records.
The SQLite-to-PG importer used unchecked integer narrowing.
Repair requires a tested migration, a pre-change backup, and row-preservation verification.

## Acceptance Gates

- [x] Create a read-only initial interface matrix; preserve status and counts, not secrets.
- [x] Confirm previous deployment drained and handed over its lease.
- [x] Fix and test profile and announcement projections on real PostgreSQL.
- [x] Test firewall timestamp widening and reconstruction; preserve IDs and payloads.
- [x] Reject overflowing integer imports instead of wrapping.
- [x] Show retryable workbench request errors instead of a false empty state.
- [x] Verify candidate interfaces on current data before admitting traffic.
- [x] Re-run fixed and parameterized interfaces with appropriate existing roles.
- [x] Verify visible model selection and automatic error recovery in a browser.
- [x] Check PG and SQLite regressions and role-protected 403 cases separately.
- [ ] Deploy without aborting existing streams; verify live pages through the CDN.

Do not create orders, payments, withdrawals, refunds, or alter balances for diagnostics.
An admin rejected by a super-admin or sales-agent endpoint is not automatically a defect.
Do not mark this audit complete while any reproduced 500 or false-empty page remains.

## First Repair Verification

The expanded real-PG regression and existing revenue/Store tests passed.
Frontend error-state source tests and TypeScript checks passed.
The importer checks reject integer overflow, fractional REAL values, and nonfinite values.
The archived SQLite scan checked 83 narrow-integer mappings.
Only the firewall millisecond column contained out-of-range source values.

After the migration, all 2905 original firewall IDs remained present.
Non-timestamp row hashes matched; all reconstructed milliseconds matched the original timestamps.
The target column is BIGINT.

Image `monoize:20261001-backend-selfcheck` passed fourteen candidate GET gates.
Public matrix `dashboard-selfcheck-5ea635c0.json` reports 46 HTTP 200 and four
expected role-based 403 responses, with no HTTP 500.
Profile summaries return 159 profiles. Metadata returns 1882 records.
Real-browser verification shows the model workbench populated with the openai profile
and its 48 models; the profile dropdown also populates.
The dropdown selection attempt was blocked by an overlapping inner element in automation;
profile-switch acceptance is not yet claimed.

The old instance still had one accepted connection at the last deployment check.
The supervised swap retains both healthy instances and waits without force-stopping streams.
Parameterized routes, fuller browser navigation, and error/retry interaction remain to be checked.

## Extended Read Acceptance

Sixteen parameterized GET checks passed through the CDN, including profile filtering,
model/provider details, historical revenue, Excel export, log pagination, and organization
detail, ledger, analytics, keys, limits, and member usage.
The export returned the spreadsheet content type and a ZIP signature; workbook contents
were not independently rendered during this check.
All temporary interface diagnostic sessions were deleted.

Browser checks confirmed keyboard Profile selection changes the model and price list.
Blocking only the profile-summary request in the isolated browser produced an error
heading and Retry button, not a no-models state.
Removing the interception restored the data automatically before a manual retry click.
Manual retry completion is therefore not independently established.
Firewall default seven-day filtering was empty; selecting all time displayed twenty
rows on the visible page without alerts. No event content was exported.

## Additional Data-Accuracy Risk

The organization member-usage implementation silently converts PostgreSQL token
aggregate decode errors to zero and an integer member-flag decode error to false.
HTTP 200 on a live dataset with no matching recent rows does not disprove this defect.
A scoped fix and SQLite/PostgreSQL tests with nonzero large token values are in progress.
The PostgreSQL fixture also checks active versus removed membership.
These changes are not deployed or accepted until the running test job passes.

## Follow-Up Verification

The previous blue-green swap completed normal drain and Store lease handover.
The original instance exited with code 0 and remains retained; no stream was force-stopped.
Role-specific checks after deployment passed for existing super-admin, sales-agent,
and regular-user accounts. A regular user still receives 403 for admin revenue.
All temporary diagnostic sessions were removed.

A browser read-through visited sixteen dashboard pages and recorded their headings,
alert elements, and displayed row counts without submitting mutations or exporting content.
The model, Store, revenue, users, groups, provider, wallet, orders, settings, announcement,
sales, organization-management, usage, and firewall surfaces rendered their expected headings.
This is read-path evidence, not acceptance of untested financial write operations.

The initial organization unit-test build and the subsequent smaller integration build
were killed by the build container's 6 GiB memory limit, confirmed in kernel OOM logs.
Neither result is a test pass. An isolated, single-job 10 GiB retry is running.
Production memory limits were not changed. The organization display fix is not yet deployed.

## Organization Regression Result

The 10 GiB isolated retry completed with exit code 0 and no OOM flag.
Both `sqlite_member_usage_endpoint_preserves_totals` and
`postgres_member_usage_endpoint_preserves_totals` passed.
The integration fixture exercises the authenticated owner endpoint, input tokens
above i32 range, NULL usage, excluded failed-request tokens, and removed-member history.
No production organization data was written by these tests.
The candidate image build is separate; test success alone does not establish deployment.

The retry button was also clicked while only profile GET requests were blocked in
the isolated browser. No dashboard POST was observed. Removing the interception
restored the model list automatically; manual post-recovery click remains unconfirmed.
The isolated browser was closed and its diagnostic session removed.

## Organization Candidate Acceptance

Image `monoize:20261001-org-usage` was built after both database integration tests passed.
The candidate passed the fourteen existing dashboard GET gates and an authenticated
organization-owner member-usage check before switching new upstream connections.
There is no schema or production-data mutation in this organization display fix.

An initial public comparison disagreed on active/removed classification while old
connections were draining. Do not discard that observation.
A subsequent independent public comparison matched all five existing organization
member rows, including nonzero input/output/cache totals and membership classification.
Old connection reuse is a possible explanation for the earlier mismatch, not a proven cause.
Repeat the comparison after the old instance exits before final acceptance.
The latest deployment check still showed three accepted old connections.
Both instances remained healthy; no forced stop or Caddy reload occurred.

## Current Acceptance Status

This section supersedes the historical build and deployment progress above.
The audit remains active.

At 2026-10-02 00:40 UTC+8, the organization swap supervisor remained running.
New upstream connections route to the healthy organization-fix candidate.
Two accepted connections remained on the previous instance.
One connection continued sending data; do not terminate either connection for acceptance.
The final Store lease handover and old-instance exit are not yet verified.

The latest fixed-route report, `dashboard-selfcheck-76bdea42.json`, contains
46 HTTP 200 responses and four expected role-based HTTP 403 responses.
Its diagnostic session cleanup marker is present.
A further public organization comparison matched all five member records.
Calls, input/output/cache tokens, and active/removed classification matched database queries.
All five records contained nonzero input tokens.
The temporary organization diagnostic session was deleted.

Remaining gates:
- Wait for existing connections to drain without interruption.
- Confirm Store lease ownership and the previous instance's normal exit.
- Repeat public organization comparisons after the previous instance exits.
- Recheck public dashboard reads after the completed handover.

Browser evidence covers read paths and automatic request-error recovery.
Manual retry completion after recovery and financial write operations remain untested.
Do not interpret successful read checks as acceptance of these untested operations.

## User-Management Error Guard

Source inspection found another false-empty path in user management.
The page ignored errors from users, groups, and billing-plan queries.
It substituted empty arrays and kept editing controls available.

The new guard shows a retryable alert before loading, empty results, or editing controls.
Retry revalidates the three read queries without submitting mutations.
The page waits for all three dependencies before it exposes editing controls.

Four source-regression checks passed across the user and model error-state tests.
Both frontend TypeScript configurations passed in the isolated build container.
These checks do not establish browser behavior or production deployment.
The local machine has no Bun executable; the existing build host supplied the test runtime.

- [x] Add the user-management error contract, guard, and source-regression checks.
- [x] Verify the user-management guard with browser request-failure injection.
- [ ] Build and deploy this follow-up after the current stream drain completes.

The local browser fixture now renders the real user-management page with synthetic data.
It injects HTTP 500 separately into users, groups, and billing-plan GET requests.
All three cases show an alert and hide the add-user control.
With automatic error retries disabled, a manual Retry click restores the populated user list.
Each retry revalidates all three dependencies. No write requests or page exceptions occurred.
The fixture blocks requests outside its loopback origin and uses no production sessions.

The browser test is `frontend/tests/users-load-errors.browser.ts`.
Run it with Bun; set `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH` when using an existing Chromium installation.
The local run used a temporary npm-cached Bun runtime and frozen-lockfile dependencies.
Twenty-three related source and API tests passed across four test files.
This evidence does not replace the pending production deployment and public-path checks.

## Groups and Providers Error Guards

Source inspection found that Groups ignored its read error.
Providers handled its primary request error but ignored dependency errors.
A failed Groups request therefore filtered valid Providers into an empty list.
Failed settings, transform registry, and metadata requests also left editing controls available.

Groups now shows an alert and read-only retry when its query fails.
Providers now guards all five page dependencies before exposing editing controls.
Both pages keep their existing data and mutation contracts.

The browser fixture now covers nine failure cases across Users, Groups, and Providers.
Every case restores a populated list after a manual retry.
All nine cases passed without write requests, unexpected API paths, or page exceptions.
The related 23 source/API tests and both TypeScript checks passed again.
The frontend release build passed; Vite reported a large main chunk warning.
No dependency or lockfile changes were required.

These guards remain undeployed.
The release binary embeds frontend resources, so deployment requires a new binary and image.
At the latest production inspection, one old connection remained and the swap supervisor was alive.
The previously transmitting connection had closed naturally; no connection was force-stopped.

## Follow-Up Image Ready

The isolated release build completed with exit code 0 after 13 minutes 10 seconds.
An SSH observation timeout interrupted the outer packaging workflow, not the container compilation.
The retained build container confirmed successful compilation before packaging resumed separately.
Image `monoize:20261002-dashboard-guards` was built and loaded on the target host.
Source, local-transfer, and target archives have the same SHA-256:
`2cb5174cec4ba01b70d3543301b69dbb97d75321db29d87290f8677b1871884f`.

The target image ID is
`sha256:3a140e3d9004eb461dece0e624fc8921e62cd57ac990062f87b016ff81ff3497`.
Loading an image does not deploy it or switch traffic.

At 2026-10-02 01:47 UTC+8, the existing organization swap supervisor remained alive.
Both overlapping instances were healthy, and one accepted old connection remained.
No second swap was started. Final lease handover, normal old-instance exit,
follow-up deployment, and post-deployment public-path acceptance remain pending.

## Public Reads During Drain

Report `dashboard-selfcheck-6a14d7f9.json` began at 2026-10-02 01:53:49 UTC+8.
All 50 public GET checks completed: 46 HTTP 200 and four expected HTTP 403 responses.
The diagnostic session cleanup marker is present.
Both overlapping instances remain healthy and the swap supervisor is still running.

Socket ownership identifies the remaining accepted old connection as Caddy-to-Monoize.
Transport counters show approximately three hours without application-data transfer.
This does not prove that the connection is safe to terminate.
No request payloads were inspected, no connections were terminated, and no second swap began.
The follow-up image remains loaded but undeployed.

## Wallet Summary Error Guard

Wallet summary code treated unavailable monthly usage as zero and unavailable entitlement as no plan.
The new summary guard shows a skeleton while reads are pending and a retryable alert on failure.
Retry revalidates usage, entitlement, and exchange rate without submitting redemption.
The separate ledger and redemption behavior remain unchanged.
An absent balance value now renders an unavailable marker instead of an invented zero.

The browser fixture now covers twelve dependency-failure cases.
All twelve passed, including wallet usage, entitlement, and exchange-rate failures.
Each wallet case recovered its nonzero usage and plan after manual retry without write requests.
Nineteen related money-format and error-state regression tests passed.
Both frontend TypeScript configurations passed.

The wallet change is not deployed and is not part of `20261002-dashboard-guards`.
That existing image contains only the previously built Users, Groups, and Providers guards.
Additional source inspection identified unhandled read errors in settings and request logs.
Those paths still require focused reproduction and repair before full acceptance.

## Settings and Request-Log Error Guards

Settings now shows a retryable alert on settings or transform-registry read failure.
Save controls remain hidden until those dependencies recover.
Retry preserves unsaved local settings and submits no mutation.
The existing Provider model-selector error handling remains unchanged.

Request logs now exposes query errors with a retry action.
A failed initial read does not render the no-records table.
Previously loaded rows remain visible if a subsequent refresh fails.
Retry retains the current query, filters, and pagination.

The real-component browser fixture passed fifteen dependency-failure cases.
Settings cases also edit a local draft, inject a subsequent failure, and verify draft preservation after retry.
The log case verifies nonempty rows remain visible during a subsequent refresh failure and recovery.
All cases reject unexpected API paths and write requests.
No production sessions or financial mutations are used by this fixture.

Seventy-three related regression tests passed across five files.
Both TypeScript configurations and the full frontend release build passed.
The existing large-chunk and Browserslist-age warnings remain.

Wallet, settings, and request-log changes still require a new release binary and image.
They are not included in the already loaded `20261002-dashboard-guards` image.
The previous organization swap still has one accepted old connection; its supervisor remains alive.
Final deployment and public-path acceptance remain incomplete.

## Unified Recovery Build

The complete frontend test directory passed: 334 tests across 34 files, with zero failures.
This is separate from the fifteen browser failure-injection scenarios.

The unified source archive comes from committed revision `b48f428`.
It includes the Users, Groups, Providers, Wallet, Settings, and Request Logs guards.
Untracked screenshots and installed local dependencies are excluded from the Git archive.

The detached build targets image `monoize:20261002-dashboard-read-recovery`.
The first process inspection confirmed live `cargo` and `rustc` processes in the bounded build container.
The OOM flag was false. The image-packaging completion marker was absent.
A running container's exit-code field is not evidence of successful completion.
Use `scripts/inspect-dashboard-read-recovery-build.sh` to inspect this existing build before any restart.

The new image is not yet accepted, transferred, or deployed.
The older loaded dashboard-guards image remains incomplete for the full repaired frontend.

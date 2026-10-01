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
- [ ] Verify the user-management guard with browser request-failure injection.
- [ ] Build and deploy this follow-up after the current stream drain completes.

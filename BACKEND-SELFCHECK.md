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
- [ ] Re-run fixed and parameterized interfaces with appropriate existing roles.
- [ ] Verify visible model selection and error recovery in a browser.
- [ ] Check PG and SQLite regressions and role-protected 403 cases separately.
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

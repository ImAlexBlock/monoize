# Admin Daily Revenue Report Specification

## 1. Scope

This specification defines the admin-only daily revenue report: the backend
endpoints under `/api/dashboard/admin/revenue/*`, the frontend page
`/dashboard/admin/revenue`, the persisted daily aggregates, the user exclusion
list, the daily settlement background task, and the Excel export.

## 2. Definitions

- A **day** is one Asia/Shanghai local calendar day `00:00:00` through
  `24:00:00` (exclusive end). Asia/Shanghai has no DST, so the offset from UTC
  is exactly `+08:00` at every instant.
- A **day id** is the string `YYYY-MM-DD` of the Asia/Shanghai local date.
- **Revenue** of a day is the SUM of canonical `request_logs.charge_nano_usd`
  over request-log rows whose `created_at` falls in that day, excluding rows
  whose `request_kind` equals `active_probe_connectivity`, excluding rows
  whose `user_id` is in the exclusion list, and including only rows with a
  non-null canonical `charge_nano_usd`. Top-up and store order money is NOT
  revenue.
- **Log retention limit** is the `request_logs` retention window defined by
  `request-logs.spec.md` RL-S9 (365 days).

## 3. Authorization

AR-1. Every `/api/dashboard/admin/revenue/*` endpoint MUST require an
authenticated dashboard admin session (`session_helpers::require_admin`).
Non-admin requests MUST return HTTP 403.

AR-2. The frontend route `/dashboard/admin/revenue` MUST render an
unauthorized state for non-admin sessions, MUST NOT call any admin revenue
endpoint for those sessions, and the navigation entry MUST render only for
admin sessions.

## 4. Daily aggregates

AR-3. The system MUST persist per-day aggregates in three tables:

- `admin_revenue_daily_summaries`: `id` TEXT PRIMARY KEY, `day` TEXT UNIQUE
  (day id), `total_charge_nano_usd` TEXT (canonical decimal), `total_calls`
  INTEGER, `total_input_tokens` BIGINT, `total_output_tokens` BIGINT,
  `computed_at` TEXT (RFC 3339).
- `admin_revenue_daily_model_rows`: `id` TEXT PRIMARY KEY, `day` TEXT,
  `model` TEXT, `charge_nano_usd` TEXT (canonical decimal), `calls` INTEGER,
  `input_tokens` BIGINT, `output_tokens` BIGINT, with
  UNIQUE(`day`, `model`).
- `admin_revenue_daily_user_rows`: `id` TEXT PRIMARY KEY, `day` TEXT,
  `user_id` TEXT, `username` TEXT (snapshot at computation time, nullable),
  `charge_nano_usd` TEXT (canonical decimal), `calls` INTEGER,
  `input_tokens` BIGINT, `output_tokens` BIGINT, with
  UNIQUE(`day`, `user_id`). The `username` snapshot is refreshed from the
  `users` table on every recomputation of that day and is NULL when the user
  row no longer exists.
- `admin_revenue_daily_user_model_rows`: `id` TEXT PRIMARY KEY, `day` TEXT,
  `user_id` TEXT, `model` TEXT, `charge_nano_usd` TEXT (canonical decimal),
  `calls` INTEGER, `input_tokens` BIGINT, `output_tokens` BIGINT, with
  UNIQUE(`day`, `user_id`, `model`).

AR-4. A settled day row MUST be recomputable: given the same `request_logs`
content, recomputation MUST produce byte-identical aggregate values.
Recomputation for a day MUST delete that day's existing summary row, model
rows, user rows, and user-model rows and insert the recomputed rows in one
transaction.

AR-5. A day MUST be settled only when the day has fully elapsed (the current
Asia/Shanghai instant is at or past the day's end). The current day MUST NOT
be persisted; it MUST be aggregated live from `request_logs` on every read.

AR-6. The settlement task MUST run at least once per Asia/Shanghai day after
00:05 local time and settle the previous day. On process start, the task MUST
settle every unsetted day that is fully elapsed and not older than the log
retention limit. A settlement failure MUST be logged and retried on the next
task tick.

## 5. Exclusion list

AR-7. The system MUST persist `admin_revenue_exclusions`: `user_id` TEXT
PRIMARY KEY, `username` TEXT (snapshot at insertion time), `created_at` TEXT
(RFC 3339). The username snapshot MUST NOT be updated when the user renames.

AR-8. Adding or removing an exclusion MUST trigger recomputation of every
persisted day not older than the log retention limit, immediately after the
list change commits. Days older than the log retention limit keep their
existing persisted values because their source rows are deleted.

AR-9. Excluded consumption MUST still be billed to the user and MUST still
appear in every non-revenue surface (usage ranking, analytics, request logs).
The exclusion applies ONLY to admin revenue aggregates.

## 6. Endpoints

AR-10. `GET /api/dashboard/admin/revenue/daily?from=&to=` MUST return, for the
inclusive day-id range `from`..`to` (validated `YYYY-MM-DD`, `from <= to`,
range length at most 366 days; omitted `from` defaults to the current day,
omitted `to` defaults to the current day):

- `from`: string day id actually used;
- `to`: string day id actually used;
- `days`: array ordered by day id descending. Persisted days come from the
  persisted tables; the current day, when in range, is computed live with the
  same predicates as persistence. Each day object:
  - `day`: string day id;
  - `total_charge_nano_usd`: canonical decimal string;
  - `total_calls`: integer;
  - `total_input_tokens`: integer;
  - `total_output_tokens`: integer;
  - `models`: array ordered by `charge_nano_usd` descending, then `model`
    ascending in UTF-8 byte order; each item `model`, `charge_nano_usd`
    (string), `calls` (integer), `input_tokens` (integer),
    `output_tokens` (integer).
  - `users`: array ordered by `charge_nano_usd` descending, then `user_id`
    ascending in UTF-8 byte order; each item `user_id`, `username` (string or
    null), `charge_nano_usd` (string), `calls` (integer), `input_tokens`
    (integer), `output_tokens` (integer), and `models`: array of that user's
    per-model rows for the day, ordered by `charge_nano_usd` descending then
    `model` ascending in UTF-8 byte order; each item `model`,
    `charge_nano_usd` (string), `calls` (integer), `input_tokens` (integer),
    `output_tokens` (integer). The per-user model rows MUST sum (charge,
    calls, tokens) exactly to that user's row.
  A day with no matching rows MUST be absent from `days` (zero-revenue days
  are omitted, both persisted and live).

AR-11. `GET /api/dashboard/admin/revenue/exclusions` MUST return
`exclusions`: array ordered by `created_at` ascending, each item `user_id`,
`username`, `created_at`.

AR-12. `POST /api/dashboard/admin/revenue/exclusions` with JSON body
`{"user_id": string}` MUST add the user to the list. The `user_id` MUST match
an existing `users.id`; otherwise the endpoint MUST return HTTP 404 with code
`user_not_found`. Adding an already-excluded user MUST be a no-op returning
HTTP 200. The endpoint MUST trigger recomputation per AR-8 and MUST NOT
return before recomputation finishes.

AR-13. `DELETE /api/dashboard/admin/revenue/exclusions/{user_id}` MUST remove
the user from the list. Removing a user not in the list MUST return HTTP 404
with code `not_found`. The endpoint MUST trigger recomputation per AR-8 and
MUST NOT return before recomputation finishes.

AR-14. Amounts MUST be aggregated as exact integers (i128) and serialized as
canonical decimal strings. The endpoint MUST NOT use binary floating point.

AR-14a. Token aggregate projections MUST decode as signed 64-bit integers on
SQLite and PostgreSQL. PostgreSQL SUM over BIGINT MUST be explicitly cast to
BIGINT before decoding. A sum outside that range MUST fail rather than wrap
or silently replace the aggregate with zero. NULL token inputs count as zero.
Persisted call-count projections MUST also return BIGINT when historical columns
use PostgreSQL INTEGER, without changing the stored data or monetary arithmetic.

## 7. Excel export

AR-15. `GET /api/dashboard/admin/revenue/daily/export?from=&to=` MUST return
the same rows as AR-10 as an Excel workbook download:

- HTTP 200 with `Content-Type`
  `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet` and a
  `Content-Disposition: attachment; filename="monoize-revenue-{from}-{to}.xlsx"`.
- One worksheet named `Revenue`. Header row: Day, Revenue (USD), Calls,
  Input Tokens, Output Tokens, Top Model, Top Model Revenue (USD). One data
  row per day, ordered by day ascending. `Top Model` is the first entry of
  that day's `models` array, or empty when `models` is empty. Revenue cells
  are numeric USD values with 6 fractional digits computed from the exact
  nano-USD integer via decimal arithmetic, not binary floating point.
- A second worksheet named `Users` with one row per (day, user) pair, ordered
  by day ascending then revenue descending. Header row: Day, User ID,
  Username, Revenue (USD), Calls, Input Tokens, Output Tokens. A NULL
  username renders as an empty cell. The pair set and ordering MUST equal
  the `users` arrays of AR-10 for the same range.
- A third worksheet named `User Models` with one row per
  (day, user, model) triple, ordered by day ascending then user revenue
  descending then model revenue descending. Header row: Day, User ID,
  Username, Model, Revenue (USD), Calls, Input Tokens, Output Tokens. The
  triple set MUST equal the per-user `models` arrays of AR-10 for the same
  range.
- The same `from`/`to` validation as AR-10 applies; an invalid range MUST
  return HTTP 400 with code `invalid_request`.

## 8. Frontend

AR-16. `/dashboard/admin/revenue` MUST contain:

- a day-range control with `from` and `to` day inputs, defaulting both to
  the current day;
- a flat spreadsheet-style table with one row per (day, user) pair, flattened
  from the per-day `users` arrays of AR-10. Columns: Day, Username (user id
  below it), Revenue, Calls, Input Tokens, Output Tokens, Model Count. Rows
  MUST be sortable by clicking the Day and Revenue column headers,
  defaulting to day descending then revenue descending. The table MUST
  render the full result of the selected range without pagination. Each row
  MUST be expandable to reveal that user's per-model rows for that day
  (model, revenue, calls, input tokens, output tokens), ordered by revenue
  descending;
- a summary strip above the table showing the range totals: revenue, calls,
  and distinct consuming users;
- an exclusion management area rendered as a compact header bar with the
  current exclusion count and a button that opens a dialog containing the
  exclusion list (with remove actions) and the user search box that adds a
  selected user. The bar MUST NOT expand the page layout when the list is
  long; only the dialog scrolls.

AR-17. Data fetching MUST use SWR with a 10-second refresh interval for the
flat table. Loading MUST render a shape-matched skeleton. A failed request
MUST render an inline error state with a retry action. Exclusion add/remove
MUST optimistically update the list and revalidate the table after the
mutation settles.

AR-18. The page MUST provide an Export button that downloads the Excel file
for the currently selected `from`..`to` range via the AR-15 endpoint. While
the download is in flight the button MUST show a busy state and MUST NOT be
re-triggerable.

AR-19. Revenue MUST be formatted with the existing Coin/currency formatting
utilities (`formatCoinFromNanoUsdForCurrency` and the store exchange-rate
hook), consistent with other admin money surfaces. Exact values MUST be
parsed with `BigInt`, never `Number`.

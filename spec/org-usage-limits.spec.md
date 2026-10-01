# Organization Usage Limits and Member Analytics Specification

## 0. Status

- Purpose: spend limits for organization spaces at three levels (space, member, key) with
  total / hourly / daily windows, plus a per-member usage analysis inside a space.
- Scope: `orgs` limit columns, `org_members` limit columns, `api_keys` limit columns,
  settlement-time enforcement, `GET /api/dashboard/orgs/{org_id}/member-usage`,
  `PUT /api/dashboard/orgs/{org_id}/limits`, `PUT /api/dashboard/orgs/{org_id}/keys/{key_id}/limits`.
- Related: `orgs.spec.md` (spaces), `metered-billing.spec.md` (settlement),
  `dashboard-usage-analysis.spec.md` (response shapes).

## 1. Data model

ORGL-1. `orgs` gains four nullable limit columns. NULL means unlimited. Each value is a
non-negative integer amount in nano-USD:

- `spend_limit_total_nano_usd` — lifetime spend of the whole space, measured from the
  durable `request_logs` attribution (`user_id = org_id`);
- `spend_limit_hourly_nano_usd` — rolling window of the last 3600 seconds;
- `spend_limit_daily_nano_usd` — calendar-day window by UTC day;
- `spend_limit_daily_reset_at` — server-side bookkeeping (see ORGL-9); NULL.

ORGL-2. `org_members` gains the same three nullable limit columns
(`spend_limit_total_nano_usd`, `spend_limit_hourly_nano_usd`,
`spend_limit_daily_nano_usd`). A member limit constrains the spend attributed to that
member inside this space: request-log rows with `user_id = org_id` and
`api_key_id IN (keys whose org_id = this org AND created_by = this member)`.

ORGL-3. `api_keys` carries the same three nullable limit columns
(`spend_limit_total_nano_usd`, `spend_limit_hourly_nano_usd`,
`spend_limit_daily_nano_usd`) for every key. A key limit constrains
request-log rows with `api_key_id = this key`. For org keys the key level
sits under the space and member levels (ORGL-4); for personal keys
(`org_id IS NULL`) the key level is the only level that applies.

ORGL-4. All three levels evaluate independently. A request is admitted only when every
configured limit at every applicable level passes. Example: a key with a 1 USD hourly
limit inside a member with a 2 USD hourly limit inside a space with a 10 USD hourly limit
rejects once any one of the three windows is exhausted.

## 2. Enforcement

ORGL-5. Enforcement happens at settlement time in `maybe_charge_usage_with_output`
(after the charge is computed, before the wallet is debited) and at the admission
preflight (`ensure_balance_before_forward`) for fast rejection. Both points MUST evaluate
the same limit set. A breached limit aborts with HTTP `402 org_spend_limit_reached` and
message naming the breached level (`space` | `member` | `key`) and window
(`total` | `hourly` | `daily`).

ORGL-6. Window spend is the sum of `charge_nano_usd` over the durable attribution:
`total` = every row with the attribution of ORGL-1/2/3; `hourly` = rows with
`created_at >= now - 3600s`; `daily` = rows with `created_at >= start of the current UTC
day`. Failed requests (`status != 'success'`) contribute zero charge and never breach a
limit. Charges are nano-USD integers; comparisons are `>=`.

ORGL-7. The admission preflight uses the same queries. It is advisory-fast (read pool)
and MUST NOT block on the write path; a race between preflight and settlement is closed
by the settlement check, which is authoritative. A request that already streamed a
response body and then breaches at settlement records a normal request-log row with its
charge; the wallet is still debited; subsequent requests are rejected until the window
rolls over. (Fail-open at the tail is deliberate: an upstream token already consumed
cannot be returned.)

ORGL-8. Space- and member-level limit checks run only for org-key traffic
(`api_keys.org_id IS NOT NULL`). Key-level checks run for every key that has
at least one configured window, personal keys included. Personal-key traffic
is otherwise untouched. Sub-account keys follow the existing sub-account
rules first; spend limits apply on top.

ORGL-9. `spend_limit_daily_reset_at` records the UTC midnight the daily window last
rolled over for display purposes ("resets at ..."). It is derived from `created_at` and
requires no writer.

## 3. Management API

ORGL-10. `PUT /api/dashboard/orgs/{org_id}/limits` (owner only) sets the space-level
limits and per-member limits in one call:

```json
{
  "space": {"total_nano_usd": null, "hourly_nano_usd": "5000000000", "daily_nano_usd": null},
  "members": {"{member_user_id}": {"hourly_nano_usd": "1000000000"}}
}
```

Absent keys and `null` mean "leave unchanged" for members and "clear (unlimited)" when
explicitly null inside a provided object. Clearing a limit MUST store SQL NULL, never an
empty string. A stored empty string (written by builds before this rule) MUST read as
NULL (unlimited) everywhere a limit column is parsed. Values MUST be integers >= 0 as
string or number; a negative or non-numeric value returns `400 invalid_request`. The
owner cannot set a limit on the owner's own member row (the owner is not a consumer of
the org wallet through member keys; owner spending is the wallet itself and is governed
by the space limits and the wallet balance).

ORGL-11. `PUT /api/dashboard/orgs/{org_id}/keys/{key_id}/limits` (owner, or the key's
creator) sets the key-level limits with the same body shape
(`{"total_nano_usd": ..., "hourly_nano_usd": ..., "daily_nano_usd": ...}`).

ORGL-12. `GET /api/dashboard/orgs/{org_id}/limits` (owner only) returns the current
limit set with live consumption per limit:

```json
{
  "space": {"total_nano_usd": null, "hourly_nano_usd": "5000000000", "daily_nano_usd": null,
            "spent": {"total_nano_usd": "123456789", "hourly_nano_usd": "420000000",
                      "daily_nano_usd": "980000000"}},
  "members": [{"user_id": "...", "username": "...", "limits": {...}, "spent": {...}}],
  "keys": [{"key_id": "...", "name": "...", "limits": {...}, "spent": {...}}]
}
```

## 4. Member usage analysis

ORGL-13. `GET /api/dashboard/orgs/{org_id}/member-usage?range_hours={1..720}&buckets={1..48}`
(owner only) returns per-member usage inside the space over request-log rows with
`user_id = {org_id}`, grouped by the key's `created_by`:

```json
{
  "range_hours": 24, "buckets": 24,
  "members": [
    {"user_id": "...", "username": "...", "role": "member",
     "total_charge_nano_usd": "...", "calls": 12, "input_tokens": 100, "output_tokens": 200,
     "cache_read_tokens": 50,
     "by_model": [{"model": "m", "charge_nano_usd": "...", "calls": 3, "input_tokens": 1, "output_tokens": 2}],
     "series": [{"bucket_start": "RFC3339", "charge_nano_usd": "...", "calls": 1}]}
  ],
  "removed_members": [
    {"user_id": "...", "username": "...", "total_charge_nano_usd": "...", "calls": 4}
  ]
}
```

- `members` covers current members plus any `created_by` value that still has rows in the
  range, so usage from since-removed members is visible in `removed_members` (rows whose
  `created_by` no longer has an `org_members` row).
- Rows whose key has no `created_by` (system-created) attribute to the owner.
- Token fields sum the corresponding request-log columns; `calls` counts success rows.

ORGL-14. `/org/{org_id}` gains a "Member usage" view (owner only) rendering ORGL-13 with
a range selector (1h/24h/7d/30d) and a per-member bar series, plus a "Limits" management
view rendering ORGL-12 with editable forms for ORGL-10/11.

ORGL-14a. Member usage MUST use an SWR cache key containing the organization ID,
range hours, and bucket count. It MUST issue no usage request until the current
user is known to own that organization. While ownership or the selected usage
query loads, it MUST show a skeleton. Responses from a previous organization or
range MUST NOT replace the current query result.

## 5. Invariants

ORGL-15. Limits never rewrite history: consumption is computed from durable
`request_logs`, so removing members or deleting keys (ORG-17/17b) leaves both the limit
baselines and the analytics unchanged.

ORGL-16. Setting a limit below already-spent consumption is allowed and takes effect
immediately: the next request breaches and is rejected.

ORGL-17. Limit queries MUST use the read pool and the existing
`idx_request_logs_org` (user_id, created_at_unix_ms) / `idx_request_logs_api_key_created` (api_key_id, created_at_unix_ms) indexes; a limit check adds at most three indexed
aggregations per request and MUST NOT add a write on the request path.

## 6. Editor currency

ORGL-20. Every limit editor — the org Limits view (ORGL-14) at the space, member, and
key levels, and the personal-key create/edit dialog — offers a USD/CNY input toggle.
The toggle defaults to CNY. An editor that prefills stored limits MUST keep USD until
the rate snapshot is loaded, then switch the drafts to CNY; it MUST NOT render a stored
limit as empty because the rate is still loading.
Drafts hold display strings in the chosen currency; conversion to the canonical
nano-USD storage happens once, at save time: a USD amount maps directly, a CNY amount
divides by the `cny_per_usd` snapshot (fixed by SB-FX-1) (the same exact-decimal contract as the
wallet CNY top-up) and the editor shows the rate it will use. Stored windows remain
nano-USD; enforcement (ORGL-5..7, ORGL-19) is unchanged.

- Input amounts accept non-negative decimal integers and non-negative decimals with
  one through nine fractional digits. Whitespace surrounding the amount is ignored.
  An empty input means unlimited.
- A usable rate is a positive decimal string matching
  `^(0|[1-9][0-9]*)(\.[0-9]+)?$`. Preserve every fractional rate digit.
  Conversion MUST NOT pass through binary floating-point numbers.
- CNY input converts to nano-USD by dividing its nano-CNY integer by the exact rate.
  Round the result to the nearest integer, with ties rounded upward.
- USD display preserves every stored nano-USD digit and uses decimal notation with at
  most nine fractional digits. CNY display multiplies the stored nano-USD integer by
  the exact rate, rounds to nano-CNY with ties upward, and uses the same notation.
- Switching currency converts a valid nonempty draft through nano-USD. An invalid
  nonempty draft remains verbatim. An empty draft uses its stored window; if that
  window is unlimited or absent, the draft remains empty. On the organization Limits
  page, this rule applies to every space, member, and key draft when any row changes
  the shared currency.
- When the rate is absent or invalid, disable the CNY option and show the unavailable
  rate message regardless of the selected currency. USD remains selectable. A
  nonempty CNY input cannot be saved until a usable rate is available.
- Any invalid nonempty amount aborts its Save action before an API mutation. Show
  the validation error and retain the drafts for correction. This applies to space,
  member, and key limits. Only empty inputs produce unlimited values.

## 7. Personal keys

ORGL-18. A personal key (`org_id IS NULL`) sets its three key-level windows through
the ordinary Token Management create and update endpoints
(`POST /api/dashboard/keys`, `PUT /api/dashboard/keys/{key_id}`) with the fields
`spend_limit_total_nano_usd`, `spend_limit_hourly_nano_usd`,
`spend_limit_daily_nano_usd`. Values are canonical non-negative integer nano-USD
strings. On create, an absent or null field means unlimited. On update the fields are
tri-state: absent keeps the stored value; null or the empty string clears it (SQL
NULL, never an empty string); a value sets it. The key's owner and admins may set
them; an org key's windows are set only through ORGL-11.

ORGL-19. A personal-key breach aborts with HTTP `402 api_key_spend_limit_reached`
and a message naming the window (`total` | `hourly` | `daily`), at the same two
enforcement points and with the same fail-open settlement semantics as ORGL-5 and
ORGL-7. The key list/detail responses expose the three fields.

ORGL-20. Member usage must preserve nonzero token aggregates on PostgreSQL.
Member flags and token sums must project to the integer width used by their decoder.
Decode failures must return an error, not report zero tokens or classify an active
member as removed. Successful requests contribute tokens; failed requests do not.

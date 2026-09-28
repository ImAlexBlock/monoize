# Dashboard API Token (API Key) Management Specification

## 0. Status

- **Purpose:** Define dashboard-managed API keys used to authenticate forwarding endpoints.
- **Scope:** Applies to `/api/dashboard/tokens*` endpoints.

## 1. Data model

### 1.1 API key

An API key row has:

- `id: string`
- `user_id: string`
- `name: string`
- `key_prefix: string` (first 12 characters of the full key; display only and never an authentication or cache identity)
- `key: string` (the full key, stored as plaintext and used for authentication)
- `created_at: RFC3339 string`
- `expires_at: RFC3339 string?`
- `last_used_at: RFC3339 string?`
- `enabled: boolean`
- `sub_account_enabled: boolean`
- `sub_account_balance_nano_usd: string`
- `sub_account_balance_usd: string` (computed)
- `model_limits_enabled: boolean`
- `model_limits: string[]`
- `ip_whitelist: string[]`
- `group_ids: string[]` (ordered group ids, see `groups-registry.spec.md`)
- `channel_bindings: ApiKeyChannelBinding[]`
- `model_bindings: ApiKeyModelBinding[]`
- `max_multiplier: string?` (canonical positive base-10 decimal string)
- `transforms: TransformRuleConfig[]`
- `request_capture_mode: "off" | "capture-all" | "capture-only-abnormal"`

### 1.2 Group-scoped routing fields

TM-GRP-1. `group_ids = []` means the key can route through every registered Group. The
dashboard MUST describe this state as "All Groups" and MUST NOT describe any Group as the
key's default Group. The legacy `token_group` and `allowed_groups` columns no longer exist
(`groups-registry.spec.md` GR-D7).

TM-GRP-2. `group_ids` is an **ordered** JSON TEXT array of group ids. Order is significant:
it defines the routing preference order per `database-provider-routing.spec.md` R-GRP-2.

TM-GRP-3. On API key create/update, the server MUST canonicalize `group_ids` per
`groups-registry.spec.md` GR-C1 (trim, drop empties, dedupe preserving first-occurrence
order) and validate it per GR-C2/GR-C3 (at most 32 entries; every id must exist).
Update MUST validate with the same accessibility rule as create — account-class match
plus public-or-granted for non-admin callers, evaluated against the key's owning user —
including an update that only replaces a previously stored selection; an unknown or
inaccessible id MUST fail the whole update.

TM-GRP-4. On API key create/update, an empty canonicalized `group_ids` value MUST be stored
as `[]` and MUST NOT be rejected.

TM-GRP-5. Group selection permission on create/update, applied after canonicalization:

- if the requesting user's role satisfies `can_manage_system()` (`admin` or
  `super_admin`), any registered group id is selectable;
- otherwise every element of `group_ids` MUST reference a group with
  `user_selectable = 1` OR equal the owning user's current `group_id`.

TM-GRP-6. API key create/update requests that violate TM-GRP-3 through TM-GRP-5 MUST be
rejected with HTTP `400` and code `invalid_request`.

TM-GRP-7. Stored `group_ids` decoding follows `groups-registry.spec.md` GR-C4.

### 1.3 Channel selection

TM-CH-1. `ApiKeyChannelBinding` has exactly `group_id`, `model`, and `channel_id` string
fields. The three fields MUST be non-empty after trimming. `(group_id, model)` MUST be
unique in one key. The stored JSON array MUST contain at most 256 bindings.

TM-CH-2. A routing conflict exists when two or more enabled Providers in one Group have
enabled embedded Channels that expose the same logical model and have a complete billable
rate matrix for that model. Disabled Providers, disabled Channels, unpriced models, and
models without a complete billable rate matrix do not participate. Conflict eligibility
MUST match request-time pricing eligibility.

TM-CH-3. `GET /api/dashboard/tokens/channel-conflicts` MUST return every current routing
conflict. Each conflict contains `group_id`, `group_name`, `model`, and an ordered `options`
array. Each option contains `channel_id`, `channel_name`, and `provider_name`. The endpoint
MUST NOT return a Base URL, credential, internal header, or pricing secret.

TM-CH-4. Create and update MUST require exactly one valid binding for every conflict in the
key's effective Group and model scope. `group_ids = []` includes conflicts from every
Group. When model limits are enabled and non-empty, only listed models are in scope.
Missing, duplicate, stale, or invalid bindings MUST return HTTP `400` with code
`invalid_request`.

TM-CH-5. At request time, a matching binding MUST remove every other Channel attempt for
that `(group_id, model)`. If a conflict exists and the key has no valid matching binding,
the request MUST fail with HTTP `409` and code `channel_selection_required`; it MUST NOT
select a Channel by priority, affinity, or random choice.

TM-CH-6. Conflict enumeration and validation MUST be scoped to the account class of the
authenticated user. A Group whose account class differs from the caller's class MUST NOT
contribute a conflict, because a key owned by that user can never route into it; surfacing
such a conflict would demand a Channel choice for a Group the caller cannot reach and would
block creation of every key. The Channel options in a returned conflict belong to Groups of
the caller's class only.

TM-CH-7. When a Provider create, wholesale create, or update mutation turns a previously
unambiguous `(group_id, model)` scope into a conflict (the scope had exactly one eligible
Channel before the mutation and at least two after), the system MUST pin every existing
API key that is in scope for it — empty `group_ids` or the Group listed; model limits
disabled, empty, or listing the model; the key owner's account class equal to the Group's
class (TM-CH-6) — and holds no binding for that scope, by appending the binding
`{group_id, model, channel_id}` where `channel_id` is the scope's unique pre-existing
Channel. Keys already holding a binding for the scope and keys at the 256-binding cap
(TM-CH-1) are skipped. Pinning MUST run after the mutation succeeds and before the request
returns; a pinning failure MUST NOT fail the mutation but MUST be logged. Introducing a
Channel MUST therefore never change the routed Channel or produce
`channel_selection_required` for any key that existed before the mutation. Scopes that
were already conflicts before the mutation remain governed by TM-CH-4/TM-CH-5, and keys
created after the mutation follow TM-CH-4 unchanged.

### 1.4 Cross-Group model selection

TM-MB-1. `ApiKeyModelBinding` has exactly `model` and `group_id` string fields. Both fields
MUST be non-empty after trimming. `model` MUST be unique in one key. The stored JSON array
MUST contain at most 256 bindings.

TM-MB-2. A model-name conflict exists when two or more Groups of the caller's account class
have at least one enabled Provider whose enabled embedded Channel exposes the same logical
model with a complete billable rate matrix. Disabled Providers, disabled Channels, unpriced
models, and Groups of another account class do not participate. A name that appears in only
one in-scope Group is not a conflict.

TM-MB-3. `GET /api/dashboard/tokens/model-conflicts` MUST return every current model-name
conflict. Each conflict contains `model` and an ordered `options` array. Each option contains
`group_id` and `group_name`. The endpoint MUST NOT return a Base URL, credential, internal
header, or pricing secret.

TM-MB-4. Create and update MUST require exactly one valid binding for every model-name
conflict in the key's effective Group and model scope. `group_ids = []` includes conflicts
from every Group of the caller's class. When model limits are enabled and non-empty, only
listed models are in scope. A binding's `group_id` MUST be one of that conflict's options and
MUST remain in the key's Group scope (`group_ids = []` admits every option). Missing,
duplicate, stale, or invalid bindings MUST return HTTP `400` with code `invalid_request`.

TM-MB-5. At request time, a matching binding MUST remove every attempt whose logical model
equals the bound model and whose Group is not the bound Group. If a model-name conflict
exists for the requested model and the key has no valid matching binding, the request MUST
fail with HTTP `409` and code `model_selection_required`. The error message MUST be exactly
`未选择具体模型，请先选择模型。` It MUST NOT select a Group by priority, affinity, or
random choice.

TM-MB-6. Conflict enumeration and validation MUST be scoped to the account class of the
authenticated user, matching TM-CH-6.

TM-MB-7. `model_bindings` is independent of `channel_bindings`. A key MAY carry both: the
model binding pins the Group for a colliding name, and the Channel binding then pins the
Channel inside that Group. A Channel binding MUST NOT substitute for a missing model
binding.

TM-MB-8. Migration `m20260915_000082_api_key_model_bindings` MUST add
`api_keys.model_bindings TEXT NOT NULL DEFAULT '[]'`. Existing keys decode as an empty
array and therefore fail closed under TM-MB-5 until the owner selects a Group for each
in-scope colliding name.

TM-IP-1. Every non-empty `ip_whitelist` entry on create or update MUST parse as either an exact IPv4/IPv6 address or an IPv4/IPv6 CIDR network. Any invalid entry MUST reject the mutation with HTTP `400` and code `invalid_request`.

TM-IP-2. The server MUST persist exact addresses and CIDR networks in their canonical string representation. It MUST trim entries, deduplicate canonical duplicates, and preserve exact-address entries as addresses rather than converting them to host-prefix CIDRs.

TM-STORAGE-1. API-key dashboard reads and forwarding authentication MUST fail with a storage error when a persisted `model_limits`, `ip_whitelist`, `transforms`, or `model_redirects` value is malformed JSON, is not an array of the declared element type, or has an incompatible database type. They MUST NOT substitute an empty array.

TM-STORAGE-2. Persisted `enabled`, `sub_account_enabled`, `model_limits_enabled`, and `reasoning_envelope_enabled` values MUST be integer `0` or integer `1`. A null value, incompatible database type, or any other integer MUST fail the read. It MUST NOT be replaced by a default value.

TM-STORAGE-3. `group_ids`, `channel_bindings`, and `model_bindings` MUST decode as JSON
arrays of their declared element types. Malformed or wrongly typed values MUST fail the
read and MUST NOT be treated as empty values.

TM-STORAGE-4. A present, non-null `request_capture_mode` MUST equal `"off"`, `"capture-all"`, or `"capture-only-abnormal"`. Any other value or incompatible database type MUST fail the read instead of falling back to `request_capture_enabled` or `"off"`. An absent or null value retains the `"off"` compatibility behavior defined by `request-capture-dumps.spec.md` RCD-C8.

TM-STORAGE-5. Every selected API-key and owning-user column MUST propagate an incompatible database type as a storage error. In particular, a failed `api_keys.key` decode MUST NOT become an empty token and a failed nullable `users.email` decode MUST NOT become null.

TM-STORAGE-6. Every query that decodes a complete API-key record MUST select the owning user's role in the same point or set-based JOIN as `owner_role`. API-key row decoding MUST NOT issue a fallback user query when `owner_role` is absent, null, or malformed.

## 2. Endpoints

All endpoints in this spec require an authenticated dashboard session.

### 2.1 List my API keys

- **Endpoint:** `GET /api/dashboard/tokens`
- **Authorization:** Any authenticated user.
- **Response:** `APIKey[]` for the current user.

### 2.2 Get API key

- **Endpoint:** `GET /api/dashboard/tokens/{key_id}`
- **Authorization:** Any authenticated user, but only for keys owned by that user.
- **Errors:** `404 not_found` if the key does not exist or is not owned by the user.

### 2.3 Create API key

- **Endpoint:** `POST /api/dashboard/tokens`
- **Authorization:** Any authenticated user.
- **Request body:** fields:
  - `name: string`
  - `expires_in_days: integer?`
  - `sub_account_enabled: boolean` (default false)
  - `sub_account_balance_nano_usd: string` (default `"0"`; an explicit value is admin only)
  - `model_limits_enabled: boolean` (default false)
  - `model_limits: string[]` (default empty)
  - `ip_whitelist: string[]` (default empty)
  - `group_ids: string[]` (default empty; empty means all Groups)
  - `channel_bindings: ApiKeyChannelBinding[]` (default empty)
  - `model_bindings: ApiKeyModelBinding[]` (default empty)
  - `max_multiplier: string?` (default null)
  - `transforms: TransformRuleConfig[]` (default empty)
  - `request_capture_mode: "off" | "capture-all" | "capture-only-abnormal"` (default `"off"`)
- **Response:** The created key object including the full key string.

TM-CREATE-1. The generated full key MUST start with the literal prefix `sk-`.

TM-CREATE-2. The server MUST compute `key_prefix` as the first 12 characters of the full key.

TM-CREATE-3. The server MUST persist the full key as plaintext in `api_keys.key`. The `api_keys` table MUST NOT contain a hash, digest, or encrypted copy of the full key. Runtime token validation semantics are defined in `api-key-authentication.spec.md`.

TM-CREATE-3a. The database MUST index `api_keys.key` for complete-token lookup.

TM-CREATE-3b. Migration `m20260824_000039_api_keys_plaintext_only` MUST delete `idx_api_keys_key_hash` and the `api_keys.key_hash` column when they exist. It MUST create `idx_api_keys_key` on `api_keys.key`. Its rollback path MUST NOT restore `key_hash`.

TM-CREATE-4. After successful key creation, there is no required cache invalidation side-effect because the new key does not exist in cache yet.

TM-CREATE-5. `POST /api/dashboard/tokens` MUST read only the `api_key_max_per_user` setting. One process-local async critical section shared by every `UserStore` clone MUST contain both `COUNT(*) WHERE user_id = $1` and the key insert. If the count is at least the supplied positive limit, the store MUST perform no insert and the endpoint MUST return HTTP `403` with code `max_api_keys_reached`. Concurrent create requests in one process MUST never commit more than the configured number of keys for one user.

### 2.4 Update API key

- **Endpoint:** `PUT /api/dashboard/tokens/{key_id}`
- **Authorization:** Any authenticated user, but only for keys owned by that user.
- **Request body:** partial update with optional fields:
  - `name`
  - `enabled`
  - `sub_account_enabled`
  - `sub_account_balance_nano_usd` (admin only)
  - `model_limits_enabled`
  - `model_limits`
  - `ip_whitelist`
  - `group_ids`
  - `channel_bindings`
  - `model_bindings`
  - `max_multiplier`
  - `transforms`
  - `request_capture_mode`
  - `expires_at` (RFC3339 string or null)
- **Errors:** `404 not_found` if the key does not exist or is not owned by the user.

TM-UPD-1. A successful API key update MUST invalidate in-memory API key cache entries for the updated key id before returning the response.

### 2.4a API-key transform safety boundary

TM-TF-1. API key `transforms` are user-scoped request/response shaping rules. They MUST NOT act as routing, pricing, or upstream service-tier controls.

TM-TF-2. The server MUST reject API key create/update requests whose `transforms` array contains any rule outside the allowed API-key transform subset defined by TM-TF-3 and TM-TF-4.

TM-TF-3. Allowed API-key request-phase transforms are exactly:

- `prompt_inject_system`
- `role_system_to_developer`
- `role_merge_consecutive`
- `prompt_append_empty_user`
- `image_compress_input`
- `image_enable_openai_generation_tool`
- `prompt_strip_anthropic_billing_header`
- `cache_anthropic_system`
- `cache_anthropic_tool_use`
- `cache_anthropic_auto`
- `cache_openai_tool_use`
- `cache_user_id`
- `cache_openai_prompt`
- `field_alias_reserved_tool_names`
- `field_custom_tools_to_function`

TM-TF-4. Allowed API-key response-phase transforms are exactly:

- `reasoning_strip_output`
- `reasoning_strip_encrypted`
- `reasoning_to_think_xml`
- `reasoning_from_think_xml`
- `stream_split_sse_frames`
- `reasoning_content_to_summary`
- `reasoning_inject_content_field`
- `reasoning_summary_to_raw_cot`
- `image_markdown_to_output`
- `image_output_to_markdown`
- `image_compress_output`
- `field_alias_reserved_tool_names`
- `field_custom_tools_to_function`

TM-TF-5. API key `transforms` MUST NOT include transforms that can modify routing, upstream model selection, upstream pricing tier, request execution mode, output token ceiling, or arbitrary provider passthrough fields. This forbidden set includes at minimum:

- `field_set`
- `field_remove`
- `stream_force`
- `field_override_max_tokens`
- `reasoning_effort_to_budget`
- `reasoning_effort_to_model_suffix`

TM-TF-6. Requests rejected by TM-TF-2 through TM-TF-5 MUST return HTTP `400` with code `invalid_request`. The error response body MUST include a human-readable message identifying the disallowed transform name.

TM-TF-7. Runtime enforcement MUST be defensive: when an API key row is loaded from storage, the server MUST discard any transform rules that are not permitted by TM-TF-3 and TM-TF-4 before attaching them to the authenticated context.

TM-TF-8. Admin bypass: Users with role `super_admin` or `admin` (as determined by `UserRole::can_manage_system()`) are exempt from TM-TF-2 through TM-TF-5. For admin users, `validate_api_key_transforms` MUST accept any transform, and `sanitize_api_key_transforms` MUST preserve all transforms without filtering.

TM-TF-9. When an API key create or update request is rejected by the server (including but not limited to transform validation failures), the frontend MUST display the server error message to the user via a toast notification. Silent failure is not acceptable.

TM-TF-10. The dashboard transform registry consumed by the API key editor MUST carry explicit transform scope metadata. The API key editor MUST list only transforms whose scope includes `api_key`; transforms that are unavailable for API keys MUST be hidden rather than displayed and rejected later.

### 2.5 Delete API key

- **Endpoint:** `DELETE /api/dashboard/tokens/{key_id}`
- **Authorization:** Any authenticated user, but only for keys owned by that user.
- **Response:** `{ "success": true }`

TM-DEL-1. A successful API key delete MUST invalidate in-memory API key cache entries for the deleted key id before returning the response.

TM-DEL-2. Monoize MUST NOT maintain an API-key name cache. A successful user delete MUST invalidate authentication-cache entries for the deleted user's keys through the user-id reverse index before returning the response.

### 2.6 Batch delete API keys

- **Endpoint:** `POST /api/dashboard/tokens/batch-delete`
- **Authorization:** Any authenticated user.
- **Request body:** `{ "ids": string[] }`
- **Behavior:** The server MUST delete only keys owned by the current user.
- **Response:** `{ "success": true, "deleted_count": integer }`

TM-BATCH-1. A successful batch delete MUST invalidate in-memory API key cache entries for all deleted key ids before returning the response.

TM-BATCH-2. Batch-delete ownership filtering MUST be performed by a set-based database query constrained by both current-user id and requested key ids. It MUST NOT list every key owned by the user or issue one ownership query per requested id.

TM-BATCH-3. `MONOIZE_API_KEY_BATCH_DELETE_MAX_IDS` MUST configure the positive maximum number of ids accepted by one batch-delete request or store call. Missing, zero, invalid, negative, or overflowing values MUST use `400`. A parsed value above `400` MUST be clamped to `400` so every supported SQLite build remains below its bind-variable limit.

TM-BATCH-4. The HTTP endpoint MUST reject a request containing more than TM-BATCH-3 ids with HTTP `400` before an ownership query. Store ownership filtering and transactional batch deletion MUST independently reject an input above the same limit. Every dynamic `IN` list in these paths MUST therefore contain at most 400 values.

## Startup Transform-ID Migration

TM-MIG-1. API-key transform-rule id canonicalization MUST use the persistent `system_settings` marker `migration.api_key_transform_rule_ids.v2`. When the marker value is `complete`, startup MUST perform only the marker point query and MUST NOT scan `api_keys`. Canonicalization MUST use the map defined by `urp-transform-system.spec.md` TF-17.

TM-MIG-2. When the marker is absent, one process MUST process API-key transform rows in ascending-ID keyset batches of at most 300 rows. Each batch MUST select, canonicalize, update changed rows through at most one CASE statement, and commit before the next batch begins. Memory use and one transaction's row count MUST remain bounded by 300. Invalid transform JSON MUST remain unchanged and MUST NOT prevent progress. The completion marker MUST be written in a separate final transaction only after every batch commits; that final transaction MUST also delete the obsolete `system_settings` row `key = "migration.api_key_transform_rule_ids.v1"`. A restart before the marker is written MAY re-read canonicalized rows and MUST converge without changing their values.

TM-QUERY-1. The create limit check MUST use `COUNT(*) WHERE user_id = current_user_id`; it MUST NOT decode every API-key row. The count and insert MUST satisfy TM-CREATE-5.

TM-QUERY-2. Get, update, and delete ownership checks MUST query by both `id` and `user_id`; they MUST NOT list every API key owned by the current user.

## 3. Runtime sub-account cache coherence

TM-Q1. Any operation that mutates `sub_account_balance_nano` for an API key MUST invalidate in-memory API key cache entries for that key id in the same process before returning.

TM-Q2. Sub-account billing behavior is defined in `api-key-sub-account-billing.spec.md`.

## 4. Dashboard balance controls

TM-UI1. The create and edit dialogs MUST render `sub_account_balance_nano_usd` only when the authenticated user's role is `admin` or `super_admin`. The create control MUST accept only a non-negative integer; the edit control MUST accept a signed integer. The frontend MUST validate and submit its value as a decimal string using `BigInt`-equivalent integer arithmetic; it MUST NOT pass the value through JavaScript `Number`, `parseInt`, `parseFloat`, or `toFixed`. When an edit disables sub-account billing, the mutation MUST omit `sub_account_balance_nano_usd` so the server can consolidate the locked current balance.

TM-UI2. A non-admin create or update mutation MUST omit `sub_account_balance_nano_usd` from its JSON request body.
## API Key Analytics

TM-AN1. `GET /api/dashboard/tokens/{key_id}/analytics` MUST accept `range` equal to `today`, `7d`, `30d`, or `all`. An absent range MUST equal `today`. Any other value MUST return HTTP `400`.

TM-AN2. A non-Admin caller MUST own `key_id`, or `key_id` MUST be an org Key (`api_keys.org_id` not NULL) of an organization the caller belongs to (`org_members` row exists); org-wide usage is already member-visible through `orgs.spec.md` ORG-23, so per-Key analytics over the same attribution adds no information. An Admin MAY inspect a Key owned by another user. An unauthorized or unknown Key MUST return the existing not-found response.

TM-AN3. The response MUST contain total, input, cache-read, and output Tokens, request count, consumed Coin in nano units, current independent Key balance, trend buckets, and model rows.

TM-AN4. Consumed Coin MUST equal the sum of persisted canonical `charge_nano_usd` values for matching request logs. The service MUST NOT calculate a historical charge from a current model rate.

TM-AN5. `today` MUST use hourly buckets covering the current Asia/Shanghai local day from 00:00:00 up to the request time. `7d` and `30d` MUST use daily buckets. `all` MUST use daily buckets when retained history spans at most 90 days and calendar-month buckets otherwise.

TM-AN5a. Each bucket boundary MUST align to the start of its own bucket unit in Asia/Shanghai local time (UTC offset `+08:00`). An hourly bucket MUST start at minute zero and second zero local time. A daily bucket MUST start at local midnight. A calendar-month bucket MUST start on day one at local midnight. A label MUST NOT round a boundary that the bucket does not start at; for example, a bucket covering `10:37` to `11:37` MUST NOT be labelled `10:00`.

TM-AN5b. Both range ends MUST align to the bucket unit. The exclusive range end MUST be the start of the unit that follows the unit containing the request instant, and the range start MUST be that end minus the bucket count in whole units. For the `today` range the exclusive end MUST be the Asia/Shanghai calendar-day end following the request instant and the start MUST be that day's local midnight, covering at most 24 hourly buckets. For the `all` range the start MUST additionally not precede the aligned unit that contains the first retained request.

TM-AN5c. Calendar months have unequal lengths, so month buckets MUST be derived from the calendar rather than by dividing the range into equal durations. Bucket `k` MUST cover the calendar month `k` months after the range start month. Aggregation MUST assign a request to the bucket of the calendar month that contains it.

TM-AN6. Each model row MUST contain model, total Tokens, request count, and consumed Coin. Rows MUST sort by total Tokens descending and model ascending.

TM-AN7. A Key with `sub_account_enabled = false` MUST return `balance_mode = wallet` and no independent balance. A Key with `sub_account_enabled = true` MUST return `balance_mode = independent` and its signed stored balance.

TM-AN8. Disabled and expired Keys MUST retain analytics access. A deleted Key MUST retain only a non-secret name snapshot and non-reversible identifier in historical data.

TM-AN9. The Key list MUST remain a compact overview. Selecting one Key MUST open an analytics dialog without navigation. Initial loading MUST use a shape-matched Skeleton. Range changes MUST retain the prior result until the replacement result arrives.

## Account-Class Transition

TM-ENT1. `PUT /api/dashboard/users/{user_id}/account-class` MUST require Admin authorization and exact JSON fields `account_class` and `confirm_delete_api_keys`.

TM-ENT2. `account_class` MUST equal `standard`, `enterprise`, `private`, or `agent`. `confirm_delete_api_keys` MUST equal `true`. Invalid input MUST change no row.

TM-ENT3. One transaction MUST lock the user, settle every signed independent Key balance under SA-DEL2 through SA-DEL4, delete every API Key owned by the user, update the user account class, and append an audit row.

TM-ENT4. The transaction MUST preserve wallet balance after required Key settlement, plan entitlement, orders, redemption history, request logs, and historical usage. A failed step MUST roll back every mutation.

TM-ENT5. After commit, the process MUST invalidate API Key authentication, Group access, Marketplace, routing, and pricing caches affected by the user. A deleted Key MUST fail its next authentication attempt.

## Spend limits per key

AKDL-1. (Superseded by ORGL-18/ORGL-19 in `org-usage-limits.spec.md`.) The
single-window `daily_limit_nano_usd` column (migration
`m20260921_000110_api_key_daily_limit`) is REMOVED (migration
`m20260921_000120_drop_api_key_daily_limit`); the column had no stored values
at removal time. A personal key instead uses the shared key-level windows
`spend_limit_total_nano_usd`, `spend_limit_hourly_nano_usd`, and
`spend_limit_daily_nano_usd`: rolling 3600-second hourly, UTC calendar-day
daily, and lifetime total. Spend is aggregated live from `request_logs` by
`api_key_id` over the window — there is no counter table and no scheduled
reset. Charges bill the owner's personal wallet exactly as an unlimited key;
a limit never moves balance to the key.

AKDL-2. Create and update accept the three fields under the ORGL-18 tri-state
rules (create: absent/null = unlimited; update: absent = keep, null or "" =
clear, value = set). The key's owner and admins may set them. Enforcement is
ORGL-19: HTTP `402 api_key_spend_limit_reached` naming the window, at the
admission preflight and at settlement with fail-open tail semantics.

AKDL-3. The key list/detail responses expose the three `spend_limit_*` fields.
The Token Management create/edit dialog renders three USD inputs labeled
total / hourly / daily with an empty field meaning unlimited — the same
control design as the org Limits view (ORGL-14).

AKDL-4. The sub-account feature is removed from the Token Management dialogs:
the create dialog no longer offers the independent-balance switch or an
initial balance, the edit dialog no longer offers the switch, and the key
list no longer renders a per-key balance. Existing sub-account keys keep
settling against their stored balance until drained or deleted; the backend
settlement and transfer endpoints remain for those keys.

# Organization Spaces Specification

## 0. Status

- Purpose: organization spaces ("组织空间") — a shared workspace with its own wallet, an
  invite link, and member-visible API keys.
- Scope: `users.is_org`, `orgs`, `org_members`, `org_key_shares`, `api_keys.org_id` /
  `api_keys.org_share_mode`, `/api/dashboard/orgs*`, `/dashboard/org`, `/join/{token}`.
  `groups-registry.spec.md` (account classes), `api-token-management.spec.md` (keys),
  `store-billing.spec.md` (wallet, exchange rates).

## 1. Data model

ORG-1. An organization is represented by one `users` row with `is_org = 1` (the **org
wallet row**) plus one `orgs` metadata row whose `id` equals that user row's id. The org
wallet row never holds a session, never logs in, and its `balance_nano_usd` is the org
wallet. `is_org = 0` is an ordinary user.

ORG-2. `orgs`: `id` (PK, equals the org wallet user id), `owner_user_id`, `display_name`
(1..64 chars), `avatar_emoji` (1..8 bytes), `avatar_color` (7-char `#rrggbb`), `avatar_image`
(NULL or a `data:image/` URL of at most 300000 characters — the custom uploaded avatar shown
in preference to the emoji), `invite_token` (unique, 32-char random), `invite_expires_at`
(NULL = never), `invite_created_at`, `created_at`, `updated_at`.

ORG-3. `org_members`: `(org_id, user_id)` PK, `role` ∈ `owner` | `member`, `joined_at`,
`alias` (NULL or 1..32 characters after trimming). Exactly one member row with
`role = owner` per org.

ORG-3a. The `alias` is a per-membership display nickname. `GET
/api/dashboard/orgs/{org_id}` member rows and org key rows (creator or sharer
context) MUST include `alias` alongside `username` for every member. `PUT
/api/dashboard/orgs/{org_id}/members/{user_id}/alias` with body
`{"alias": string | null}` (owner only; a member may set their own) sets or
clears the alias: `null` or a blank string clears it to SQL NULL; a non-null
value MUST trim to 1..32 characters. The response returns `{user_id, username,
alias}`. Every member-visible surface that lists members (member list, key
creator, key share lists, member-usage, limits-page member rows and key-creator
cells) MUST render `username (alias)` when an
alias exists and `username` otherwise. The limits payload
(`GET /api/dashboard/orgs/{org_id}/limits`) member rows MUST include `alias`
and key rows MUST include `creator_alias`; the member-usage payload member
entries MUST include `alias`.

ORG-4. `api_keys.org_id` (NULL = personal key) marks a key as an org key. An org key
is owned by the org wallet row: `api_keys.user_id = org_id` and
`api_keys.created_by` = the member who created it. An org key authenticates as the
org wallet user, is billed to the org wallet, and every request-log row it produces
carries `request_logs.user_id = org_id`. A personal key (`org_id IS NULL`) is owned
by and bills its human `user_id`. Usage never crosses scopes: org-key usage is
invisible to every member's personal analytics, logs, and wallet, and personal-key
usage is invisible to the org.
`api_keys.org_share_mode` is `private`, `public`, `allow`, or `deny`. `org_key_shares
(api_key_id, member_user_id)` holds the member list whose meaning follows the mode: for
`allow` the listed members may see the key and nobody else; for `deny` every member except
the listed ones may see it. Org keys carry the ordinary API-key `model_limits`
restriction; sharing changes only visibility of the key material inside the space.

## 2. Creation

ORG-5. `POST /api/dashboard/orgs` with `{display_name, avatar_emoji?, avatar_color?,
invite_expiry: "24h"|"3d"|"7d"|"30d"|"never"}` creates one org. The caller MUST have
`account_class = enterprise`, `parent_user_id IS NULL`, no `sales_agents` row, and
`is_org = 0`; otherwise HTTP `403 org_forbidden`. A caller MAY own at most 2 orgs
(`MAX_ORGS_PER_USER`); further creations return `409 org_limit_reached`.

ORG-6. Creation carries no fee: the enterprise account class is the sole qualification
(admin-granted), and the org wallet starts at zero. The wallet is later funded only by
owner deposits under ORG-12.

ORG-7. The creator becomes the `owner` member row. The org wallet user row is created with
`is_org = 1`, `role = user`, `account_class = enterprise`, and the default enterprise
group (first public enterprise group, else the system default group).

## 3. Invite link

ORG-8. Each org has exactly one invite link, generated at creation with the chosen expiry
(`24h`, `3d`, `7d`, `30d`, or never). The link path is `/join/{invite_token}`. The owner
can copy it from the space at any time; only the owner receives the token through the API.

ORG-9. `GET /api/dashboard/orgs/invite/{token}` (session required) returns
`{org_id, display_name, avatar_emoji, avatar_color, owner_username, member_count}` without
marking anything; a missing, expired, or full org link returns `404 invite_invalid` with
the same body (no distinction).

ORG-10. `POST /api/dashboard/orgs/join {token}` adds the caller as a `member`. It MUST
reject with `404 invite_invalid` when the token is unknown or expired, `409
org_member_limit_reached` when the org already has 15 members (`MAX_ORG_MEMBERS`), and
`409 org_already_member` when the caller is already a member. Org wallet rows cannot join.

ORG-11. `POST /api/dashboard/orgs/{org_id}/invite` with `{invite_expiry}` regenerates the
single link (old token becomes invalid immediately) and is owner-only.

## 4. Wallet

ORG-12. `POST /api/dashboard/orgs/{org_id}/deposit {amount_nano_usd}` (any member) moves a
positive amount from that member's personal wallet into the org wallet.

ORG-13. `POST /api/dashboard/orgs/{org_id}/distribute {member_user_id, amount_nano_usd}`
(owner only) moves a positive amount from the org wallet into one member's personal
wallet. Both directions write two `billing_ledger` rows in one transaction
(`org_deposit`/`org_deposit_receive` and `org_grant`/`org_receive`) with the org id and
counterparty in `meta_json` and row locks taken org-wallet-first.

## 5. Keys and sharing

ORG-14. `POST /api/dashboard/orgs/{org_id}/keys {name, share_mode?, model_limits?, group_ids?}` creates
an org key owned by the org wallet row (`api_keys.user_id = org_id`,
`api_keys.created_by` = the caller) and billed to the org wallet. The owner's default
`share_mode` is `public`; a member's default is `private`. `model_limits` maps to the
ordinary API-key model restriction. `group_ids` maps to the ordinary API-key Group
selection and is validated against the org wallet row exactly like a personal key's
selection against its human owner: every id must exist, match the wallet's
`account_class` (enterprise), and be public to the wallet (or granted to it). An
absent or empty `group_ids` means every Group the org wallet can access.
`api_keys.org_id` equals the org id.

ORG-14a. The org-key Group picker is fed from the org wallet, never from the acting
member. `GET /api/dashboard/orgs/{org_id}` returns `wallet_groups`: the Group rows
whose `account_class` equals the wallet's, that are public to the wallet (or granted
to it), and that are `user_selectable` or equal the wallet's own `group_id`, in
canonical registry order (GR-D5). Every member receives the same `wallet_groups`
list regardless of the member's own `account_class`, because an org key
authenticates as and bills the wallet; the member's personal Group accessibility is
irrelevant to the picker.

ORG-15. `GET /api/dashboard/orgs/{org_id}/keys` returns (a) the caller's keys in the space
with their sharing state and FULL key material and (b) keys usable by the caller (mode
`public`; or `allow` with a share row for the caller; or `deny` without one) including
full key material for copying. When the caller is the owner, list (b) instead contains
EVERY other key of the space regardless of share mode, because those keys bill the org
wallet the owner funds and ORG-17 keeps keys of removed members alive while no member
surface can see them; the owner is the only surface that can manage them. Both lists
carry each key's `group_ids` and `model_limits`; list (a) carries `created_at`, list (b)
carries `created_by`, so the edit dialog can present the saved state. List (b) and the
ORGL-12 key list include only keys whose `created_by` is NULL or belongs to a current
member; keys left behind by ended memberships never surface. Key material in
the space is always visible, never once-only. Each of the caller's own key entries with
mode `allow` or `deny` also carries `shared_with`: the `user_id` list of its current
`org_key_shares` rows, so the sharing editor can present the saved selection.

ORG-16. `PUT /api/dashboard/orgs/{org_id}/keys/{key_id}/sharing {mode:
"private"|"public"|"allow"|"deny", member_ids?}` is restricted to the key's owner.
`allow`/`deny` MUST list only current members; otherwise `400 invalid_request`.

ORG-17. Removing a member (`DELETE /api/dashboard/orgs/{org_id}/members/{user_id}`,
owner only, cannot remove the owner) deletes their membership and share rows, and —
unless the request body sets `delete_keys = false` — also deletes every org key the
member created (`api_keys.org_id = {org_id} AND created_by = member`) in the same
transaction. The response reports `deleted_keys`. Historical request-log rows written
while the member belonged to the org remain attributed to the org
(`request_logs.user_id = org_id`), so org analytics and logs are unchanged by removal.

ORG-17c. `delete_keys` defaults to `true`. When it is `false`, keys the member created
remain org keys that keep working and keep billing the org wallet, visible to the owner
(ORG-15 owner list) and in the limits management surface (ORGL-12). The frontend remove
action always sends the default and confirms that the member's keys are deleted.

ORG-17a. `DELETE /api/dashboard/orgs/{org_id}/leave` lets a non-owner member leave the
space. It behaves like ORG-17 removal of the caller with `delete_keys = true`:
membership and share rows are deleted and every org key the leaver created is deleted
in the same transaction, so a saved key string stops billing the org wallet
immediately. Historical request-log rows keep their org attribution. No balance moves.
The owner cannot leave (400 `invalid_request`).

ORG-17b. `DELETE /api/dashboard/orgs/{org_id}/keys/{key_id}` deletes one org key
(`created_by = caller`, or the owner for any key). The org's historical request-log
rows for that key are preserved and keep their org attribution. The handler MUST NOT
run the store's key deletion inside another write transaction: on SQLite every
`DbPool::write()` acquires the same process-wide mutex, and nesting two would
deadlock the request.

ORG-17c. `PUT /api/dashboard/orgs/{org_id}/keys/{key_id}` edits one org key with the
optional fields `{name?, group_ids?, model_limits_enabled?, model_limits?,
ip_whitelist?, expires_in_days?}`. Authorization equals ORG-17b (the key's creator or
the owner). `name` must stay 1..64 characters after trimming; `expires_in_days` must be
>= 1 and replaces `expires_at` with now + N days. `group_ids` follows the ORG-14
validation rule. The update reuses the ordinary key update path, so Group changes
invalidate the key's authentication cache and take effect on the next request. Absent
fields keep their stored value.

## 6. Surfaces

ORG-18. `/dashboard/org` is the org space page: org switcher (member's orgs), tabs for
overview (wallet balance, deposit/distribute, invite link card), members (list, remove),
keys (mine + shared to me, create, sharing editor, edit dialog with name/groups/model
restriction per ORG-17c), and the org ledger. Creation and
joining are reachable from the same page.

ORG-18a. The org-space shell is one viewport-height flex row: a 240px sidebar at `lg`
and above, and below `lg` a fixed floating menu button opening the same navigation in a
left sheet. The main column is a flex column whose single content wrapper is
`min-height: 0; flex: 1`, so full-height children — the ORG-24 logs table — receive a
bounded height; the wrapper carries top padding for the floating button below `lg`.
Wide tables (limits, member usage) scroll horizontally inside their bordered sections.

ORG-19. `/join/{token}` is a centered landing: org avatar, display name, owner username,
member count, and Accept/Decline buttons. Accept enters the space; Decline returns to the
dashboard.

ORG-20. `GET /api/dashboard/orgs` lists the caller's orgs with role, member count, wallet
balance, and (owner only) the invite token and expiry. Admin surfaces exclude `is_org`
rows from the ordinary user groupings.

## 7. Limits

ORG-21. The compile-time defaults are `MAX_ORGS_PER_USER = 2` and
`MAX_ORG_MEMBERS = 15`. Migration 076 adds `users.org_creation_limit` and
`orgs.max_members`; NULL means "use the default" (ORG-21a). An admin may set
them per owner / per org through ORG-28. `GET /api/dashboard/orgs` additionally
returns `creation_limit`, `creation_used`, and `can_create` (eligible AND below
the quota), which drives the create affordances.

ORG-8a. Admin-role accounts (`role.can_manage_users()`), sales agents, and
agent-class accounts can neither create nor join organizations; creation also
requires the enterprise account class as before. The workspace sidebar hides
every org entry for these roles.

## 8. Ledger

ORG-22. `GET /api/dashboard/orgs/{org_id}/ledger` (any member) returns the org wallet's
`billing_ledger` rows, newest first, at most 200.

ORG-22a. The ledger response body is `{"entries": [...], "actors": {...}}`. Each entry
MUST include `id`, `kind`, `delta_nano_usd`, `balance_after_nano_usd`, `created_at`, and
`meta` (the parsed `meta_json` object, or null). `actors` MUST map every
`meta.from_user_id` / `meta.to_user_id` referenced by the returned entries to
`{"username": string | null, "alias": string | null}`, where `alias` is the membership
alias when the user is still a member (ex-members resolve with `alias: null`). The UI
MUST label usage kinds (`request_charge`, `api_key_charge`) as a member usage charge —
not as "Other" — and MUST render a detail line: recharge receive rows show the
recharging member, distribution (`org_grant`) and deletion-refund rows show the
receiving member, and usage rows show the logical model.

## 9. Space analytics and logs

ORG-23. `GET /api/dashboard/orgs/{org_id}/analytics?buckets={1..48}&range_hours={1..720}`
(any member) returns exactly the response shape of `GET /api/dashboard/analytics`, where
every aggregate (bucketed model cost/calls/tokens, provider calls, today and range totals)
is computed over request-log rows with `rl.user_id = {org_id}`. Rows of members' personal
keys never appear. Org analytics are computed from the durable log attribution, not from a
live join on `api_keys.org_id`, so removing members or deleting keys does not rewrite
history.

ORG-24. `GET /api/dashboard/orgs/{org_id}/request-logs` (any member) accepts the same
query parameters as `GET /api/dashboard/request-logs` (`limit` 1..200, `offset`, `model`,
`status`, `api_key_id`, `search`, `time_from`, `time_to`; `username` is ignored) and
returns the same response shape (`data`, `total`, `total_charge_nano_usd`, `limit`,
`offset`) over request-log rows with `rl.user_id = {org_id}`. Non-admin callers get
masked error detail under the same `mask_sensitive_info` setting as the personal log
list. The response MUST omit `upstream_response_model` for non-admin callers, regardless
of `mask_sensitive_info`, under `request-logs.spec.md` RL-API16. A non-member caller
receives `404 not_found` for both endpoints.

ORG-24a. Personal dashboards exclude org usage. `GET /api/dashboard/analytics`,
`GET /api/dashboard/request-logs`, and the personal key list exclude every row and key
whose `api_keys.org_id` is not NULL, so org-key usage never appears in a member's
personal usage, logs, or token list.

ORG-24b. `GET /api/dashboard/tokens/{key_id}/analytics` answers for an org key when the
caller is a member of the key's space (rule recorded as TM-AN2 in
`api-token-management.spec.md`). The aggregates follow the key's durable org
attribution (`request_logs.user_id = org_id`, `api_key_id = key_id`), so the org key
card's analytics view works for every member and the owner.

ORG-25. `/org/{org_id}` navigation offers Overview, Usage Analysis, Cache Hit Rate, Logs,
Members, Keys, Wallet; the three analytics/logs pages are the workspace pages bound to the
ORG-23/24 endpoints instead of the personal ones.

ORG-26. `/join/{token}` without a session redirects to `/login` carrying the invite path
as return state; a successful login returns to the invite. The return path is accepted
only when it starts with a single `/`.

ORG-26a. Every invite has a 6-character `invite_code` (unique, unambiguous
alphabet). `invite_preview` and `join_org` accept the invite token OR the code;
the owner's invite card shows the link and the code side by side, both
copyable, and regeneration rotates both.

ORG-26b. When `member_count >= max_members`, `invite_preview` still returns the
org with `is_full: true`; the landing page shows a full-state banner and disables
Accept, and `join_org` rejects with `409 org_member_limit_reached`. The owner's
invite card shows the same warning.

## 10. Deletion

ORG-27. `DELETE /api/dashboard/orgs/{org_id}` (owner or admin) removes the space
atomically: the remaining wallet balance refunds to the owner (ledger kinds
`org_delete_refund` / `org_delete_receive`), `org_key_shares` rows are deleted,
the org's keys are deleted (`WHERE org_id = {org_id}`),
`org_members`, the `orgs` row, and the `is_org = 1` wallet user row are deleted.
`billing_ledger` and `request_logs` history is preserved.

## 11. Admin console

ORG-28. `GET /api/dashboard/admin/orgs` (admin) lists every space with owner,
member count, cap, balance, invite token/code, and expiry.
`PUT /api/dashboard/admin/orgs/{org_id}` sets `max_members` (1..1000) and/or the
owner's `org_creation_limit` (0..100). The admin page can delete a space through
ORG-27.

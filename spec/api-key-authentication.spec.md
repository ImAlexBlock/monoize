# Forwarding API Key Authentication Specification

## 0. Status

- **Purpose:** Define forwarding API-key authentication for forwarding endpoints.
- **Scope:** Applies to all forwarding endpoints in `spec/unified_responses_proxy.spec.md` §2.2 (including `/api` aliases).

## 1. Token extraction

AK1. Monoize MUST extract the forwarding API key token from one of these HTTP headers:

- `Authorization: Bearer <token>`
- `x-api-key: <token>`

AK2. If neither header is present, or if `Authorization` is present but does not use the `Bearer ` prefix, Monoize MUST return:

- HTTP `401`
- error code `unauthorized`

## 2. Token resolution sources

Monoize MUST use one source for resolving `<token>` to `tenant_id`:

- **Database API keys** stored in the `api_keys` table (dashboard-managed).

## 3. Resolution order

AKP1. Monoize MUST attempt database API key validation first **only** if:

- the token starts with the literal prefix `sk-`, AND
- the token length is at least 12 characters.

AKP2. If AKP1 holds, Monoize MUST:

1. Reject a token longer than 512 bytes as invalid before querying it.
2. Look up the API-key row by complete-token equality through the indexed plaintext `api_keys.key` column.
3. Read the API-key row and its owning user in the same database query.
4. Use the complete token as the in-memory authentication-cache identity. The first 12 characters MUST NOT identify a cache entry.
5. If no row exists, treat the token as invalid.
6. If a row exists, Monoize MUST validate the token as follows:
   - `enabled` MUST be true.
   - `expires_at` MUST be null or a future timestamp.
   - the stored full key value MUST equal the full token.
   - the referenced user MUST exist and have `enabled` true.
   - if an in-memory cache entry for the same complete token exists but fails cache-side validation, Monoize MUST invalidate that cache entry and continue with the database validation path in the same request.
   - if cache invalidation occurs after the database read but before cache publication, Monoize MUST discard that result and repeat database validation.
7. If validation succeeds:
   - Monoize MUST update `last_used_at` to the current time.
   - Monoize MUST authenticate the request with `tenant_id = user.id`.
   - Monoize MUST attach API key routing policy and runtime guards (`max_multiplier`, `effective_groups`, ordered `transforms`, `reasoning_envelope_enabled`) to the authenticated context.
   - The attached `transforms` value MUST already satisfy the API-key transform safety boundary in `api-token-management.spec.md` §2.4a. Stored disallowed transform rules MUST be discarded before request processing continues.

AKP3. If database validation fails or is skipped, Monoize MUST return:

- HTTP `401`
- error code `unauthorized`

AKP4. An authentication-cache hit MUST be accepted only when the cached entry's
generation equals the cache's current invalidation generation. If the generations
differ, Monoize MUST treat the entry as a miss and continue with database validation.
This check MUST occur before returning the cached authorization policy to the caller.

## 4. Effective group resolution

Groups are first-class registry rows (`groups-registry.spec.md`). All group values in this
section are group **ids** (UUID strings), never names.

AKG1. The owning user's single `group_id` MUST NOT affect API-key Group resolution.

AKG2. The authenticated API key row MUST contain `group_ids: string[]` (ordered).

AKG2a. A stored `api_keys.group_ids` value that is absent, null, empty string, or a
serialized empty array decodes as `[]`. Every other value MUST decode as a JSON array of
strings; malformed JSON, a non-array JSON value, a non-string element, or an incompatible
database type MUST fail authentication with an internal storage error. It MUST NOT be
converted to `[]`.

AKG3. The billing-plan layer is read as `plan_group_ids: string[] | absent` from the
enabled plan referenced by the user (`billing-plan-subscriptions.spec.md` BP-R2: a
disabled or missing plan contributes no restriction). Storage decoding follows AKG2a.

AKG4. The authenticated context MUST represent request-scoped group access as
`effective_groups: string[] | null` where the array is an **ordered** list of group ids.
For API-key authentication the array is always concrete: it contains exactly the group ids
the request may route to. `null` is reserved for system-originated internal traffic.

AKG4a. `accessible_groups` is the owner's accessible Group set from
`groups-registry.spec.md` section 0B: every Group whose `account_class` equals the owner's
`account_class` and which is public or carries an explicit `user_group_grants` row for the
owner. An administrator or super-administrator owner accesses every Group of that account
class. The order is `sort_order`, then `created_at`, then `id`, all ascending.

AKG5. Authentication MUST resolve `effective_groups` as follows:

1. `base = api_key.group_ids` with order preserved. An empty `base` selects every Group.
2. If `plan_group_ids` is present and non-empty and `base` is empty, then
   `resolved = plan_group_ids`.
3. If `plan_group_ids` is present and non-empty and `base` is non-empty, then
   `resolved` is the intersection in `base` order. An empty intersection MUST reject
   authentication under AKG5c before applying step 5.
4. Otherwise `resolved = base`.
5. If `resolved` is empty, then `effective_groups = accessible_groups`. Otherwise
   `effective_groups` is `resolved` with every id absent from `accessible_groups` removed,
   preserving `resolved` order.

AKG5a. Consequence: a stored selection survives a Group becoming private or a grant being
revoked. The Group stops routing immediately and starts routing again without editing the
key once the owner is granted access. An empty stored selection never routes to a private
Group the owner cannot access.

AKG5b. An empty `resolved` and an empty restriction result are distinct states and MUST NOT
produce the same `effective_groups`.

- `resolved` empty means the key selects every Group only when `base` is empty and
  no non-empty plan restriction applies. Step 5 replaces it with `accessible_groups`.
- An empty intersection of non-empty key and plan selections means no Group is
  authorized. It MUST NOT be expanded to `accessible_groups`.
- `resolved` non-empty whose restriction against `accessible_groups` removes every id means
  the key selects only Groups the owner cannot access. This is an empty authorization, not
  an absent restriction.

When step 5 produces an empty array from a non-empty `resolved`, authentication MUST fail
closed. It MUST NOT attach `effective_groups = []`, because
`database-provider-routing.spec.md` R-GRP-1a treats `[]` as every Provider being
group-eligible, which would route the request to Providers in Groups the owner cannot
access.

AKG5c. A request rejected under AKG5b MUST return the same response as an API key with no
routable Group. It MUST NOT disclose which Groups the key selected, which Groups the owner
can access, or that a visibility change caused the rejection.

AKG5d. `accessible_groups` MUST be empty only when the owner can access no Group of the
owner's `account_class`. In that state every API key of that owner MUST fail closed under
AKG5b regardless of its stored selection, because step 5 yields an empty array in both
branches.

AKG-M1. `GET /v1/models` MUST intersect the visible model list with the key's resolved
effective groups (section 4): a model is listed only when an enabled Provider of one of
those groups serves it through an enabled embedded Channel. `effective_groups == null`
(internal system traffic) and an empty resolved list (every group eligible, R-GRP-1a) keep
the unrestricted list. The API-key `model_limits` filter applies after the Group filter.

AKG6. The attached array MUST be deduplicated preserving first occurrence order. Elements
MUST NOT be lowercased, sorted, or otherwise rewritten; group ids are opaque and their
order defines routing preference (`database-provider-routing.spec.md` R-GRP-2).

AKG7. Authentication MUST attach the key's `channel_bindings` after strict storage
decoding. Runtime Channel selection follows `api-token-management.spec.md` TM-CH-5.

AKG8. Authentication MUST attach the key's `model_bindings` after strict storage
decoding. Runtime Group selection for a colliding logical model follows
`api-token-management.spec.md` TM-MB-5.

## 5. Error response uniformity

AKE1. Authentication failures MUST NOT reveal whether a token partially matched an existing prefix.

AKE2. The number of database rows decoded for one invalid token MUST be bounded independently of the total API-key count.

AKE3. Authentication MUST decode the selected API-key row and owning user before publishing an authentication-cache entry. The following persisted API-key fields are required authorization-policy values:

- `model_limits`, `ip_whitelist`, `transforms`, `model_redirects`, `channel_bindings`, and `model_bindings` MUST decode from JSON arrays of their declared element types;
- `enabled`, `sub_account_enabled`, `model_limits_enabled`, and `reasoning_envelope_enabled` MUST decode from integer `0` or integer `1` only;
- the owning user's `enabled` value MUST decode from integer `0` or integer `1` only;
- `request_capture_mode` MUST be absent, null, or one of `"off"`, `"capture-all"`, and `"capture-only-abnormal"`; absent or null means `"off"` as defined by `request-capture-dumps.spec.md`;
- every decoded non-empty `ip_whitelist` entry MUST be a valid IP address or CIDR network;
- every decoded `model_redirects` entry MUST satisfy the stored-rule invariants applied by API-key create and update operations.

AKE4. If any required value in AKE3 has malformed JSON, an incompatible database type, an out-of-domain integer value, an unsupported enum value, or invalid element semantics, API-key validation MUST return an internal storage error. It MUST NOT authenticate the request, publish or retain an authentication-cache entry from that database read, or record the key as used. Empty/default policy values MUST NOT be substituted for the invalid value.

## 6. Max multiplier enforcement

AKM1. The effective `max_multiplier` for a request is resolved as follows:

1. Let `ceiling` = API key's stored `max_multiplier` (may be null).
2. Let `requested` = the first defined value from:
   - `max_multiplier` field in the request body `extra`, OR
   - `X-Max-Multiplier` HTTP header parsed as a finite positive float.
3. Resolution:
   - If both `ceiling` and `requested` are present: `effective = min(requested, ceiling)`.
   - If only `ceiling` is present: `effective = ceiling`.
   - If only `requested` is present: `effective = requested`.
   - If neither is present: `effective = null` (no multiplier filtering).

AKM2. Consequence: a per-request `requested` value can only lower the effective multiplier below the API key ceiling, never raise it above.

AKM3. During provider selection, if `effective` is not null, providers whose model entry `multiplier` exceeds `effective` MUST be excluded from the candidate set.

## 7. Model allowlist enforcement

AKL1. If `model_limits_enabled = true` and `model_limits` is non-empty on the authenticated API key, every forwarding request MUST be rejected unless the logical model requested by the client is an exact member of `model_limits`.

AKL2. AKL1 enforcement MUST occur on forwarding endpoints themselves, not only on `/v1/models` listing responses.

AKL3. Requests rejected by AKL1 MUST return HTTP `403` with code `model_not_allowed`.

## 8. Group id cache placement

AKC1. The API key cache entry MUST include the key's accessible group ids, resolved once at cache fill. Request authentication MUST read group ids from the cache entry instead of re-querying group membership per request.

AKC2. Group mutations that change membership or grants MUST invalidate the whole API key cache (generation bump), preserving the existing invalidation hooks; a stale group id set MUST NOT outlive one cache TTL (60 s) plus one generation.

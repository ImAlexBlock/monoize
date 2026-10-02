# Dashboard Data Cache Specification

## 1. Scope

DC1. This specification applies to the browser-process SWR cache used by the dashboard.

DC2. An SWR key that contains authenticated data MUST be scoped to the currently authenticated principal by cache invalidation at every authentication transition.

## 2. Authentication transitions

DC3. After login or registration succeeds and before the new token becomes active, the client MUST delete every SWR cache entry without revalidation.

DC4. After logout starts clearing authentication state, the client MUST delete every SWR cache entry without revalidation even when the logout HTTP request fails.

DC5. When current-user refresh rejects the stored authentication state, the client MUST clear the token and every SWR cache entry. This deletion MUST include fixed keys, parameterized request-log and analytics keys, marketplace keys, and Provider-detail keys.

## 3. Mutation dependencies

DC6. A successful full settings mutation MUST revalidate `PUBLIC_SETTINGS`, `PRICING_PROFILE_PATTERNS`, and `PROVIDERS` after publishing the returned `SETTINGS` value.

DC7. A successful Provider create, update, or delete MUST revalidate `PROVIDERS`, `CONFIG`, and `MARKETPLACE_MODELS`. Create and delete MUST also revalidate `STATS`. Delete MUST remove the deleted Provider-detail key without revalidation. Provider mutations MUST NOT revalidate `DASHBOARD_GROUPS`: the group registry is a first-class resource and is not derived from provider rows.

DC8. A successful model-metadata create, update, delete, or models.dev sync MUST revalidate `MODEL_METADATA`, `MARKETPLACE_MODELS`, and `PROVIDERS`. Models.dev sync MUST also revalidate `BILLING_RATES`. Models.dev sync MUST also revalidate `BILLING_RATE_PROFILES`, every present `BILLING_RATES_FOR_PROFILE(profile)` key, and every present `MODEL_METADATA_DETAIL(model_id)` key, because the sync replaces both catalogs wholesale.

DC8a. A successful model-metadata delete MUST remove the deleted record's `MODEL_METADATA_DETAIL(model_id)` key without revalidation. A successful model-metadata update MUST publish the server's returned record into that key without revalidation.

DC9. A successful billing-rate create, update, delete, or catalog sync MUST revalidate `BILLING_RATES` and `PROVIDERS`. Each MUST also revalidate `BILLING_RATE_PROFILES` and every present `BILLING_RATES_FOR_PROFILE(profile)` key, because rate writes can change per-profile counts and rows.

DC9a. `BILLING_RATE_PROFILES` MUST be the canonical SWR key for the profile-summary resource served by `GET /api/dashboard/billing-rates/profiles`. `BILLING_RATES_FOR_PROFILE(profile)` MUST be the canonical SWR key for the filtered rate list served by `GET /api/dashboard/billing-rates?pricing_profile={profile}`. A page MUST NOT derive profile names or counts from the unfiltered `BILLING_RATES` key when it does not otherwise need rate rows.

DC9b. An optimistic billing-rate upsert MUST insert or replace the row only in populated
`BILLING_RATES` and `BILLING_RATES_FOR_PROFILE(row.pricing_profile)` cache entries. It MUST
remove the same row ID from every other populated profile entry. It MUST NOT insert the
row into another profile or initialize an absent catalog with a partial list. Omitted
update fields MUST retain their stored values, except `source`, which defaults to `manual`.

DC9b1. A rejected billing-rate upsert MUST restore every populated rate-cache array
that still equals the optimistic array produced by that upsert, without requiring a
successful follow-up GET. It MUST NOT restore an array replaced by a later cache write.
The client MUST also revalidate populated rate views after rejection.

DC9c. A successful pricing-profile copy, pricing-profile delete, or profile-model rename
MUST revalidate `BILLING_RATES`, `BILLING_RATE_PROFILES`, every present
`BILLING_RATES_FOR_PROFILE(profile)` key, and `PROVIDERS`. Every page that performs these
mutations MUST use the shared mutation helper.

DC10. A successful pricing-pattern mutation MUST publish the returned `PRICING_PROFILE_PATTERNS` value and revalidate `SETTINGS` and `PROVIDERS`.

DC11. A successful user create MUST revalidate `USERS` and `STATS`. A successful user update MUST revalidate those keys plus `ME`. A successful user delete MUST revalidate `USERS` and `STATS`. User mutations MUST NOT revalidate `DASHBOARD_GROUPS`.

DC11a. A successful group create, update, or reorder MUST revalidate `DASHBOARD_GROUPS`. A successful group delete MUST revalidate `DASHBOARD_GROUPS`, `USERS`, `API_KEYS`, `PROVIDERS`, `BILLING_PLANS`, and `ME` because the server-side deletion cascade rewrites group references in those entities (`groups-registry.spec.md` §3).

## 4. Global operations

DC12. Global revalidation MUST target every key currently present in the SWR cache. It MUST NOT rely on a fixed key list.

DC13. Global cache deletion MUST target every key currently present in the SWR cache and MUST disable revalidation for that operation.

DC14. Every dashboard consumer of one server resource MUST use that resource's exported canonical SWR key and hook. A page MUST NOT create an alias key for `MODEL_METADATA` or another exported resource because mutation invalidation would leave the alias stale.

## 5. Polling cadence

DC15. Dashboard SWR polls whose handler executes a multi-query aggregate over `request_logs` (usage analysis, admin usage ranking, public usage ranking, org analytics, cache hit-rate) MUST use a refresh interval of at least 10000 ms. The status page MAY use 5000 ms. Live request logs remain SSE-driven with the existing 3000 ms disconnect fallback.

DC16. Studio canvas updates MUST arrive through the studio SSE stream (ST-X1); studio pages MUST NOT poll run/step state.

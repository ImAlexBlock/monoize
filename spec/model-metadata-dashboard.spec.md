# Model Metadata Dashboard Specification

## 0A. LynShen migration release

MD-MIG-1. Model metadata and billing-rate records remain administrative source catalogs.
After the Provider pricing migration, `pricing_profile_model_patterns` and metadata Profile
suggestions MUST NOT select a runtime Provider billing Profile. Runtime selection MUST use
`provider-pricing.spec.md` PP-E1 through PP-E4.

MD-MIG-2. Every committed Marketplace-relevant insert, update, delete, and bulk sync in
this subsystem MUST advance `marketplace_generation` through the database triggers in
`model-marketplace.spec.md` MM-G1 through MM-G13. A rolled-back write MUST leave the
generation unchanged.

MD-MIG-3. The authenticated Model Database and Billing Profile tabs remain Console
surfaces. The public Marketplace is a separate read-only surface governed by
`model-marketplace.spec.md` and MUST NOT expose Profile names or source catalog rows.

## 0. Status

- Product name: Monoize.
- Scope:
  - dashboard UI page `/dashboard/models` for viewing/editing model metadata;
  - dashboard UI tabs for billing-rate records and pricing-profile patterns;
  - CRUD REST endpoints for `model_metadata_records`;
  - CRUD REST endpoints for `billing_rate_records`;
  - sync-vs-manual priority semantics.

## 1. Data Model

MD1. This spec operates on `model_metadata_records` (defined in `user-billing-and-model-metadata.spec.md` § 7) and `billing_rate_records` (defined in `metered-billing.spec.md` § 1).

MD2. `model_id` (PK) is the **bare model API name** (e.g. `gpt-4o`, `claude-sonnet-4-20250514`), not prefixed with provider.

MD3. `source` column distinguishes record origin:

| `source` value | Semantics |
|----------------|-----------|
| `models_dev` | Populated or last updated by Models.dev sync |
| `manual` | Created or last updated by admin manual edit |

MD4. All pricing fields are nano-unit integer strings (same precision as the billing spec),
denominated in the row's `price_currency` as defined by
`user-billing-and-model-metadata.spec.md` M4 and M4a.

MD4a. `price_currency` MUST equal `"CNY"` or `"USD"`. A row created by manual edit MUST
default to `"CNY"`; a row written by Models.dev sync MUST use `"USD"`. An update that omits
the field MUST preserve the stored value.

MD5. `raw_json` stores all provider variants from models.dev as `{ "providers": { "openai": {...}, "azure": {...}, ... } }`. Every value inside a variant's `cost` object MUST be stored and served by the §3.2 detail endpoint as its exact decimal string rather than a JSON number. This enables the edit UI to switch pricing source without JavaScript binary-floating-point conversion. The §3.1 list endpoint omits `raw_json` entirely.

MD6. `models_dev_provider` indicates which models.dev provider's pricing is currently applied.

MD7. Billing computation MUST NOT read model pricing directly from `model_metadata_records`. Billing computation MUST read `billing_rate_records`.

MD8. When a model metadata row is created, updated, or synced with token prices, the server MUST mirror the present token prices into `billing_rate_records` rows whose `source` identifies the metadata origin.

MD8a. A mirrored `billing_rate_records` row MUST set `unit_price_currency` to the
`price_currency` of the metadata row it mirrors. The mirror carries the price unchanged, so
it MUST carry the same denomination; it MUST NOT substitute a fixed currency and MUST NOT
inherit the default of the dashboard billing-rate API.

MD9. Every non-null metadata price and every billing-rate `unit_price_nano` MUST be a canonical non-negative integer string. A negative, signed-plus, fractional, exponent, or out-of-range value MUST be rejected with `400 invalid_request`.

MD10. Models.dev decimal USD-per-million prices MUST be parsed directly from their JSON decimal token. The conversion to nano-unit per token is `trunc(price_usd_per_million * 1000)`. This conversion MUST NOT pass through `f32` or `f64`. For example, `1.001` MUST become `"1001"`. The synced row records `price_currency = "USD"` under MD4a, so the number is not converted between currencies at sync time.

MD10a. Migration `m20260909_000067_model_metadata_price_currency` MUST add
`price_currency` to `model_metadata_records` with a `CHECK` constraint admitting only `CNY`
and `USD`, set it to `"CNY"` for every row whose `source = "manual"`, and set it to
`"USD"` for every other row. It MUST then set `unit_price_currency` on every
`billing_rate_records` row whose `id` matches `model_metadata:{model_id}:{usage_class}` to
the `price_currency` of that `model_id`, so that MD8a holds for rows mirrored before this
migration. It MUST NOT change any `unit_price_nano` or metadata price digit.

## 2. Sync Priority & Merge

SP1. `POST /api/dashboard/model-metadata/sync/models-dev` MUST skip upsert for any row whose current `source = 'manual'`.

SP2. Rows with `source = 'models_dev'` (or no prior row) MUST be upserted normally.

SP3. When models.dev contains the same bare model name under multiple providers:
  - Group all variants by bare model name.
  - Select the default variant using the following priority:
    1. **Official provider preference**: If the canonical `model_id` belongs to a known model family, prefer the variant from that family's official provider — but only when that variant has strictly positive `input_cost_per_token_nano`. The known family→provider mappings are:
       - Model IDs starting with `gpt-` or `o` followed by a digit (e.g. `o1`, `o3-pro`) → provider `openai`
       - Model IDs starting with `claude-` → provider `anthropic`
       - Model IDs starting with `gemini-` → provider `google`
       - Model IDs starting with `grok-` → provider `xai`
       - Model IDs starting with `deepseek-` → provider `deepseek`
       - Model IDs starting with `mistral-` or `codestral-` or `pixtral-` or `ministral-` → provider `mistral`
    2. **Highest-cost fallback**: If no official provider variant exists or it lacks positive pricing, fall back to the variant with the highest non-zero `input_cost_per_token_nano`. This prevents resale losses when the platform charges users based on the stored price.
  - Store all variants in `raw_json.providers` so the user can switch sources in the edit UI.

SP4. Sync MUST first delete all records with `source != 'manual'`, then insert new data. This ensures models removed upstream are also cleaned up. Sync response MUST return `upserted`, `skipped`, and `deleted` counts.

SP5. During sync, canonical `model_id = "auto"` MUST be ignored (not inserted/updated).

SP6. During sync, a grouped model MUST be ignored when **all** variants have missing or non-positive (`<= 0`) `input_cost_per_token_nano`. In other words, at least one variant must have strictly positive input pricing to be eligible.

SP7. During sync, canonical model IDs that end with `-thinking`, `:thinking`, or `-think` MUST be ignored (not inserted/updated).

SP8. Admin MAY explicitly reset a manual record back to sync-managed by updating it with `source = 'models_dev'` via the PUT endpoint, after which subsequent syncs will overwrite it.

## 3. CRUD Endpoints

### 3.1 List model metadata

- Method/Path: `GET /api/dashboard/model-metadata`
- Auth: admin required.
- Response: `ModelMetadataRecord[]` in which every record omits `raw_json`. The list serves catalog tables and profile grouping; `raw_json` (all models.dev provider variants, MD5) is served only by the single-record endpoint §3.2. The response body for the full catalog MUST stay below 2 MiB for a 2000-record catalog; embedding `raw_json` (measured 3.7 MB for 1710 records) violates this rule.

### 3.2 Get single model metadata

- Method/Path: `GET /api/dashboard/model-metadata/{model_id}`
- Auth: admin required.
- Response: `200 OK` with the full `ModelMetadataRecord` including `raw_json`.

### 3.3 Upsert model metadata

- Method/Path: `PUT /api/dashboard/model-metadata/{model_id}`
- Auth: admin required.
- Body (all fields optional):

```json
{
  "models_dev_provider": "openai",
  "mode": "chat",
  "input_cost_per_token_nano": "30000",
  "output_cost_per_token_nano": "60000",
  "cache_read_input_cost_per_token_nano": "15000",
  "output_cost_per_reasoning_token_nano": null,
  "max_input_tokens": 128000,
  "max_output_tokens": 16384,
  "max_tokens": 128000
}
```

- If row exists: update only fields present in the JSON object, set `source = 'manual'`, set `updated_at = now()`. An omitted field preserves its stored value. An explicitly null nullable field clears its stored value.
- If row does not exist: insert with provided fields, `source = 'manual'`, `raw_json = '{}'`, `updated_at = now()`.
- The metadata write and deletion/replacement of its generated `billing_rate_records` mirror rows MUST execute in one database transaction. Any mirror failure MUST roll back the metadata write.
- Response: `200 OK` with the full updated `ModelMetadataRecord`.
- Errors: `400 invalid_request` if `model_id` path param is empty.

### 3.4 Delete model metadata

- Method/Path: `DELETE /api/dashboard/model-metadata/{model_id}`
- Auth: admin required.
- Response: `200 OK` with `{ "success": true }`.
- Errors: `404 not_found` if record does not exist.
- The metadata delete and deletion of all generated billing-rate rows whose id starts with `model_metadata:{model_id}:` MUST execute in one database transaction.

### 3.5 Billing-rate CRUD

- Method/Path: `GET /api/dashboard/billing-rates`
- Auth: admin required.
- Optional query parameter `pricing_profile`: when present, the response contains only rows whose `pricing_profile` equals it; when absent, the response contains every row. Ordering is unchanged (`pricing_profile` ASC, `priority` DESC, `id` ASC).
- Response: `BillingRateRecord[]` (full records including `match_json` and `raw_json`).

- Method/Path: `GET /api/dashboard/billing-rates/profiles`
- Auth: admin required.
- Response: `{ "profiles": [ { "pricing_profile": string, "rate_count": number, "model_count": number, "has_models_dev": boolean } ] }` ordered by `pricing_profile` ASC. `rate_count` is the number of `billing_rate_records` rows carrying that profile; `model_count` is the number of distinct non-null `model_pattern` values carrying that profile; `has_models_dev` is true when at least one row of the profile has `source = 'models_dev'`. This endpoint exists so surfaces that need only profile names, counts, or source presence do not download the full rate catalog (measured 2.6 MB for 5096 rows).

- Method/Path: `PUT /api/dashboard/billing-rates/{id}`
- Auth: admin required.
- Body: any mutable fields from `billing_rate_records` except `id` and `updated_at`.
- Response: full updated `BillingRateRecord`.
- Errors: `400 invalid_request` if required fields are absent for a new row, `unit_price_nano` is not a non-negative integer string, or `unit_price_currency` is present and is neither `USD` nor `CNY`.
- A created row that omits `unit_price_currency` MUST use `CNY`; an updated row that omits it MUST keep its stored currency, per `metered-billing.spec.md` MB-A2c.

- Method/Path: `DELETE /api/dashboard/billing-rates/{id}`
- Auth: admin required.
- Response: `{ "success": true }`.
- Errors: `404 not_found` if row does not exist.

### 3.6 Billing-rate catalog sync

- Method/Path: `POST /api/dashboard/billing-rates/sync/catalog`
- Auth: admin required.
- Behavior: sync the bundled catalog as defined in `metered-billing.spec.md` MB-A4.
- Response: `{ "success": true, "upserted": number, "skipped": number, "deleted": number, "fetched_at": string }`.

### 3.7 Pricing-profile patterns

- Method/Path: `GET /api/dashboard/pricing-profile-patterns`
- Auth: admin required.
- Response: `{ "patterns": [{ "pattern": string, "pricing_profile": string }] }`.

- Method/Path: `PUT /api/dashboard/pricing-profile-patterns`
- Auth: admin required.
- Body: `{ "patterns": [{ "pattern": string, "pricing_profile": string }] }`.
- Behavior: replace the ordered pattern list.
- Errors: `400 invalid_request` if any `pattern` or `pricing_profile` is empty after trim.

## 4. Dashboard UI

### 4.1 Page location

UI1. Page MUST be accessible at `/dashboard/models`.

UI2. Sidebar navigation MUST include a "Models" entry between "Playground" and "Users" in the admin section.

### 4.2 Layout

UI3. Page MUST follow standard dashboard layout: `PageWrapper`, `text-3xl` heading, motion animations.

UI4. Page heading: "Model Database" (en) / "模型数据库" (zh).

UI4a. The page MUST be a single-page master-detail workbench (no tab bar):

- a toolbar: a pricing-profile selector (fed by `GET /api/dashboard/billing-rates/profiles`, each option showing its model count), a model search input, one sync dropdown (Sync models.dev / Sync catalog), a "Match rules" button, a "Manage profiles" button, and an add-price action (New model price), and a create-model-metadata action (New model metadata);
- a master list: one virtualized row per model of the selected profile, with the price editor as a right-hand sticky panel (a bottom sheet under `lg`);
- an "Advanced rates" entry that opens the existing low-level rate table inside a dialog.

UI4b. Every surface MUST use SWR for data loading, skeleton fallback while loading, and optimistic updates for user-triggered mutations.

### 4.3 Model Database tab: compact virtualized list

UI5. Default display MUST be a compact virtualized table (`TableVirtuoso`) with columns:

| Column | Content |
|--------|---------|
| Model | Provider icon (from `models_dev_provider`) + `model_id` (bare name) |
| Input | `input_cost_per_token_nano` under the row's `price_currency` symbol |
| Output | `output_cost_per_token_nano` under the row's `price_currency` symbol |
| Context | `max_tokens` formatted with `K` suffix |
| Source | Badge showing `models_dev` or `manual` |
| Updated | Relative timestamp |

UI5.1. In the Model column badge, GLM-series icon compatibility MUST follow `dashboard-ui-layout.spec.md` PL14.1.

UI6. Each row MUST be clickable to open an edit dialog.

UI7. Price display: `nano_per_token / 1000` = currency units per 1M tokens. Display up to 4
decimal places. The prefix MUST be `¥` when the row's `price_currency` is `CNY` and `$` when
it is `USD`. The page MUST NOT apply an exchange rate to a metadata price: the number shown
is the stored number, under its own currency.

UI7a. Model Database and Billing Profiles MUST keep nano-unit prices and per-million form values as decimal strings. Conversion, provider switching, form editing, validation, and API serialization MUST NOT pass a price through JavaScript `Number`, `parseFloat`, `toFixed`, or binary floating-point arithmetic.

UI7b. Converting a per-million input to a nano-unit price per token MUST compute `trunc(input * 1000)` with decimal-string arithmetic. A negative or syntactically invalid input MUST be blocked before the mutation request. For example, `1.001` MUST serialize as `"1001"` and round-trip back to `1.001`. This conversion is currency-independent: it applies to a CNY-per-million and to a USD-per-million input identically.

### 4.4 Search and filter

UI8. Page MUST include a search input that filters by `model_id` substring (client-side).

### 4.5 Edit dialog — provider source switcher

UI9. When `raw_json.providers` contains multiple entries, the edit dialog MUST show a provider selector listing available providers with their pricing. The dialog MUST fetch the record from `GET /api/dashboard/model-metadata/{model_id}` when it opens, and the provider selector MUST consume that detail response's `raw_json`. While the detail request is in flight the dialog body MUST show a skeleton fallback and MUST NOT render the provider selector from the list row.

UI10. Selecting a provider MUST auto-fill all pricing and limit fields from that provider's data in `raw_json.providers[provider]`.

UI11. The user MAY further edit the auto-filled values. Any save always sets `source = 'manual'`.

UI11a. The edit dialog MUST expose the row's `price_currency` as a `CNY`/`USD` control and
MUST send it with the save. A create dialog MUST preselect `CNY`. An edit dialog MUST
preselect the stored value. Because UI10 auto-fills from Models.dev USD data, selecting a
provider MUST also set the control to `USD`, so that auto-filled USD numbers are not saved
under a CNY label. The per-million values shown beside each provider option in the selector
MUST carry the `$` prefix, since `raw_json` prices are always USD.

### 4.6 Actions

UI12. Page header:
- "Sync Models.dev" button: triggers sync, shows loading, toast with upserted/skipped counts.
- "Add Model" button: opens create dialog.

UI13. Edit dialog MUST include a "Delete" action.

UI14. After any mutation, the model list MUST revalidate via SWR.

### 4.7 Loading state

UI15. Skeleton placeholders while loading.

### 4.8 Billing Profiles tab

UI17. The master list MUST group rate records by the selected `pricing_profile` and present a master-detail workbench. Data loading follows UI17c unchanged.

UI17c. The Billing Profiles tab MUST load the profile list and per-profile model counts from `GET /api/dashboard/billing-rates/profiles` and the selected profile's rate rows from `GET /api/dashboard/billing-rates?pricing_profile={selected}`. The tab MUST NOT request the unfiltered billing-rate catalog.

UI17a. Desktop (`lg` and above) MUST render the master list in the main column and the price editor in a sticky right panel (about one third of the width). The detail pane MUST show model ID plus input, cache-read, and output token prices formatted as the price per one million tokens, prefixed by the symbol of that row's `unit_price_currency`: `¥` for `CNY` and `$` for `USD`. A price MUST NOT be shown under a currency symbol that does not match its stored `unit_price_currency`. When the effective row has a non-null `peak_unit_price_nano`, that cell MUST show the off-peak price (`unit_price_nano`) and the peak price (`peak_unit_price_nano`) in that order, both converted as CNY-or-USD per one million tokens under the same currency symbol. When `peak_unit_price_nano` is null, the cell MUST show only the off-peak price.

UI17b. Mobile (`< lg`) MUST render a horizontally scrollable profile selector and stacked model-price rows. No pricing table may require horizontal page scrolling.

UI18. Billing Profiles MUST provide model search and source/status filters without exposing nano-unit prices or raw JSON in the primary flow.

UI19. The page MUST provide:

- a `Sync models.dev` action that calls `POST /api/dashboard/model-metadata/sync/models-dev`;
- a visible last-sync/source status derived from synchronized records;
- an ordered match-rule editor backed by `GET/PUT /api/dashboard/pricing-profile-patterns`;
- a manual-override action that creates or updates manual `billing_rate_records` using human-readable CNY-per-million inputs.

UI19c. The manual-override dialog MUST offer a currency selector with exactly the values `CNY` and `USD`, defaulting to `CNY` for a model with no existing manual rates. Every price input MUST carry the symbol of the selected currency (`¥` for `CNY`, `$` for `USD`), and the dialog MUST state the selected currency per one million tokens. Every rate it writes MUST send the `unit_price_currency` the selector shows; the typed digits are interpreted directly in that currency and the dialog MUST NOT apply an exchange rate to them. When the model already has manual rates in exactly one currency, opening the dialog MUST default the selector to that currency and prefill from those rates; when manual rates exist in both currencies or in none, the selector defaults to `CNY` and prefill follows the selected currency only.

UI19d. The manual-override dialog MUST expose, for each of Input, Cache read, Cache write 5m, Cache write 1h, and Output, an off-peak field (`unit_price_nano`) and an optional peak field (`peak_unit_price_nano`). All fields are per one million tokens in the currency selected per UI19c and MUST use the same decimal-string conversion as UI7b. A blank peak field MUST send `peak_unit_price_nano = null` (MB-A8: clear the peak so the row always bills at the off-peak price). A filled peak field MUST send the converted nano-unit string. The dialog MUST state that the peak field applies only during the Beijing weekday peak window defined by `metered-billing.spec.md` MB-R14 (`09:00–12:00` and `14:00–18:00` Monday through Friday, UTC+08:00). A blank cache-write 5m or cache-write 1h field MUST delete an existing manual override row for that usage class and write nothing when none exists; a filled field MUST write a rate row with `usage_class = "cache_write_5m"` or `"cache_write_1h"`, `cache_ttl = "5m"` or `"1h"` respectively. Prefill MUST follow UI19c: only rates denominated in the selected currency prefill; a rate in the other currency leaves the field empty so digits are never reinterpreted across currencies. Switching the selector MUST re-prefill every field from the rates of the newly selected currency.

UI19a. When metadata and billing rates have finished loading and no `models_dev` records exist, the UI MUST trigger at most one automatic models.dev sync for that mounted page instance. A failed automatic sync MUST show a retry action and MUST NOT loop.

UI19b. A successful models.dev sync MUST revalidate both model metadata and billing-rate SWR resources in the same interaction.

UI20. Manual overrides MUST be visually separated from synchronized rates. Manual rows take precedence through the existing rate priority and source semantics; deleting a manual override MUST reveal the synchronized value after SWR revalidation.

UI20a. Billing Profiles MUST provide a rename action on each listed model. The action MUST call `POST /api/dashboard/billing-rates/profiles/{profile}/models/{model}/rename` with body `{ "target_model": <string> }`. The model name MUST NOT be presented as a fixed value.

UI20b. The rename dialog MUST prefill the current model name, MUST reject an empty or unchanged target client-side, and MUST revalidate the billing-rate SWR resource on success.

UI20c. When the renamed model retains synchronized (`model_metadata:`) rows under its former name, the UI MUST state that the former name keeps its synchronized prices, because a synchronized row is owned by the model registry rather than by the profile (PP-R1c).

### 4.9 Advanced Rates tab

UI21. The Advanced Rates tab MUST list `billing_rate_records` with every low-level mutable field, including the nano-unit off-peak price, the optional nano-unit peak price, its currency, and JSON match fields. The price column MUST render `unit_price_nano`, and when `peak_unit_price_nano` is non-null MUST also render that peak value, then `unit_price_currency` and `unit`.

UI22. Advanced Rates MUST provide catalog sync, search, add, edit, and delete actions.

UI23. The low-level rate edit dialog MUST allow editing every mutable field exposed by the Billing-rate CRUD API, including `unit_price_currency` as a choice between `CNY` and `USD`. A new row MUST default that control to `CNY`. The off-peak nano-unit field (`unit_price_nano`) and the optional peak nano-unit field (`peak_unit_price_nano`) MUST be labeled as off-peak and peak. The peak field MUST state that it applies only during the Beijing weekday window of `metered-billing.spec.md` MB-R14, and a blank peak field MUST send `peak_unit_price_nano = null`. JSON fields MUST be edited as JSON text and rejected client-side when not valid JSON.

UI24. Empty pricing-profile match-rule `pattern` or `pricing_profile` values MUST be blocked before submitting.

UI24a. The Provider edit dialog MUST source its pricing-profile name list from `GET /api/dashboard/billing-rates/profiles`. The dialog MUST NOT request the full billing-rate catalog to derive profile names.

### 4.10 Profile management

UI25. The "Manage profiles" dialog MUST list every pricing profile (name, model count, `has_models_dev` mark) with copy and delete actions per row.

- Delete MUST call `DELETE /api/dashboard/billing-rates/profiles/{profile}` behind an AlertDialog confirmation that states the number of rate rows to be removed.
- A 409 `pricing_profile_in_use_patterns` or `pricing_profile_in_use_providers` response MUST render the server message inline in the dialog and keep the row listed.
- A successful delete MUST remove the profile from the selector and revalidate the billing-rate SWR resources; when the deleted profile was selected, the selector MUST move to the first remaining profile.

### 4.11 Sync result report

UI26. After a models.dev sync completes, the page MUST show a result report with the counts `upserted`, `deleted`, and `retained_manual` (models whose manual metadata kept them out of the sync), each as a labeled number. The report MUST offer a detail action that lists the retained model ids. The catalog sync MUST show its existing upserted/skipped/deleted counts in the same report surface.

### 4.12 Model limits inside the price editor

UI28. The price editor panel MUST include a model-limits card with three fields —
`max_input_tokens`, `max_output_tokens`, `max_tokens` — each a non-negative integer or
blank. The card prefills from the selected model's `model_metadata_records` row when one
exists and renders blank otherwise. Saving the card MUST upsert the model's metadata
preserving every stored price field (an omitted field keeps its stored value server-side),
and revalidate the model-metadata SWR resource. Invalid input (non-integer or negative)
MUST be rejected client-side before the request. The card MUST be available in create mode
too, so limits can be set together with a new price without visiting the metadata editor.

### 4.13 Adding a model price directly

UI27. The workbench MUST provide an "Add price" toolbar action. It MUST open a dialog that
asks for a model name (required, non-empty after trimming) and then loads the price editor
for that name inside the selected profile with every lane empty (create mode). The dialog
MUST state that the profile is not tied to one channel — the created rows apply to every
Provider that routes the model through the selected profile. Saving writes the same manual
rows as the regular editor (including `provider_type = null` so the price applies to every
protocol), and after the first save the editor MUST switch to editing the now-existing
model in place. The model name MUST NOT require a registry metadata record to exist; the
master list MUST show the model as soon as its rate rows exist.

### 4.8 Billing integration note

UI16. Billing resolves a normalized pricing key for `upstream_model` first. Pricing-key normalization MUST strip at most one recognized reasoning-tier suffix using the suffix rules in § 8. If a redirected normalized `upstream_model` key has no complete rates, billing MUST retry the lookup with the normalized logical-model key. Both lookups query `billing_rate_records` through the selected pricing profile.

## 5. Invariants

INV1. `source = 'manual'` whenever created or updated via PUT endpoint.

INV2. Sync MUST NOT modify records where `source = 'manual'`.

INV3. `model_id` is the primary key, bare model API name, MUST be unique.

INV4. Price fields are nullable. Billing **blocks** a request only when neither the normalized `upstream_model` pricing key nor the normalized redirected source logical-model pricing key resolves to complete input/output pricing.

## 6. Billing Enforcement

BE1. `build_monoize_attempts()` MUST filter out an attempt when both of the following are true:

- the normalized pricing key of `upstream_model` has no complete eligible rate matrix in `billing_rate_records`, and
- the normalized pricing key of the request logical model also has no complete eligible rate matrix in `billing_rate_records`.

BE2. If ALL attempts for a request are filtered out due to missing pricing, the system MUST return HTTP 403 with error code `model_pricing_required` and a message listing the blocked model name(s).

BE3. `maybe_charge_response()` MUST return an error (HTTP 403 `model_pricing_required`) only if both the normalized `upstream_model` pricing-key lookup and the normalized logical-model fallback lookup fail. This is a defense-in-depth check — BE1 should already prevent this path from being reached.

BE4. The Provider dashboard page MUST display a visible warning badge on any `ProviderCard` whose models include entries with no complete eligible rate matrix in `billing_rate_records`. The badge MUST show the count of unpriced models.

- `GET /api/dashboard/providers` MUST include `unpriced_model_count`, `unpriced_model_ids`, and `model_runtime_statuses` for each provider. `unpriced_model_ids` MUST contain exactly the logical model ids counted by `unpriced_model_count`, sorted ascending. Each `model_runtime_statuses` entry MUST expose its `pricing_status` and the Channel id and name for every unpriced mapping as defined by `channel-management.spec.md`.
- For a redirected entry, the card MUST treat the model as priced when either the normalized pricing key of the `redirect` model or the normalized pricing key of the logical model has complete rates. It MUST count the entry as unpriced only when both are missing/incomplete.

BE5. The billing enforcement check uses a per-request cache to avoid redundant pricing lookups. Because redirect fallback and suffix normalization depend on both `upstream_model` and logical model after pricing-key normalization, the cache key MUST include both values or an equivalent composite identity.

## 7. Model ID Normalization

NID1. **Canonical form**: `model_id` MUST be normalized in this order:
  1. Take the last segment after splitting on `/`.
  2. Optionally strip a provider prefix in either `provider--model` or `provider.model` form, but ONLY when `provider` is a known provider identifier.
  3. Lowercase the result.
  - `openai/gpt-4o` → `gpt-4o`
  - `accounts/fireworks/models/llama-v3p1-405b-instruct` → `llama-v3p1-405b-instruct`
  - `anthropic--claude-4.5-opus` → `claude-4.5-opus`
  - `xxxxx/anthropic.claude-opus-4.6` → `claude-opus-4.6`
  - `flux.1-dev` → `flux.1-dev` (no known provider prefix; preserve)
  - `GPT-4o` → `gpt-4o`
  - `claude-sonnet-4-20250514` → `claude-sonnet-4-20250514` (no `/`, unchanged except lowercase)

NID2. Normalization MUST be applied:
  - During `sync_from_models_dev`, when grouping variants by model name.
  - During migration on startup (existing records with `/` in `model_id`).

NID3. When normalization produces duplicate `model_id` values, the most recently updated record wins.

NID4. Dashboard CRUD routes for model metadata MUST use Axum wildcard `{*model_id}` to support model IDs that may contain `/` (e.g. user-created records). The handler MUST strip a leading `/` from the captured path if present.

## 8. Suffix-Based Reasoning Effort Resolution

### 8.1 Reasoning effort value domain

RE1. Valid `reasoning_effort` values: `none`, `minimum`, `low`, `medium`, `high`, `xhigh`, `max`. `xhigh` and `max` are two distinct effort levels and MUST NOT be aliased to each other.

RE2. The built-in suffix table maps each `-<effort>` suffix to its own identical effort string (e.g. `-max -> max`, `-xhigh -> xhigh`). Monoize MUST NOT collapse `-max` to `xhigh` at suffix-resolution time.

### 8.2 Global suffix → effort mapping

RE3. A global setting `reasoning_suffix_map` stores a JSON object mapping string suffixes to reasoning effort values.

Default value:
```json
{
  "-thinking": "high",
  "-reasoning": "high",
  "-nothinking": "none"
}
```

RE4. Suffixes are matched **longest-first** against the end of the model name.

RE5. The setting is stored in `system_settings` table under key `reasoning_suffix_map` and exposed via the existing `GET/PUT /api/dashboard/settings` endpoints.

RE5a. Startup and every successful settings mutation MUST publish `reasoning_suffix_map` into the process runtime snapshot. Forwarding suffix resolution MUST read that snapshot and MUST NOT query `system_settings` per request.

RE6. The setting is editable in the dashboard Settings page.

RE6a. The default provider-level suffix transform used for Anthropic/OpenRouter compatibility SHOULD map wildcard `*` to `-thinking` (not `-{effort}`), so suffix resolution keeps model IDs on supported aliases.

### 8.3 Model resolution algorithm

RE7. When `collect_provider_attempts` looks up `urp.model` in each `channel.models`:
  1. **Exact match**: If `channel.models` contains `urp.model`, use it directly. No suffix processing.
  2. **Suffix resolution**: If no exact match, iterate `reasoning_suffix_map` entries (longest suffix first). For each suffix, check if `urp.model` ends with that suffix. If yes:
     - `base_model = urp.model` with the suffix removed.
     - Look up `base_model` in `channel.models`.
     - If found, use that Channel model entry AND set `reasoning_effort` to the mapped value.
  3. **No match**: If neither exact nor suffix match, skip this Channel.

RE8. When a suffix match resolves to a base model, the resolved `reasoning_effort` value MUST be injected into the URP request's `reasoning.effort` field (typed flow) before the request is encoded for the upstream provider. If the user already specified `reasoning_effort` explicitly in the request body, the explicit value takes precedence over the suffix-derived value.

RE9. Billing and any other model-pricing identification path use the **base model**'s pricing from `model_metadata_records`. When a model ID ends with a recognized reasoning-tier suffix, Monoize MUST strip that suffix (longest suffix first, at most one suffix removed) before metadata lookup. The suffix model itself does not need a separate pricing entry.

### 8.4 Billing: reasoning token fallback

RE10. In `calculate_charge_nano`, when `reasoning_tokens > 0` and `output_cost_per_reasoning_token_nano` is `None`, the system MUST fall back to `output_cost_per_token_nano` for reasoning tokens (i.e. charge all completion tokens at the output rate).

This is already the existing behavior (the `else` branch charges `completion_tokens * output_cost_per_token_nano` which includes reasoning tokens). No change needed.

## 9. Migration

MIG1. On startup, existing records with `model_id` containing `/` (e.g. `openai/gpt-4o`) MUST be migrated to bare name via NID1 normalization. When duplicates arise after stripping, keep the most recently updated record.

## PostgreSQL Read and Failure Contract

Profile summary flags MUST have a consistent signed integer SQL projection.
The workbench MUST display a retryable error when metadata, profiles, or selected
profile rates fail to load. It MUST NOT display the no-models state for a failed
request. Retry MUST only refetch data; it MUST NOT synchronize or mutate pricing.

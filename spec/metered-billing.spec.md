# Metered Billing Specification

## 0A. LynShen Provider pricing

MB-MIG-1. After the Provider pricing migration, runtime Profile selection and exact
multiplier resolution MUST follow `provider-pricing.spec.md` PP-E1 through PP-E5. Global
Profile patterns and model metadata MUST NOT provide a runtime fallback.

MB-MIG-2. Billing MUST sum checked integer nano-USD token and meter line items, multiply
the aggregate by the selected exact decimal multiplier once, and truncate toward zero
once at final scaling. It MUST NOT multiply or truncate individual line items.

MB-MIG-3. An unpriced selected mapping MUST be excluded before upstream dispatch. If all
structurally eligible mappings are unpriced, the request MUST return HTTP `503` code
`model_pricing_required` and MUST create no upstream call or charge.

MB-MIG-4. Each committed relevant billing-rate write MUST invalidate the public
Marketplace snapshot through `model-marketplace.spec.md` MM-G1 through MM-G13.

## 0. Status

- Product name: Monoize.
- Scope:
  - billing-rate matrix storage;
  - pricing-profile selection;
  - token, cache, context-tier, modality, and server-native-meter charging;
  - dashboard APIs for billing-rate administration.

## 1. Data Model

MB-D1. Billing rates MUST be stored in table `billing_rate_records`.

MB-D2. `billing_rate_records` MUST contain these columns:

- `id: TEXT PRIMARY KEY`
- `source: TEXT`
- `pricing_profile: TEXT`
- `model_pattern: TEXT NULL`
- `provider_type: TEXT NULL`
- `rate_kind: TEXT`
- `usage_class: TEXT`
- `unit: TEXT`
- `unit_price_nano: TEXT`
- `unit_price_currency: TEXT`
- `peak_unit_price_nano: TEXT NULL`
- `context_tier: TEXT NULL`
- `service_tier: TEXT NULL`
- `modality: TEXT NULL`
- `cache_ttl: TEXT NULL`
- `match_json: TEXT`
- `priority: INTEGER`
- `enabled: INTEGER`
- `raw_json: TEXT`
- `updated_at: TEXT`

MB-D3. `unit_price_nano` MUST be an integer string denominated in nano-units of `unit_price_currency` per one `unit`. One nano-unit is `10^-9` of one major unit of that currency.

MB-D3a. `unit_price_nano` MUST be non-negative and representable as `i128`. Create, update, sync, and metadata-mirror paths MUST reject a negative or malformed rate before persistence.

MB-D3b. `unit_price_currency` MUST be exactly `USD` or `CNY`. The database MUST enforce this domain with a `CHECK` constraint. A create or update path MUST reject any other value with `400 invalid_request`.

MB-D3g. `peak_unit_price_nano` is the optional high-demand price of the row. When it is
non-null it MUST obey the MB-D3a canonical non-negative `i128` rule and MUST be
denominated in the same `unit_price_currency` per the same one `unit` as
`unit_price_nano`. A NULL `peak_unit_price_nano` means the row has no peak price
and always bills at `unit_price_nano`. Create and update paths MUST reject a
negative or malformed non-null value with `400 invalid_request`, and MUST treat
an empty string as NULL. Catalog sync and metadata-mirror rows copy the value
unchanged when their source carries one and store NULL otherwise.

MB-D3c. Write-path currency defaults are:

- A rate written by Models.dev sync (`source = "models_dev"`) MUST set `unit_price_currency = "USD"`, because Models.dev publishes list prices in USD.
- A rate written by bundled-catalog sync (`source = "catalog"`) MUST set `unit_price_currency = "USD"`, because `billing-rates.catalog.json` stores USD list prices in its `unit_price_nano_usd` field.
- A rate written through the dashboard billing-rate API or the billing-profile editor MUST default to `unit_price_currency = "CNY"` when the request omits the field. The dashboard price form is denominated in CNY per 1,000,000 tokens.
- An update that omits `unit_price_currency` for an existing row MUST preserve the stored currency. An omitted field MUST NOT re-denominate a stored price.

MB-D3d. A rate row mirrored from `model_metadata_records` has `id` of the form `model_metadata:{model_id}:{usage_class}` and MUST use the `price_currency` of the metadata row it mirrors, as required by `model-metadata-dashboard.spec.md` MD8a. The mirror copies the price digits unchanged, so it MUST copy the denomination with them. A metadata row synced from Models.dev is USD and mirrors as USD; a manually priced metadata row is CNY by default and mirrors as CNY.

MB-D3e. Migration `m20260909_000066_billing_rate_currency` MUST rename `unit_price_nano_usd` to `unit_price_nano`, add `unit_price_currency` with the MB-D3b `CHECK` constraint and default `USD`, and then set `unit_price_currency = "CNY"` for exactly the rows where `source = "manual"` and `id` does not start with `model_metadata:`. It MUST NOT change any `unit_price_nano` value and MUST NOT apply an exchange rate: a pre-migration manual price of `9000` MUST become `9000` CNY, not a converted value. The migration MUST preserve the `idx_billing_rate_records_lookup` index of migration `m20260619_000019_billing_rate_records`.

MB-D3f. Migration `m20260909_000067_model_metadata_price_currency` MUST then relabel the
mirrored rows that MB-D3e deliberately left as `USD`, setting each to the `price_currency`
of its metadata row per `model-metadata-dashboard.spec.md` MD10a. MB-D3e excluded them
because the metadata layer carried no currency at that time; MD4 now denominates each
metadata row explicitly, so the mirror follows it. This migration MUST NOT change any
`unit_price_nano` value and MUST NOT apply an exchange rate.

MB-D4. `match_json` and `raw_json` MUST be JSON object strings. Decoding a persisted value that is malformed JSON or is not a JSON object MUST return a storage error that identifies the billing-rate row and column. A get, list, or matching-rate query MUST propagate that error; it MUST NOT replace the value with `{}`, omit the row, or treat the row as an unconditional rate. Create, update, and catalog-sync paths MUST reject an explicit non-object value before persistence. An omitted value MAY default to `{}` before persistence.

MB-D4a. Migration `m20260809_000030_normalize_billing_json_nulls` MUST evaluate `billing_rate_records.match_json` and `billing_rate_records.raw_json` independently. For each column, it MUST replace the stored value with `{}` if and only if removing leading and trailing JSON whitespace (`U+0009`, `U+000A`, `U+000D`, and `U+0020`) yields the exact string `null`. It MUST leave every other value unchanged, including malformed JSON, arrays, objects, strings, booleans, numbers, and values containing non-JSON whitespace. SQLite and PostgreSQL MUST apply these predicates identically. The down migration MUST be a no-op because the original whitespace cannot be reconstructed. Runtime decoding MUST continue to satisfy MB-D4 after this migration.

MB-D5. `model_metadata_records` MUST continue to store model capabilities, limits, Models.dev raw data, and legacy token prices. Billing computation MUST read `billing_rate_records`. Metadata writes and Models.dev sync MAY mirror token prices into `billing_rate_records`.

## 2. Pricing Profiles

MB-P1. System setting `pricing_profile_model_patterns` MUST store an ordered array of objects:

```json
[{ "pattern": "gpt-*", "pricing_profile": "openai" }]
```

MB-P1a. The default `pricing_profile_model_patterns` value MUST be exactly:

```json
[
  { "pattern": "gpt-image-*", "pricing_profile": "openai" },
  { "pattern": "text-embedding-*", "pricing_profile": "openai" },
  { "pattern": "gpt-*", "pricing_profile": "openai" },
  { "pattern": "o*", "pricing_profile": "openai" },
  { "pattern": "claude-*", "pricing_profile": "anthropic" },
  { "pattern": "gemini-*", "pricing_profile": "google" },
  { "pattern": "grok-*", "pricing_profile": "xai" },
  { "pattern": "*", "pricing_profile": "default" }
]
```

MB-P1b. The profile name `default` denotes the fallback pricing profile for model names that do not match a more specific provider profile rule. It MUST NOT imply legacy billing behavior.

MB-P2. Pattern matching MUST use case-insensitive glob semantics with `*` matching zero or more characters and `?` matching exactly one character.

MB-P2a. Pattern matching MUST compare ASCII bytes without recursion. It MUST use `O(1)` call-stack space and `O(1)` auxiliary space for every pattern and value length. Its worst-case comparison count MUST be `O(pattern_bytes * value_bytes)`. A pattern or value with at least `200000` bytes and a pattern containing multiple `*` operators MUST not cause call-stack growth proportional to input length.

MB-P3. After the Provider pricing migration, pattern matching is an administrative suggestion only. Runtime pricing MUST use the effective Profile from `provider-pricing.spec.md` section 7.

MB-P4. A mapping without an effective Profile has no billable pricing.

MB-P5. Migration `m20260619_000020_default_pricing_profile` MUST rename stored pricing profile value `legacy` to `default` in `billing_rate_records.pricing_profile` and in the `pricing_profile_model_patterns` system setting. Runtime pricing selection MUST NOT treat `legacy` as an alias for `default`.

MB-P6. Runtime pricing MUST NOT use `model_metadata_records.models_dev_provider` as a fallback Profile after the Provider pricing migration.

MB-P7. Rate rows for all effective Profiles in one request MUST be loaded in one set-based database query.

MB-P8. Provider-attempt preflight MUST attach the complete selected rate-matrix snapshot to the attempt. Settlement of that attempt MUST reuse the attached snapshot and MUST NOT repeat profile selection, metadata lookup, or rate-row lookup.

MB-P9. One active-probe scheduler tick MUST read the reasoning-suffix map once. It MUST use Provider-model effective Profiles and bulk-read candidate Billing Rate rows at most once for all probe candidate model, Provider-type, and Profile tuples in that tick.

MB-P10. Active-probe pricing resolution MUST use the mapping's effective Profile. Within that Profile it MUST preserve Billing Rate priority order, Provider-type matching, model-pattern matching, and dimensionless token-rate selection.

MB-P11. Each active-probe request-log task MUST receive its pricing resolution from the scheduler-tick snapshot. The task MUST NOT query Settings, model metadata, or Billing Rates.

MB-P12. One forwarding request MUST clone the reasoning-suffix map once before pricing its eligible attempts. It MUST execute no `system_settings` or model-metadata query for Profile selection. It MUST bulk-read candidate Billing Rate rows at most once for all distinct normalized model, effective Provider-type, and effective Profile tuples in that request. Pricing resolution for each attempt MUST then use only this request-local snapshot. The runtime MUST NOT execute pricing queries per attempt, Provider, or Channel.

## 3. Rate Selection

MB-R1. A rate row is eligible for a request only when all of these predicates are true:

- `enabled = 1`;
- `pricing_profile` equals the selected pricing profile;
- `provider_type` is null or equals the effective upstream provider type;
- `model_pattern` is null or matches the normalized pricing model key using MB-P2.

MB-R2. Eligible rows MUST be ordered by `priority DESC, id ASC`. The first matching row for a class and dimension set is the applied row.

MB-R3. `rate_kind = "token"` rows charge token quantities. `rate_kind = "meter"` rows charge non-token quantities.

MB-R4. `usage_class` for token rows MUST support at least:

- `input_uncached`
- `input_cached`
- `cache_write_5m`
- `cache_write_1h`
- `cache_read`
- `output`
- `reasoning_output`

MB-R5. The context tier domain is `default`, `short`, `long`.

MB-R6. If any eligible row for a pricing model has `context_tier` other than null or `default`, then the matrix MUST provide either:

- an authoritative upstream usage/service field that selects the tier, or
- `match_json.context_threshold_tokens` as an integer threshold.

MB-R7. If a tiered matrix has no deterministic tier selector under MB-R6, preflight MUST reject the request with HTTP `403` and code `model_pricing_required`.

MB-R8. For a context-tiered matrix, every non-default context tier present for a requested token class MUST have a matching rate for that token class. Missing tier rows MUST reject with HTTP `403` and code `model_pricing_required`.

MB-R9. Preflight MUST parse `unit_price_nano` for every candidate row as a canonical non-negative `i128` string. One malformed, non-canonical, or negative candidate row MUST make the matrix incomplete.

MB-R9a. When at least one candidate row has `unit_price_currency = "CNY"` and no exchange-rate snapshot with a positive `cny_per_usd` exists, the matrix MUST be incomplete. The mapping MUST then be excluded before upstream dispatch under MB-MIG-3, and a request whose every eligible mapping is excluded for this reason MUST return HTTP `503` and code `model_pricing_required`. A CNY-basis rate MUST NOT be settled at its USD magnitude, and settlement MUST NOT be the first place this condition is detected.

MB-R10. A complete matrix MUST contain dimensionless fallback rows for `input_uncached` and `output`. A dimensionless fallback row has `modality = null`, `cache_ttl = null`, and `service_tier` equal to null or `default`. For a non-tiered matrix, its `context_tier` MUST equal null or `default`. For a context-tiered matrix, each tier required by MB-R8 MUST contain such a fallback row. Preflight MUST reject a matrix that lacks one of these rows.

MB-R11. Settlement MUST select the service tier from the non-empty `service_tier` field in the actual upstream response envelope. For a Responses stream, the field is `response.service_tier`. For a Chat Completions stream, the field is the top-level `service_tier`. For a Messages stream, the field is `message.service_tier`. A request field or `Usage.extra_body.service_tier` MUST NOT select the settled service tier.

MB-R12. If the settled service tier is absent or equals `default`, a rate row with `service_tier` equal to null or `default` is eligible. If the settled service tier has any other value, only a rate row with the same non-null `service_tier` is eligible. A null or `default` row MUST NOT act as a fallback for a different settled service tier.

MB-R13. For a non-default settled service tier, the matrix MUST contain matching dimensionless `input_uncached` and `output` token rows. It MUST also contain a matching meter row for each requested server-native usage class. If one of these rows is absent, settlement MUST fail with HTTP `403` and code `model_pricing_required`.

## 4. Token Billing

MB-T1. Token quantities MUST be read from normalized upstream `Usage`. Monoize MUST NOT estimate token quantities when upstream usage is available.

MB-T1a. `Usage.input_tokens` and `Usage.output_tokens` MUST be inclusive totals before billing starts. If a provider reports tool-result prompt tokens or reasoning tokens as disjoint counters, its decoder MUST add those counters to the corresponding total with checked integer addition while retaining the counters in `input_details` or `output_details`. Billing MUST NOT add a retained detail counter to an inclusive total a second time.

MB-T2. `Usage.input_tokens` is the aggregate prompt total. The uncached input quantity is:

```text
input_uncached = input_tokens - cache_read_tokens - cache_creation_tokens
```

with saturation at zero.

MB-T3. `cache_read_tokens` MUST charge against `usage_class = "cache_read"` when the quantity is non-zero. A rate row with `usage_class = "input_cached"` is an accepted alias for the same quantity.

MB-T3a. When eligible rows for cached input have non-null `modality` and `Usage.input_details.cache_read_modality_breakdown` is present, billing MUST use that breakdown. Billing MUST NOT derive the cached modality split from aggregate `cache_read_tokens` or from total input modality counts. When the breakdown is absent, billing MUST use the first matching dimensionless `cache_read` or `input_cached` row; if neither exists, billing MUST use the dimensionless `input_uncached` fallback row.

MB-T4. `cache_creation_5m_tokens` MUST charge against `usage_class = "cache_write_5m"` and `cache_ttl = "5m"` when the quantity is non-zero.

MB-T5. `cache_creation_1h_tokens` MUST charge against `usage_class = "cache_write_1h"` and `cache_ttl = "1h"` when the quantity is non-zero.

MB-T6. If `cache_creation_tokens > 0`, both 5-minute and 1-hour cache-write rates are eligible, and `cache_creation_5m_tokens = cache_creation_1h_tokens = 0`, billing MUST reject with HTTP `403` and code `model_pricing_required`. Monoize MUST NOT split aggregate cache-creation usage between 5-minute and 1-hour buckets.

MB-T6a. For each cache-read or cache-write bucket whose selected dimensions have no matching specialized rate, billing MUST charge that bucket with the matching dimensionless `input_uncached` fallback rate. An unsplit aggregate cache-write bucket for which MB-T6 does not select a TTL-specific rate MUST be charged once with that fallback rate.

MB-T7. Output tokens excluding reasoning tokens MUST charge against `usage_class = "output"`.

MB-T8. Reasoning output tokens MUST charge against `usage_class = "reasoning_output"` when the quantity is non-zero and a matching rate exists. If no matching reasoning rate exists, those tokens MUST be included in the base output bucket.

MB-T9. When eligible rows for a token class have non-null `modality` and a modality breakdown is present, billing MUST charge each non-zero modality quantity using its matching modality row. The modality quantities used for that token class MUST sum exactly to the quantity billed for that token class. When the breakdown is absent, billing MUST charge the aggregate quantity with the first matching dimensionless row.

MB-T10. For `gpt-image-2`, Monoize MUST bill text/image input tokens, cached input tokens, and image output tokens from upstream usage. Monoize MUST add an output-item fixed fee only when `billing_rate_records` contains an enabled meter row for that fee.

## 5. Meter Billing

MB-M1. Server-native tool meter charges MUST be based only on:

- authoritative provider usage counters in `Usage.extra_body`, or
- decoded native provider events represented in URP output nodes.

MB-M2. Monoize MUST NOT charge a server-native tool from local wall-clock measurement.

MB-M3. Duration, session, and billed-minute meters MUST require an authoritative upstream billed quantity. If the request enabled such a meter class and upstream usage does not provide the billed quantity, billing MUST reject with HTTP `403` and code `model_pricing_required`.

MB-M4. Call-count meters MAY use decoded native provider events when no authoritative provider usage counter exists.

MB-M5. If a request enables a server-native tool and no eligible meter rate exists for its `usage_class`, preflight MUST admit the request and settlement MUST charge that `usage_class` at unit price `0`. An absent meter rate MUST NOT reject the request, MUST NOT exclude the provider mapping from routing, and MUST NOT contribute to `model_pricing_required`. An eligible meter rate whose `unit_price_nano` is `0` settles identically to an absent one.

Rationale: a client may enable a server-native tool on every request without the operator's knowledge. The Codex client enables `web_search` by default, so a rejecting gate makes every request fail with `model_pricing_required` before request-log admission, which presents to the operator as a gateway that serves no traffic and records no reason.

MB-M5a. When settlement charges a `usage_class` at unit price `0` under MB-M5 because no eligible meter rate exists, Monoize MUST emit one observability log entry per request naming each such `usage_class`, and MUST mark each resulting meter line item with `unpriced = true`. The line item MUST carry the observed billable quantity, so the unbilled volume is recoverable from the request log.

MB-M5b. A meter line item settled from an eligible rate MUST NOT carry `unpriced = true`, including when that rate's `unit_price_nano` is `0`. The marker distinguishes "the operator has not priced this class" from "the operator priced this class at zero".

MB-M6. For each meter `usage_class`, billing MUST apply only the first eligible row under MB-R2 whose context tier, service tier, modality, and cache-TTL dimensions match the settled usage. A lower-priority duplicate row for the same class and selected dimensions MUST NOT create another line item.

MB-M7. Pass-through streaming MUST retain the decoded terminal URP output nodes until settlement. When authoritative provider meter usage is absent, MB-M4 MUST count call meters from those retained nodes using the same rule as non-stream settlement.

## 6. Charge Formula

MB-R14. Peak window: a request is in the peak window if and only if its
billing-rate resolution snapshot was built while the wall clock, converted to
Beijing time (UTC+08:00, no daylight saving), falls on a Monday through Friday
and within either `09:00 <= time < 12:00` or `14:00 <= time < 18:00`. Every
other instant is the off-peak window. The determination is made exactly once
per request, when the `BillingRateResolution` snapshot is built (MB-P8), and
MUST be carried on the resolution so that preflight holds and settlement use
the same determination. A snapshot rebuilt lazily at settlement time re-evaluates
the predicate at rebuild time.

MB-R15. Effective unit price: when the resolution's peak-window determination
is true and the applied rate row has a non-null `peak_unit_price_nano`, every
charge computation for that row MUST use `peak_unit_price_nano` as the unit
price; otherwise it MUST use `unit_price_nano`. Rate selection (MB-R1, MB-R2)
is unchanged: the peak window never changes which row is applied, only the
price read from the applied row. The preflight matrix validation (MB-R9) MUST
also verify a non-null `peak_unit_price_nano` obeys the MB-D3a canonical
non-negative rule; a malformed value makes the matrix incomplete exactly as a
malformed `unit_price_nano` does.

MB-C1. Base charge is:

```text
base_charge = sum(token_line_items.charge_nano) + sum(meter_line_items.charge_nano)
```

Every `charge_nano` is denominated in nano-USD, because wallet balances, holds, and ledger entries are nano-USD.

MB-C1a. For a rate row with `unit_price_currency = "USD"`, the line charge is:

```text
charge_nano = quantity * unit_price_nano
```

For a rate row with `unit_price_currency = "CNY"`, the line charge is:

```text
charge_nano = round_half_away_from_zero((quantity * unit_price_nano) / cny_per_usd)
```

The division MUST be applied to the product, not to `unit_price_nano`. Dividing the unit price first quantises a small rate: at `cny_per_usd = 7.1`, a `unit_price_nano` of `10` would round to `1`, a 29 percent error. Applying the division after multiplication bounds the error of one line item at half a nano-USD. The arithmetic MUST use exact decimal or checked integer operations and MUST NOT pass through `f32` or `f64`.

MB-C1b. `cny_per_usd` is the value of one exchange-rate snapshot read at most once per forwarding request, before pricing any attempt. Every line item of one request MUST use that single value, so the same snapshot applies no matter how long the request ran. If the snapshot is absent or its `cny_per_usd` is not a positive decimal, a CNY-basis line item MUST fail with a billing error and MUST NOT be charged at its USD magnitude.

MB-C1c. A reserved maximum charge under a hold MUST be computed as the maximum over per-rate charges already normalized to nano-USD by MB-C1a. It MUST NOT be computed as the maximum over raw `unit_price_nano` values, because a CNY-basis price and a USD-basis price are not comparable before normalization.

MB-C1d. Peak charge: when MB-R15 selects `peak_unit_price_nano` for an applied
row, the MB-C1a formulas apply unchanged with `peak_unit_price_nano` in place
of `unit_price_nano`. Each line item billed at the peak price MUST record
`pricing_window: "peak"`, and each line item billed at the off-peak price MUST
record `pricing_window: "off_peak"`, in addition to the MB-C4a fields, which
record the price actually applied so the breakdown stays recomputable without
reading `billing_rate_records`.

MB-C2. Final charge is:

```text
final_charge = trunc(base_charge * provider_multiplier)
```

`provider_multiplier` MUST be an exact positive decimal sourced from a decimal string. Multiplication MUST use checked decimal/integer arithmetic and truncate toward zero without conversion through `f32` or `f64`.

MB-C3. If any required rate is missing, the request MUST be rejected for all roles, including `admin` and `super_admin`.

MB-C4. A successful billing snapshot MUST persist `billing_breakdown_json` with:

- `version = 2`
- `token_line_items[]`
- `meter_line_items[]`
- selected `context_tier`
- selected `service_tier`
- `provider_multiplier`
- `base_charge_nano`
- `final_charge_nano`
- `cny_per_usd`: the decimal string of the MB-C1b snapshot, or `null` when no snapshot existed

MB-C4a. Each token and meter line item in `billing_breakdown_json` MUST record `unit_price_nano` and `unit_price_currency` of the applied rate row, and `charge_nano` in nano-USD after MB-C1a. A stored breakdown MUST therefore be sufficient to recompute the charge without reading `billing_rate_records` or the exchange-rate history.

MB-C5. A billable non-stream response or buffered synthetic stream without normalized usage MUST be rejected before delivery when the selected Channel has `allow_missing_usage = false`. A pass-through stream without terminal normalized usage MUST settle from an estimate whose input quantity is `ceil(serialized_upstream_request_utf8_bytes / 4)` and whose output quantity is `ceil(decoded_visible_output_utf8_bytes / 4)` when the selected Channel has `allow_missing_usage = false`; the resulting billing snapshot MUST contain `estimated = true`. When the selected Channel has `allow_missing_usage = true`, each of these missing-usage cases MUST instead settle with normalized input and output token quantities of zero and a total charge of zero. Present upstream usage MUST always take precedence over this Channel flag.

MB-C6. Once pass-through stream bytes have been delivered, a settlement error MUST NOT be converted into a successful zero-charge snapshot. Monoize MUST finalize the request log as an explicit billing failure containing the billing error code. The server MUST NOT claim that an error response was delivered downstream after the terminal stream event has already been sent.

MB-A8. The billing-rate partial upsert MUST accept `peak_unit_price_nano` with
dual-Option semantics matching MB-A2b: omitted keeps the stored value, an
explicit null (or an empty string) clears it to NULL, and a present non-empty
string MUST pass the MB-D3g canonical non-negative rule before persistence.
The response MUST always include the effective `peak_unit_price_nano` (null
when unset). `MB-A7b` profile copy MUST preserve the stored
`peak_unit_price_nano` of each source row.

## 7. Dashboard APIs

MB-A1. Admin endpoint `GET /api/dashboard/billing-rates` MUST return all billing-rate rows ordered by `pricing_profile ASC, priority DESC, id ASC`.

MB-A2. Admin endpoint `PUT /api/dashboard/billing-rates/{id}` MUST upsert one billing-rate row.

MB-A2a. If the request body omits `source`, the upserted row MUST use `source = "manual"`, even when a row with the same `id` already exists from `source = "catalog"` or `source = "models_dev"`.

MB-A2c. When `PUT /api/dashboard/billing-rates/{id}` creates a row and omits `unit_price_currency`, the created row MUST use `CNY`. When it updates a row and omits the field, the row MUST keep its stored currency. The response MUST always include the effective `unit_price_currency`.

MB-A2b. A billing-rate partial upsert MUST preserve omitted fields at the database write boundary. Concurrent partial upserts to distinct fields MUST NOT restore omitted fields from a stale pre-update snapshot on SQLite or PostgreSQL.

MB-A3. Admin endpoint `DELETE /api/dashboard/billing-rates/{id}` MUST delete one billing-rate row.

MB-A4. Admin endpoint `POST /api/dashboard/billing-rates/sync/catalog` MUST sync the bundled catalog. Manual rows with the same `id` MUST take precedence over catalog rows.

MB-A4a. Catalog sync MUST read the protected manual id set once and insert every non-protected catalog row through set-based statements split into fixed-size chunks. It MUST NOT issue one database round trip per catalog row. Every chunk MUST remain below the portable SQLite bound-parameter limit, and PostgreSQL MUST use the same chunking semantics.

MB-A4b. Bulk pricing-profile, Provider-type, and model-metadata lookup methods MUST split dynamic input sets into portable fixed-size chunks when necessary. A caller-controlled or database-sized input set MUST NOT exceed a backend bind-variable limit. Results from all chunks MUST preserve the method's documented deterministic order.

MB-A5. Admin endpoint `GET /api/dashboard/pricing-profile-patterns` MUST return the ordered profile-pattern setting.

MB-A6. Admin endpoint `PUT /api/dashboard/pricing-profile-patterns` MUST replace the ordered profile-pattern setting after rejecting empty `pattern` or `pricing_profile` strings.

MB-A7. Admin endpoint `POST /api/dashboard/billing-rates/profiles/{profile}/copy` MUST copy
every `billing_rate_records` row whose `pricing_profile` equals `{profile}` to the
`target_profile` given in the body, and MUST return the number of rows copied.

Profile names may now be shared across account classes (PP-ENT6 removed): request-side
routing keeps classes isolated, so a shared Profile bills every class the same rows. Copy
remains the way to diverge prices per class when an operator wants class-differentiated
pricing. Recreating a profile of several hundred rows by hand is not a workable alternative.

MB-A7a. The endpoint MUST reject, without writing any row:
- a `target_profile` that is empty or whitespace only, with `invalid_request`;
- a `target_profile` equal to `{profile}`, with `invalid_request`;
- a `{profile}` that has no rows, with `not_found`;
- a `target_profile` that already has at least one row, with HTTP `409` and code
  `pricing_profile_not_empty`.

Refusing a non-empty target is what keeps the operation from silently repricing a profile
that is already billing traffic. It also makes a repeated call fail rather than duplicate.

MB-A7b. Each copied row MUST take a new globally unique `id` derived from the target profile
and the source row id, MUST set `source` to `manual`, and MUST otherwise preserve every
field of the source row, including `unit_price_nano`, `peak_unit_price_nano`, `unit_price_currency`, `priority`, and
`enabled`.

`source = manual` and an id outside the `model_metadata:` namespace are both required for the
copy to survive. Catalog sync deletes every `source = 'catalog'` row, and deleting a model
metadata record deletes every rate whose id begins with `model_metadata:`. A copy that kept
either property would disappear when its unrelated source was next synced or removed.

MB-A9. Admin endpoint
`POST /api/dashboard/billing-rates/profiles/{profile}/models/{model}/rename` MUST rename a
model inside one pricing profile. The body is `{ "target_model": <string> }`. The endpoint
MUST return `{ "target_model": <string>, "written": <count>, "removed": <count>, "synchronized_retained": <count> }`.

A profile's model name is not a fixed property of the profile. An operator who renames an
upstream model, or who serves the same prices under a second alias, must be able to carry the
priced rows across without retyping every usage class.

MB-A9a. The rename MUST resolve the source rows as every `billing_rate_records` row whose
`pricing_profile` equals `{profile}` and whose `model_pattern` equals `{model}`, and MUST
write one row per source row under `model_pattern = target_model`.

MB-A9b. Each written row MUST take a new globally unique `id` derived from the profile, the
target model, and the source row's `usage_class`, MUST set `source` to `manual`, and MUST
otherwise preserve every field of the source row, including `unit_price_nano`,
`peak_unit_price_nano`, `unit_price_currency`, `rate_kind`, `unit`, `context_tier`,
`service_tier`, `modality`, `cache_ttl`, `match_json`, `priority`, and `enabled`.

`source = manual` and an id outside the `model_metadata:` namespace are required for the same
reason as MB-A7b: a written row that kept either property would disappear when the model
registry next synced or removed an unrelated record.

MB-A9c. After writing, the rename MUST delete every source row whose `id` does not begin with
`model_metadata:`. It MUST NOT delete a source row whose `id` begins with `model_metadata:`,
and MUST report the count of those retained rows as `synchronized_retained`.

A `model_metadata:` row is a mirror owned by the model registry, not by the profile: the
registry rewrites it on the next metadata edit and deletes it when the metadata record is
deleted. Deleting one here would either be undone or would silently remove pricing the
operator did not ask to remove. The former name therefore keeps its synchronized prices, and
the endpoint reports that rather than concealing it.

MB-A9d. The endpoint MUST reject, without writing any row:
- a `target_model` that is empty or whitespace only, with `invalid_request`;
- a `target_model` equal to `{model}`, with `invalid_request`;
- a `{profile}`/`{model}` pair with no rows, with `not_found`;
- a `target_model` that already has at least one row in `{profile}`, with HTTP `409` and code
  `pricing_profile_model_not_empty`.

MB-A11. `DELETE /api/dashboard/billing-rates/profiles/{profile}` MUST require an admin
session and delete every `billing_rate_records` row whose `pricing_profile` equals `{profile}`
(manual and synchronized rows alike) in one transaction. It MUST return `{ "deleted_rates":
<rows>, "deleted_models": <distinct model_pattern count> }` with HTTP 200. The endpoint MUST
reject, without deleting anything:

- a `{profile}` with no rate rows, with HTTP `404` and code `not_found`;
- a `{profile}` referenced by any pricing-profile match rule (`pricing_profile` field), with
  HTTP `409` and code `pricing_profile_in_use_patterns`;
- a `{profile}` referenced by any Provider (`monoize_providers.pricing_profile` or
  `pricing_profile_override`, or `monoize_provider_models.pricing_profile_override`), with
  HTTP `409` and code `pricing_profile_in_use_providers`.

The endpoint MUST NOT touch `model_metadata_records`. A later models.dev sync or metadata
edit recreates the `model_metadata:` mirrors under the deleted profile name if the registry
still maps models to it, so deleting a profile does not orphan the registry.

Refusing a non-empty target keeps the rename from silently repricing a model that is already
billing traffic, and makes a repeated call fail rather than duplicate.

MB-A9e. The whole rename MUST run in one transaction. A partially renamed model MUST never be
visible: a model that bills traffic must carry one complete rate set under one name.

MB-A9f. When two or more source rows share one `usage_class`, the rename MUST write exactly one
row for that class, and that row MUST carry the field values of the source row whose `id` does
not begin with `model_metadata:`. If every such source row begins with `model_metadata:`, the
row with the greatest `id` in ascending byte order MUST win.

Two source rows share a usage class when the model registry mirrors a price and the operator
then overrides that price manually. The manual row is the operator's explicit decision and the
`model_metadata:` row is a value the registry computed, so the manual value MUST survive the
rename; taking the mirror instead would silently revert an override at rename time. Writing one
row per class rather than one per source id is required because the target `id` derives from the
usage class (MB-A9b), so two source rows of one class address the same target row.



## Wallet preflight under concurrency

MB-CONC1. Before forwarding a balance-funded request, Monoize MUST answer the preflight from the user's persisted balance minus the total amount already reserved for that user by other in-flight requests. When no amount is reserved, this answer MUST equal the pre-existing rule: sufficient when the balance is positive, and unrestricted for an unlimited balance.

MB-CONC2. On passing the preflight, Monoize MUST reserve that request's own cost ceiling for the user. The ceiling MUST be the same value the plan-funded path reserves against. The reservation MUST be held until the request's funding scope is finalized, and MUST be released on every exit path: success, error, early return, or task drop. Release MUST NOT depend on a hand-written call on one path.

MB-CONC3. A cloned funding scope MUST share one reservation with its original, and the amount MUST be released exactly once, when the last clone is dropped. A clone MUST NOT double-count the amount, and dropping one clone MUST NOT release it while another still holds it.

MB-CONC4. The reservation MUST be subtracted from the available balance only. It MUST NOT change the persisted balance, MUST NOT write a ledger row, and MUST NOT settle.

MB-CONC5. When the request's cost ceiling is unknown, Monoize MUST fall back to the pre-existing rule unchanged and MUST NOT reserve. An unpriced route MUST NOT become newly rejectable because of this rule.

MB-CONC6. The reservation store MUST live in memory and MAY be lost on restart. Losing it MUST NOT permit spending, because it only ever makes the preflight stricter.

MB-CONC7. The reservation MUST NOT be exposed as a balance, a ledger entry, or a billing field. It is an admission bound, not money.

## Studio workflow charging

MB-ST1. Studio `image`/`video` steps charge a fixed per-run price: `ceil(base_nano_usd × channel_multiplier)` where `channel_multiplier` is the selected provider's `multiplier` (default 1). The charge posts when the step transitions `pending → running`; terminal `failed`/`canceled` steps refund in full, idempotently per step. Ledger reasons: `studio_image_charge`, `studio_video_charge`, `studio_image_refund`, `studio_video_refund`, with `meta.request_id = "studio_step_<step_id>"`.

MB-ST2. Studio `llm` steps charge by actual token usage from the rate snapshot resolved for `studio_agent_model`: `input_tokens × in_rate + output_tokens × out_rate`, settled on completion, ledger reason `studio_llm_charge`.

MB-ST3. Public API `/v1/videos` jobs bill the key owner's wallet via MB-ST1; key window limits do not apply in this version.

## Typed usage iterations

MB-U1. If canonical Usage contains a non-empty iterations list, settlement MUST sum the inclusive counters of those iterations. Settlement MUST NOT add the primary-generation top-level counters again.
MB-U2. Context tiers, token line quantities, and usage breakdowns MUST use the same aggregate. Native protocol response encoders MAY retain primary-generation counters where required by the target protocol.
MB-U3. If iterations are absent or empty, settlement MUST use the top-level counters without changing existing price, currency, or rounding rules.

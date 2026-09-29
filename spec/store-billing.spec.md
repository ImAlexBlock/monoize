# Store Billing Specification

## 0. Scope And Terms

SB-0.1. The Store MUST sell one-time balance products and one-time plan products.

SB-0.2. The Store MUST support one EPay Channel with Alipay and WeChat methods, and Stripe Checkout in Milestone 1.

SB-0.3. The configurable HTTP payment adapter belongs to Milestone 2. A draft HTTP Channel MUST NOT accept production checkout or callbacks in Milestone 1.

SB-0.4. The Store MUST support purchase orders, payment attempts, verified provider events, fulfillment, full refunds, disputes, chargebacks, settlement reconciliation, and redemption codes.

SB-0.5. An Admin MUST NOT set an unpaid order to paid or fulfilled. The API MUST NOT expose a manual paid or manual complete endpoint.

SB-0.6. `CNY` and `USD` are the only Store currencies.

SB-0.7. A payment amount MUST be a canonical base-10 integer string in the currency minor unit. A canonical nonnegative integer string matches `0|[1-9][0-9]*`.

SB-0.8. CNY minor units are fen. USD payment minor units are cents. The account ledger remains integer nano USD in `users.balance_nano_usd`.

SB-0.9. Payment code MUST NOT use `f32` or `f64` for money, exchange rates, quotas, reserves, or settlements.

SB-0.10. A Store deployment MUST mount Store mutation and callback endpoints on exactly one Store Primary unless a later specification defines shared routing and shared rate limits.

## 1. Exchange Rates And Money

SB-FX-1. The rate is the fixed constant `6.72` CNY per USD (`FIXED_CNY_PER_USD`). It is not a market quote. Every USD/CNY conversion in the Store, metered billing, spending limits, Admin balance adjustment, and every display MUST use this rate, so a stored nano-USD amount always projects to the same CNY amount. At startup and 15 minutes after each completed refresh, the Store Primary MUST persist a snapshot with this rate, with source and refresh timestamps equal to the refresh time. The Store Primary MUST NOT request a remote rate source. Changing the constant rescales the CNY projection of every existing balance; it requires a spec change and a deployment.

SB-FX-1a. SB-FX-3, SB-FX-4, SB-FX-15, SB-FX-16, SB-FX-19, and SB-FX-20 describe a remote rate source. They do not apply while SB-FX-1 fixes the rate. The fixed snapshot MUST still pass the SB-FX-4 response shape, SB-FX-5, SB-FX-6, and SB-FX-7 validation.

SB-FX-2. A Replica MUST read the persisted rate. A Replica MUST NOT request or update the remote rate.

SB-FX-3. The HTTP client MUST use the exact HTTPS host, system trust store, no redirects, a five-second timeout, and a 65,536-byte response limit.

SB-FX-4. A valid response MUST have `result = success`, `base_code = USD`, one positive integer source timestamp, and one positive finite decimal `rates.CNY` with at most 18 fractional digits.

SB-FX-5. The source timestamp MUST NOT be more than five minutes in the future, more than 48 hours before refresh time, or older than the active source timestamp.

SB-FX-6. The decimal rate MUST be between 1 and 20 CNY per USD inclusive.

SB-FX-7. The service MUST parse the decimal as reduced positive integers `N/D`. It MUST NOT persist or calculate a rounded reciprocal.

SB-FX-8. USD cents to CNY fen MUST equal `round(cents * N / D)`. CNY fen to USD cents MUST equal `round(fen * D / N)`.

SB-FX-9. USD cents to nano USD MUST equal `cents * 10,000,000`. CNY fen to nano USD MUST equal `round(fen * D * 10,000,000 / N)`.

SB-FX-10. Nano USD to plan CNY fen MUST equal `round(nano_usd * N / (D * 10,000,000))` using the entitlement rate generation.

SB-FX-11. `round` in SB-FX-8 through SB-FX-10 means round half away from zero. The service MUST round once at the final nonnegative rational result.

SB-FX-12. Checked integer overflow MUST return HTTP `409` with code `amount_overflow` and MUST write no order or ledger mutation.

SB-FX-13. A conversion-dependent checkout MUST fail with HTTP `503` and code `exchange_rate_unavailable` when the active local refresh is older than 60 minutes or the source timestamp is older than 48 hours.

SB-FX-14. A failed refresh MUST retain the prior snapshot without changing its validity timestamps.

SB-FX-15. A candidate rate change above the configured threshold MUST enter quarantine. The threshold MUST be configurable from 0.1 percent through 25 percent and MUST default to 5 percent.

SB-FX-16. Admin approval of a quarantined rate and Admin resume after suspected compromise MUST require reauthentication. Suspected compromise recovery MUST require two valid observations at least 15 minutes apart and two distinct Admin identities.

SB-FX-17. Each order MUST store the exact rate rational, source timestamp, refresh timestamp, settlement amount, and settlement currency used at quote time. Later rate updates MUST NOT change an order.

SB-FX-18. EPay MUST settle in CNY. Stripe MAY settle in CNY or USD only when the configured Stripe account capability allows the currency. EPay MUST reject a USD order and MUST NOT convert it to CNY.

SB-FX-19. The Store MUST persist refresh attempt URL, parser version, HTTP status, response-body SHA-256 digest when present, result category, consecutive failure count, and attempt time. It MUST NOT log the response body.

SB-FX-20. Three consecutive refresh failures MUST create a warning. Six failures or expiry within 15 minutes MUST create a critical alert.

SB-FX-21. Conversion checkout MUST require a current accepted exchange-source governance record. The review interval MUST NOT exceed 180 days.

## 2. Products, Quotes, And Entitlements

SB-P-1. A product MUST have an immutable ID, kind, name, description, price currency, price minor amount, sort order, enabled state, and monotonic revision.

SB-P-2. A product kind MUST be `balance` or `plan`. A price MUST be greater than zero.

SB-P-3. A balance product MUST store recharge and bonus amounts in its price currency. Actual received MUST equal recharge plus bonus with checked integer addition.

SB-P-4. A custom recharge MUST use the Admin minimum and maximum for its selected payment currency. Its bonus MUST be zero.

SB-P-4b. The default custom-recharge minimum MUST be 100 minor units for both CNY and USD,
which is 1 CNY. `sales-commission.spec.md` SC-1.7 requires it: at a 500 bp commission rate a
face value below 20 fen floors its commission to zero, so a smaller minimum would admit an
order that earns a sales agent nothing.

SB-P-4a. The face value that SB-P-3 and SB-P-4 produce is the input to a sales-code
discount, never its output. The equality between a fixed balance product's price and the
order amount MUST be evaluated against that face value, so a discounted order does not read
as a price mismatch. `sales-commission.spec.md` SC-2.4 through SC-2.6 define the resulting
amounts.

SB-P-5. A plan price and quota base MUST use CNY. A currency control MAY present the price in USD by using the active exchange rate.

SB-P-6. A plan MUST have a duration from 3,600 through 31,536,000 seconds and at least one quota rule.

SB-P-7. A quota rule kind MUST be `5h`, `12h`, `day`, `week`, `month`, or `custom`. A custom rule MUST use a whole-hour duration from 1 through 8,760 hours.

SB-P-8. Quota amounts MUST be positive CNY fen. Two rules in one plan MUST NOT use the same effective window duration.

SB-P-9. All plan quota rules MUST apply concurrently. `5h`, `12h`, and `custom` windows MUST roll. Day, week, and month windows MUST use Asia/Shanghai calendar boundaries. A week MUST start on Monday.

SB-P-10. A plan quota shown to a user MUST use a whole currency unit and round half away from zero. It MUST NOT show a decimal separator.

SB-P-11. A plan MAY contain zero or more Group IDs. Group IDs MUST follow `groups-registry.spec.md`. A nonempty list MUST restrict routing by intersection with the user's available Groups.

SB-P-12. Product update, disable, emergency disable, and delete MUST require the expected revision. A stale revision MUST return HTTP `409` and change no row.

SB-P-13. An order MUST contain an immutable versioned snapshot of product, price, reward, duration, Groups, quota rules, Channel public identity, settlement currency, rate rational, and payment contract version.

SB-P-13a. The snapshot MUST also contain the sales code submitted at creation and the
discount basis points that applied, as defined by `sales-commission.spec.md` SC-D5. A
discount reduces `payment_minor` while leaving the balance quote's
`actual_received_minor` at the face value, so the two amounts diverge by design and both MUST
remain frozen. Fulfillment continues to credit `actual_received_minor` per SB-RC, and a
refund continues to return `payment_minor` per SB-RC-25, which is the discounted amount the
buyer actually paid.

SB-P-14. Product edits MUST affect only orders created after the edit. A product referenced by an order MUST NOT be physically deleted.

SB-P-15. Disabling a product MUST NOT modify an existing unpaid order. That order MAY be paid until its 30-minute expiration.

SB-P-16. Emergency disable MUST create a close request for each unpaid order. It MUST query the provider before closing an attempt that may have been presented.

SB-P-17. One user MUST have at most one current Store entitlement pointer. The pointer MUST identify one immutable entitlement ID and generation.

SB-P-17A. `(user_id, generation)` MUST be unique. A user's first generation MUST equal `1`. Each later generation MUST equal the prior current generation plus `1`.

SB-P-17B. An entitlement generation row MUST NOT be updated or deleted after insertion. Suspension, admission blocking, and the current pointer MUST use separate rows.

SB-P-18. A new plan fulfillment or redemption MUST replace the active plan immediately. Unused time and quota MUST NOT carry forward.

SB-P-18A. Replacement MUST lock the current pointer, require the caller's expected current generation, insert one new generation, and change the pointer in one transaction. A stale expected generation MUST return `409 entitlement_generation_conflict` and write no generation or pointer change.

SB-P-18B. Replacement MUST NOT delete a prior generation, its buckets, or its reservations. A prior reservation MUST settle or release against its bound generation after replacement.

SB-P-19. Each entitlement generation MUST copy one immutable exchange rational `N/D`. `N` and `D` MUST be positive canonical integer strings and MUST be reduced by their greatest common divisor.

SB-P-19A. Reservation MUST convert maximum nano USD to CNY fen with ceiling division of `maximum_nano_usd * N / (D * 10,000,000)`.

SB-P-19B. Settlement MUST convert actual nano USD to CNY fen with round-half-away-from-zero of `actual_nano_usd * N / (D * 10,000,000)`. Reservation and settlement MUST use the rational stored on the bound generation.

SB-P-20. `(source_kind, source_id)` MUST be unique for entitlement generations. `source_kind` MUST be `order` or `redemption`.

SB-P-20A. Repeating one source with the same immutable snapshot MUST return the existing generation. Repeating that source with a different user, duration, Group, quota, or exchange rational MUST return `409 entitlement_source_conflict` and change no row.

## 3. Payment Channels, Credentials, And Capability

SB-C-1. A Channel adapter kind MUST be `epay`, `stripe`, or `http`. EPay method values MUST be `alipay` or `wxpay`.

SB-C-2. The migration MUST create one disabled, unconfigured EPay Channel and one disabled, unconfigured Stripe Channel when an equivalent Channel is absent. Legacy Alipay and WeChat Channel kinds MUST NOT remain active.

SB-C-3. A saved Channel credential MUST create a new immutable encrypted credential version. It MUST NOT overwrite a prior version.

SB-C-3A. `POST /api/dashboard/store/admin/reauth` MUST accept an Admin password and scope. Scope `credential_update` MUST return one random token with a five-minute expiry. The database MUST store only the token SHA-256 digest and current session-token SHA-256 digest. A grant MUST be invalid after its session becomes invalid, its user loses Admin role, or its expiry passes.

SB-C-3B. `PUT /api/dashboard/store/admin/payment-channels/{id}/credential` MUST require header `X-Store-Reauth-Token` with a valid `credential_update` grant. It MUST validate the adapter-specific credential, encrypt the exact JSON with record-bound AAD, insert one active version, retire every prior active version, invalidate merchant capabilities, and disable the Channel in one transaction.

SB-C-3C. A successful credential replacement response MUST contain only credential version ID, Channel ID, adapter kind, account-identity digest, status, and creation time. It MUST set `Cache-Control: no-store`. It MUST NOT return any credential field or ciphertext field.

SB-C-3D. A persisted Channel adapter kind MUST be immutable. `PUT /api/dashboard/store/admin/payment-channels/{id}` MUST require `expected_revision`. It MUST return HTTP `409` when the stored revision differs. Credential replacement MUST lock the Channel row, increment its revision, and disable it in the credential transaction.

SB-C-4. Credentials and recoverable redemption codes MUST use XChaCha20-Poly1305 with a 256-bit key, a random nonce, and associated data containing table name, row ID, and field name.

SB-C-5. The key ring MUST name one active encryption key and zero or more decrypt-only prior keys. An encrypted row without a matching key MUST block checkout and code reveal.

SB-C-5A. `MONOIZE_STORE_PAYMENT_KEYS_JSON` MUST contain one JSON object with `active` and `prior`. Each key object MUST contain `id` and `key_base64`. `key_base64` MUST decode to exactly 32 bytes. An absent variable MUST permit startup with payment creation unavailable. An empty or otherwise invalid present variable MUST stop startup.

SB-C-6. Admin reads MUST NOT return saved credentials, private keys, Webhook secrets, API v3 keys, or full redemption codes except through a scoped reveal response.

SB-C-7. A payment attempt MUST store its adapter kind, credential version ID, merchant-account identity, expected payment method, and payment contract version.

SB-C-8. A credential version MUST remain decryptable while any referenced attempt is inside its provider callback, refund, dispute, or reconciliation window.

SB-C-8A. A Stripe credential plaintext MUST be JSON with exactly `secret_key`, `publishable_key`, `webhook_signing_secret`, `api_version`, `account_id`, and `live_mode`. Every string MUST be nonempty. `api_version` MUST use `YYYY-MM-DD` or `YYYY-MM-DD.name` format. Unknown fields MUST fail validation.

SB-C-8B. A Stripe credential account identity MUST equal the lowercase SHA-256 hexadecimal digest of the exact UTF-8 `account_id`. Checkout MUST reject a credential when this digest differs from the immutable attempt identity.

SB-C-9. A Channel MUST have a current compliance confirmation. Confirmation MUST require an Admin session, reauthentication, `confirmed = true`, and the current terms version.

SB-C-10. A terms-version change MUST make production checkout unavailable. It MUST NOT disable historical callback verification, query, refund, dispute, or reconciliation.

SB-C-11. Each Channel capability MUST be `supported`, `unsupported`, or `manual` for payment query, refund, refund query, dispute event, dispute query, bill download, and settlement report.

SB-C-12. A capability record MUST contain merchant-account digest, environment, provider product, evidence digest, controlled transaction ID when applicable, verifier Admin, verification time, and expiry time.

SB-C-13. Production capability verification MUST expire after 90 days and immediately after a credential, merchant account, provider product, or API-version change.

SB-C-14. Checkout, payment query, callback verification, refund, refund query, and settlement report MUST be supported before a Channel becomes effectively available.

SB-C-15. A dispute capability MAY be `manual` only when the manual case path, provider deadline source, owner assignment, evidence path, and tabletop drill are current.

SB-C-16. Effective Channel availability MUST require stored enablement, current compliance, complete credentials, configured callback verification, compatible product and currency, valid amount capability, current merchant capability, current privacy record, current license record, and passing runtime gates.

SB-C-17. The user catalog MUST return only effectively available Channels. It MUST return public name, icon, adapter kind, supported currencies, amount limits, and checkout action kinds.

SB-C-18. A Channel icon MUST be built-in, HTTPS URL, or validated same-origin upload. An uploaded icon MUST pass the byte-signature and SVG restrictions in the prior Store icon contract.

SB-C-19. Every enabled Channel MUST record measured callback retry and query availability windows. Both MUST exceed the selected Store RTO by at least 15 minutes.

SB-C-20. `GET /api/dashboard/store/admin/payment-channels/{id}/compliance` MUST require an Admin session. It MUST return `{ "current_terms_version": string, "compliance": null | StorePaymentCompliance }`. `StorePaymentCompliance` MUST contain `id`, `channel_id`, `terms_version`, `admin_user_id`, `source_ip`, `confirmed_at`, and nullable `invalidated_at`. The response MUST use `Cache-Control: no-store`.

SB-C-21. `PUT /api/dashboard/store/admin/payment-channels/{id}/compliance` MUST require an Admin session, the SB-S-2 Origin check, and `X-Store-Reauth-Token` with scope `compliance_confirm`. Its exact JSON body MUST be `{ "confirmed": true, "terms_version": CURRENT_STORE_PAYMENT_TERMS_VERSION }`. Unknown fields, `confirmed = false`, and any other terms version MUST return HTTP `400`. The server MUST set `id`, `admin_user_id`, `source_ip`, and `confirmed_at`. The transaction MUST invalidate prior active confirmations for the Channel before it inserts the new confirmation. The response MUST return `StorePaymentCompliance` and use `Cache-Control: no-store`.

SB-C-22. `GET /api/dashboard/store/admin/payment-channels/{id}/capabilities` MUST require an Admin session. It MUST return `{ "capabilities": StoreMerchantCapability[] }` and use `Cache-Control: no-store`. `StoreMerchantCapability` MUST contain `id`, `channel_id`, `capability`, `state`, `environment`, `merchant_account_digest`, `provider_product`, `evidence_digest`, nullable `controlled_transaction_id`, `verifier_admin_id`, `verified_at`, and `expires_at`. It MUST NOT contain credential plaintext or encrypted credential fields.

SB-C-23. `PUT /api/dashboard/store/admin/payment-channels/{id}/capabilities/{capability}` MUST require an Admin session and the SB-S-2 Origin check. `capability` MUST be exactly one of `payment_query`, `refund`, `refund_query`, `dispute_event`, `dispute_query`, `bill_download`, or `settlement_report`. Its exact JSON body MUST be `{ "state": CapabilityState, "environment": string, "provider_product": string, "evidence_digest": string, "controlled_transaction_id": null | string }`. `CapabilityState` MUST be `supported`, `unsupported`, or `manual`. Unknown fields and invalid values MUST return HTTP `400`. `environment` and `provider_product` MUST contain 1 through 128 non-whitespace characters. `evidence_digest` MUST be 64 lowercase hexadecimal characters. A non-null `controlled_transaction_id` MUST contain 1 through 256 non-whitespace characters. The server MUST read `merchant_account_digest` from the active credential, set `verifier_admin_id`, set `verified_at`, and set `expires_at = verified_at + 90 days`. Missing active credentials MUST return HTTP `409`. The transaction MUST lock the Channel before it reads the active credential and upserts the `(channel_id, capability)` record. The response MUST return `StoreMerchantCapability` and use `Cache-Control: no-store`.

SB-C-24. `GET /api/dashboard/store/admin/payment-channels/{id}/availability` MUST require an Admin session. It MUST return `StoreChannelAvailability` and use `Cache-Control: no-store`. `StoreChannelAvailability` MUST contain `channel_id`, `effective_available`, and `unavailable_reasons`. `unavailable_reasons` MUST be a sorted array without duplicates. Admin `PaymentChannel` responses MUST contain the same `effective_available` and `unavailable_reasons` values.

SB-C-25. One evaluator MUST determine Channel availability for SB-C-17, SB-C-24, and order creation. The evaluator MUST return unavailable when the Channel is missing, disabled, uses `http`, lacks an active credential, lacks a current non-invalidated compliance confirmation, lacks a current matching `supported` record for any of `payment_query`, `refund`, `refund_query`, and `settlement_report`, or fails SB-C-27 through SB-C-30. A capability is not current when `expires_at <= evaluation_time`. A capability does not match when its `merchant_account_digest` differs from the active credential digest. A required capability row with an invalid capability name, state, environment, provider product, evidence digest, controlled transaction ID, merchant-account digest, verification time, or expiry time MUST return `capability_<kind>_invalid`. The evaluator MUST NOT return a storage error for malformed values in one capability row. Unknown capability data MUST NOT satisfy a requirement. `unavailable_reasons` MUST use only the reason codes defined by SB-C-25 through SB-C-30.

SB-C-26. The user catalog MUST omit a Channel when the SB-C-25 evaluator returns unavailable. Order creation MUST return `payment_channel_unavailable` for the same Channel at the same persisted state. The evaluator MUST fail closed for expired or credential-mismatched capability records.

SB-C-27. Migration `054` MUST create `store_channel_readiness_profiles` with one row per `channel_id`. A row MUST contain `channel_id`, `active_credential_digest`, `privacy_record_id`, `callback_verification_passed`, `supported_currencies_json`, `amount_limits_json`, `checkout_action_kinds_json`, `license_evidence_digest`, `runtime_evidence_digest`, `availability_evidence_digest`, `verifier_admin_id`, `verified_at`, and `expires_at`. `channel_id` MUST be unique. Each digest MUST be exactly 64 lowercase hexadecimal characters. `callback_verification_passed` MUST be `0` or `1`. Migration `054` MUST also expand the existing `store_reauth_grants.scope` constraint to accept `compliance_confirm` without deleting existing grant rows or removing the token-digest and expiry indexes. Migration `052` MUST remain byte-compatible with its released two-scope schema.

SB-C-28. `supported_currencies_json` MUST be a nonempty JSON array without duplicates containing only `CNY` and `USD`. `amount_limits_json` MUST be a JSON object with exactly one property for each supported currency and no other properties. Each property value MUST be `{ "min_minor": string, "max_minor": string }`. Both strings MUST be canonical positive base-10 integers and `min_minor <= max_minor`. `checkout_action_kinds_json` MUST be a nonempty JSON array without duplicates containing only `redirect`, `qr`, and `form`. Alipay MUST use currencies `["CNY"]` and actions `["form"]`. WeChat Pay MUST use currencies `["CNY"]` and a nonempty subset of `["qr", "redirect"]`. Stripe MUST use a nonempty subset of currencies `["CNY", "USD"]` and actions `["redirect"]`. A malformed, unknown, duplicate, empty, or adapter-incompatible currency or amount value MUST return `readiness_metadata_invalid`. An invalid or adapter-incompatible action value MUST return `checkout_action_incompatible`. Each reason MUST make the Channel unavailable.

SB-C-29. A readiness profile is current only when its `active_credential_digest` matches the active credential, `verified_at <= evaluation_time < expires_at`, `callback_verification_passed = 1`, and — for an adapter subject to the attestation gates (SB-C-38) — all three evidence digests are valid and its referenced `store_privacy_records` row exists, has `accepted = 1`, has `approved_at <= evaluation_time < next_review_at`, and contains a valid evidence digest. Missing readiness MUST return `readiness_profile_missing`. A credential mismatch MUST return `readiness_profile_credential_mismatch`. A future verification time or expired profile MUST return `readiness_profile_expired`. A missing, rejected, future, or expired privacy record MUST return `privacy_gate_pending`. Missing callback verification MUST return `callback_verification_pending`. Invalid license, runtime, or availability evidence MUST return `license_gate_pending`, `runtime_gate_pending`, or `availability_evidence_pending`, respectively. For an `epay` Channel the evidence and privacy gates MUST NOT be evaluated, so none of those four reasons can be raised. Each reason MUST block Catalog inclusion and order creation.

SB-C-30. `PaymentChannel` and `StoreChannelAvailability` MUST contain `supported_currencies`, `amount_limits`, and `checkout_action_kinds`. The evaluator MUST return empty values for all three fields when no valid readiness metadata exists. Catalog MUST return only Channels with `effective_available = true`. Order creation MUST first compute the final `payment_currency` and `payment_minor`, then use the same readiness profile to require a listed currency and `min_minor <= payment_minor <= max_minor`. An unsupported currency MUST return reason `payment_currency_unsupported`. An out-of-range amount MUST return reason `payment_amount_out_of_range`. An adapter-incompatible action set MUST return reason `checkout_action_incompatible`. Each order failure MUST map to `payment_channel_unavailable`.

SB-C-31. Before `create_attempt_with_outcome` inserts a payment attempt, it MUST run the SB-C-25 evaluator with the persisted order `payment_currency` and `payment_minor`. It MUST perform the same evaluation before it returns a replayed attempt in state `created`, because that result permits a provider checkout mutation. The required checkout action MUST be `qr` for WeChat Pay method `native`, `redirect` for WeChat Pay method `h5`, `form` for Alipay method `computer_web` or `mobile_web`, and `redirect` for Stripe method `card`. A missing method MUST select the adapter default: `native` for WeChat Pay, `computer_web` for Alipay, and `card` for Stripe. The readiness `checkout_action_kinds` MUST contain the required action. Missing, unknown, expired, disabled, credential-mismatched, amount-incompatible, currency-incompatible, or action-incompatible governance MUST return `payment_channel_unavailable`. The transaction MUST NOT insert an attempt in this case.

SB-C-32. Order creation and payment-attempt creation MUST lock the same `store_payment_channels` row used by credential replacement and capability writes. PostgreSQL MUST use `SELECT id FROM store_payment_channels WHERE id = $1 FOR UPDATE`. SQLite MUST execute `UPDATE store_payment_channels SET revision = revision WHERE id = $1` inside the serialized write transaction. Order creation MUST acquire its user lock before its Channel lock and MUST acquire the Channel lock before SB-C-25 evaluation. Attempt creation MUST acquire its order lock before its Channel lock and MUST acquire the Channel lock before SB-C-25 evaluation or active-credential selection. Credential replacement and Channel disablement MUST serialize on the same row before either transaction can commit.

SB-C-33. Catalog and Admin Channel-list governance reads MUST execute a fixed number of SQL statements independent of the Channel count. One batch load MUST read Channels, active credentials, current compliance confirmations, capabilities, readiness profiles, and privacy records with six set-based queries. It MUST NOT join capability or privacy collections into a row-multiplying result. Catalog and Admin Channel-list code MUST NOT call the asynchronous single-Channel evaluator inside a Channel loop. The single-Channel evaluator MUST use a scoped six-query load. Its first five queries MUST restrict Channel, active credential, current compliance, capabilities, and readiness by the requested `channel_id`. Its privacy query MUST restrict the row to the privacy ID referenced by that Channel readiness profile. It MUST NOT scan an unrelated Channel governance row. Batch and single-Channel evaluation of the same persisted state and evaluation time MUST return identical `StoreChannelAvailability` values, including sorted and deduplicated reasons.

SB-C-34. `GET /api/dashboard/store/admin/privacy-records` MUST require an Admin session. It MUST return `{ "records": StorePrivacyRecord[] }` and use `Cache-Control: no-store`. Records MUST be ordered by `approved_at DESC, id DESC`. `StorePrivacyRecord` MUST contain `id`, `policy_version`, `jurisdiction`, `allowed_regions`, `retention`, `legal_basis`, `reviewer_id`, `evidence_digest`, `approved_at`, `next_review_at`, and `accepted`. `retention` MUST be `{ "raw_callback_days": 30, "network_metadata_days": 90, "financial_records_days": integer, "redemption_audit_days": 730, "expired_reauth_grant_hours": integer }`. `POST /api/dashboard/store/admin/privacy-records` MUST require an Admin session, the SB-S-2 Origin check, and a Store Primary. Its exact JSON body MUST be `{ "policy_version": string, "jurisdiction": string, "allowed_regions": string[], "retention": StorePrivacyRetention, "legal_basis": string, "evidence_digest": string, "accepted": true, "review_after_days": integer }`. Unknown fields MUST return HTTP `400`. `review_after_days` MUST be in `[1, 365]`. `policy_version` MUST equal its trimmed value and contain 1 through 64 bytes. `jurisdiction` MUST equal its trimmed value and contain 1 through 128 bytes. `legal_basis` MUST equal its trimmed value and contain 1 through 512 bytes. `evidence_digest` MUST contain exactly 64 lowercase hexadecimal characters. `allowed_regions` MUST contain 1 through 32 unique values. Each region MUST equal its trimmed value, contain 1 through 64 bytes, and contain only ASCII letters, ASCII digits, `.`, `_`, or `-`. `raw_callback_days` MUST equal `30`. `network_metadata_days` MUST equal `90`. `financial_records_days` MUST be in `[1, 36500]`. `redemption_audit_days` MUST equal `730`. `expired_reauth_grant_hours` MUST be in `[1, 24]`. `accepted` MUST equal `true`. The server MUST generate `id`, set `reviewer_id` to the authenticated Admin ID, set `approved_at` to the transaction time, and set `next_review_at = approved_at + review_after_days`. The server MUST store compact canonical JSON for `allowed_regions` and `retention`. A privacy record MUST NOT be updated. The response MUST use HTTP `201` and `Cache-Control: no-store`.

SB-C-35. `GET /api/dashboard/store/admin/payment-channels/{id}/readiness` MUST require an Admin session. It MUST return `{ "readiness": null | StoreChannelReadinessProfile }` and use `Cache-Control: no-store`. An unknown Channel MUST return HTTP `404`. `StoreChannelReadinessProfile` MUST contain `channel_id`, `active_credential_digest`, `privacy_record_id`, `callback_verification_passed`, `supported_currencies`, `amount_limits`, `checkout_action_kinds`, `license_evidence_digest`, `runtime_evidence_digest`, `availability_evidence_digest`, `verifier_admin_id`, `verified_at`, and `expires_at`. `PUT /api/dashboard/store/admin/payment-channels/{id}/readiness` MUST require an Admin session, the SB-S-2 Origin check, and a Store Primary. Its exact JSON body MUST be `{ "privacy_record_id": string, "callback_verification_passed": bool, "supported_currencies": Currency[], "amount_limits": object, "checkout_action_kinds": CheckoutActionKind[], "license_evidence_digest": string, "runtime_evidence_digest": string, "availability_evidence_digest": string, "valid_for_days": integer }`. Unknown fields MUST return HTTP `400`. `privacy_record_id` MUST equal its trimmed value and contain 1 through 255 bytes. Each evidence digest MUST contain exactly 64 lowercase hexadecimal characters. `valid_for_days` MUST be in `[1, 90]`. The metadata MUST satisfy SB-C-28 for the locked Channel adapter. One write transaction MUST lock the Channel, read its active credential digest, require the referenced privacy record to have `accepted = 1` and `approved_at <= verified_at < next_review_at`, set `verifier_admin_id` to the authenticated Admin ID, set `verified_at` to the transaction time, set `expires_at = verified_at + valid_for_days`, and upsert the Channel readiness profile. An unknown Channel MUST return HTTP `404`. A missing active credential or non-current privacy record MUST return HTTP `409`. Invalid input MUST return HTTP `400`. The operation MUST NOT enable the Channel. The response MUST return the profile and use `Cache-Control: no-store`.

SB-C-36. The POST operation in SB-C-34 and the PUT operation in SB-C-35 MUST be mounted only in the Store mutation router. The Store mutation router MUST apply the SB-S-2 Origin check and Store Primary write guard before either handler mutates a business table. Their JSON input types MUST reject unknown fields.

## 4. Orders, Attempts, And Provider Mutations

SB-O-1. An order payment state MUST be `unpaid`, `paid`, `refund_pending`, `refunded`, or `closed`.

SB-O-2. An order fulfillment state MUST be `pending`, `fulfilled`, or `failed`.

SB-O-3. A payment attempt state MUST be `created`, `presented`, `expired`, `failed`, or `paid`.

SB-O-4. One order MUST have at most one attempt in `created` or `presented`.

SB-O-5. The service MUST commit the local order and attempt before it sends provider checkout bytes.

SB-O-6. Order creation MUST require a user-scoped `Idempotency-Key`. Reuse with the same canonical request MUST return the same order. Reuse with different input MUST return HTTP `409`.

SB-O-7. A user MUST create at most five orders per minute and have at most ten unpaid unexpired orders. Excess creation MUST return HTTP `429`. The service MUST perform the transactional idempotency recheck, both limit counts, and the order insert in one write transaction. SQLite MUST serialize that transaction with the `DbPool` write lock. PostgreSQL MUST execute `SELECT id FROM users WHERE id = $1 FOR UPDATE` before the transactional idempotency recheck and both limit counts. A concurrent request with the same user and `Idempotency-Key` MUST wait for this lock and MUST return the committed order after the wait. A missing user row MUST NOT change the existing order-creation result.

SB-O-8. A user MUST poll one order at most 30 times per minute across that user's order-detail requests on the single Store Primary. The order MUST belong to that user.

SB-O-9. Every provider mutation MUST use a stable idempotency key derived from the local object.

SB-O-10. A timeout, disconnect, HTTP 5xx, or unrecognized response after any provider request byte is sent MUST be ambiguous. The service MUST query provider state before retry.

SB-O-10A. A repeated attempt request that finds the same attempt in `created` state MUST return HTTP `502` with code `payment_provider_ambiguous` unless SB-O-10B applies. It MUST NOT create a second local attempt or use a new Provider idempotency key.

SB-O-10B. Stripe does not expose a Checkout Session query key before Checkout Session creation returns. A repeated request for the same `created` Stripe attempt MUST resend the byte-equivalent creation contract with the same `Idempotency-Key`. Stripe idempotency replay is recovery of the first mutation. It MUST NOT count as a second checkout mutation. A successful replay MUST persist the returned Checkout Session on the original attempt.

SB-O-10C. A Stripe idempotency replay that cannot load its historical credential or runtime configuration MUST return `payment_configuration_unavailable`. It MUST leave the original attempt in `created` state with no failure kind. A later replay with restored configuration MUST reuse the same local attempt and Provider idempotency contract.

SB-O-11. Only a verified Provider `NotFound` result or a Provider rejection that proves no payment object was created MAY permit a second checkout or refund mutation. SB-A-10B is such proof for Stripe Checkout Session creation. A transport error, HTTP 5xx, or unrecognized response is not proof.

SB-O-12. Checkout MUST return one action: `redirect`, `qr`, or `form`. Redirect and form action URLs MUST use HTTPS.

SB-O-12A. `POST /api/dashboard/store/orders/{id}/attempts` MUST commit the attempt before sending provider bytes. A successful response MUST contain `{ "attempt": PaymentAttempt, "action": CheckoutAction }`. The service MUST persist the exact browser-safe action before returning it.

SB-O-12B. Missing runtime keys, missing public origin, credential decryption failure, credential mismatch, and unsupported adapter checkout MUST set a new attempt to `failed` with failure kind `configuration_unavailable`. The endpoint MUST return HTTP `503` with code `payment_configuration_unavailable`. A definite provider rejection MUST set the attempt to `failed` with failure kind `provider_rejected` and return HTTP `422` with code `payment_provider_rejected`. An ambiguous provider result MUST leave the attempt in `created` state and return HTTP `502` with code `payment_provider_ambiguous`. Replaying a failed attempt MUST return the error mapped by its persisted failure kind without another provider request.

SB-O-12D. An order with a failed `provider_rejected` Alipay or WeChat attempt MUST reject a new attempt key with HTTP `409` and code `provider_query_required`. Only a verified Provider `NotFound` or `Closed` query result MAY clear this block. A Stripe rejection classified under SB-A-10B MUST permit a new attempt key because no Checkout Session exists. Rejection MUST occur before another checkout mutation.

SB-O-12E. A new payment-attempt transaction MUST lock its `store_orders` row before it counts or inserts attempts. A payment callback projection transaction MUST lock the same row before it validates or changes an attempt. PostgreSQL MUST use `SELECT ... FOR UPDATE`. SQLite MUST hold the serialized write transaction.

SB-O-12C. A first successful attempt response MUST use HTTP `201`. A repeated successful attempt response MUST use HTTP `200` and MUST return the persisted action without a provider request when the attempt is `presented` or `paid`.

SB-O-13. A custom return or cancel URL MUST match the configured HTTPS origin allow-list exactly. A rejected URL MUST create no local order and no provider object.

SB-O-13A. `MONOIZE_PUBLIC_ORIGIN` MUST be one HTTPS origin without credentials, path, query, or fragment. An absent value MUST permit startup with payment creation unavailable. An empty or otherwise invalid present value MUST stop startup.

SB-O-13B. Stripe success and cancel URLs MUST use `/dashboard/store`. The query MUST contain the exact local `order_id` and `checkout=success` or `checkout=cancel`.

SB-O-14. The allowed payment transitions are `unpaid -> paid`, `unpaid -> closed`, version-2 `closed -> paid`, `paid -> refund_pending`, `refund_pending -> refunded`, and `refund_pending -> paid` after definite rejection and local compensation.

SB-O-15. No other payment transition is valid. SQLite and PostgreSQL MUST enforce equivalent transition constraints in the database.

SB-O-16. The allowed fulfillment transitions are `pending -> fulfilled`, `pending -> failed`, and `failed -> fulfilled`.

SB-O-17. A state update MUST include the expected current state and revision. Zero updated rows MUST cause a fresh read.

SB-O-18. A provider transaction ID MUST be unique per adapter account. A second transaction for one paid order MUST create a reconciliation case and MUST NOT fulfill twice.

SB-O-19. A version-1 closed legacy order MUST reject new callback fulfillment. Version-2 `closed -> paid` MUST record a late-payment alert.

SB-O-20. `GET /api/dashboard/store/admin/orders/{id}` MUST require an Admin session. It MUST return `{ "order": PaymentOrder, "attempts": PaymentAttempt[], "refunds": RefundRecord[] }` and `Cache-Control: no-store`. Attempts MUST be ordered by `created_at ASC, id ASC`. Refunds MUST be ordered by `created_at ASC, id ASC`. The response MUST NOT contain credential plaintext, encrypted credential fields, Provider event raw bodies, or secret values.

SB-O-21. `POST /api/dashboard/store/admin/orders/{id}/query` MUST require an Admin session, the SB-S-2 Origin check, and a Store Primary. Its exact JSON body MUST be `{ "attempt_id": string }`. The path `id` and body `attempt_id` MUST each contain 1 through 128 Unicode characters. Every character MUST satisfy `char::is_whitespace() = false`. The attempt MUST belong to the path order. The operation MUST query the Provider through `PaymentQueryOperations` with the attempt's immutable credential version, Channel, adapter kind, merchant-account identity, order number, amount, currency, and payment contract version. The attempt `payment_contract_version` MUST equal the order `contract_version` and MUST be a supported contract version. A mismatch or unknown version MUST return `payment_configuration_unavailable` without a Provider call. It MUST NOT require the current Channel to be enabled or effectively available. A version-1 closed order MUST reject the operation.

SB-O-22. An Admin Provider-query response MUST be `{ "order": PaymentOrder, "attempt": PaymentAttempt, "provider_state": StoreProviderPaymentState, "projection": null | "applied" | "duplicate", "closed": bool }`. `StoreProviderPaymentState` MUST be `{ "kind": "not_found" | "unpaid" | "closed" | "ambiguous", "provider_transaction_id": null }` or `{ "kind": "paid", "provider_transaction_id": string }`. A Paid result MUST create a deterministic verified query event and apply it through `PaymentCallbackStore`. The event row ID, Provider event ID, and body digest MUST be deterministic functions of the immutable attempt contract and Provider transaction ID. Repeating the same Paid query MUST be idempotent. The operation MUST attempt fulfillment after a successful or duplicate payment projection. NotFound, Unpaid, Closed, and Ambiguous MUST return the verified Provider state without closing the order or expiring the attempt.

SB-O-23. `POST /api/dashboard/store/admin/orders/{id}/close` MUST require an Admin session, the SB-S-2 Origin check, and a Store Primary. Its exact JSON body and historical Provider-query binding MUST equal SB-O-21. Before any Provider query, it MUST require `contract_version = 2` and `payment_state = unpaid`. Every version-1 order and every order in `paid`, `refund_pending`, `refunded`, or `closed` MUST return `order_not_payable` without a Provider call, projection, or local state change. Paid MUST follow SB-O-22 and MUST NOT close the order. NotFound, Unpaid, or Closed MUST use one write transaction that locks the order before the attempt. The transaction MUST inspect every `created` or `presented` attempt for the locked order. If an active attempt other than the selected attempt exists, the operation MUST return `order_not_payable` without changing the order or any attempt. It MUST change only the selected attempt from `created`, `presented`, or `failed` to `expired`; it MUST change only a version-2 order from `unpaid` to `closed`; and it MUST return the latest order and attempt with `closed = true`. A repeated close MUST return `order_not_payable` before a Provider call. Two concurrent close transactions that both complete a Provider query MUST allow exactly one state transition. The losing transaction MUST return `order_not_payable` and MUST NOT return a successful closed result. It MUST reject a version-1 order and any order that is not unpaid.

SB-O-24. An Ambiguous result from Admin query or close MUST return HTTP `409` with code `payment_provider_ambiguous`. A Provider transport failure or unrecognized Provider response MUST return HTTP `502` with code `payment_provider_query_failed`. Historical credential absence, decryption failure, invalid credential data, contract mismatch, merchant-account mismatch, adapter `InvalidConfiguration`, or adapter `InvalidRequest` MUST return HTTP `409` with code `payment_configuration_unavailable`. These failures MUST NOT change order or attempt state. A missing order or an attempt that does not belong to the path order MUST return HTTP `404` with code `order_not_found`. Admin detail, query, and close responses MUST use `Cache-Control: no-store`.

## 5. Official Adapter Contracts

SB-A-1. Every adapter MUST implement checkout creation, payment query, callback verification, and full refund.

SB-A-2. An adapter MUST NOT modify a user balance, entitlement, order state, or recovery row directly.

SB-A-3. Alipay MUST support computer website and mobile website payment and MUST sign with RSA2.

SB-A-3A. An Alipay credential plaintext MUST be JSON with exactly `app_id`, `seller_id`, `merchant_private_key_pem`, `alipay_public_key_pem`, and `environment`. `environment` MUST be `production` or `sandbox`. Checkout MUST use `alipay.trade.page.pay` for `computer_web` and `alipay.trade.wap.pay` for `mobile_web`. The signed form MUST use the fixed official gateway for the selected environment.

SB-A-3B. Alipay checkout MUST settle in CNY. `biz_content` MUST contain the immutable order number, decimal amount with two fractional digits, subject, and a product code. Computer website payment MUST use `FAST_INSTANT_TRADE_PAY`. Mobile website payment MUST use `QUICK_WAP_WAY`. Common parameters MUST use `RSA2`, `utf-8`, JSON, version `1.0`, configured App ID, return URL, notify URL, and a UTC+08:00 timestamp.

SB-A-4. Alipay callback verification MUST check signature, App ID, seller identity, order number, amount, currency, and success state.

SB-A-5. Alipay MUST support trade query, refund, refund query, and bill download. Automated dispute support MUST remain unavailable unless the merchant capability test proves it.

SB-A-5A. Alipay payment query MUST call `alipay.trade.query` at the fixed gateway for the credential environment. It MUST sign App ID, method, format, charset, sign type, timestamp, version, and `biz_content` with RSA2. `biz_content` MUST contain the exact immutable order number.

SB-A-5B. An Alipay query response MUST use HTTP `200` and contain `alipay_trade_query_response` plus `sign`. The service MUST verify `sign` over the exact raw response-node JSON bytes with the configured Alipay public key. Code `40004` with sub-code `ACQ.TRADE_NOT_EXIST` MUST map to `NotFound`. Code `10000` MUST match order number, seller ID, CNY amount, and one documented trade state before projection.

SB-A-6. WeChat Pay MUST support Native QR and H5 payment using API v3.

SB-A-6A. A WeChat Pay credential plaintext MUST be JSON with exactly `merchant_id`, `app_id`, `api_v3_key`, `merchant_certificate_serial`, `merchant_private_key_pem`, `platform_certificate_serial`, and `platform_public_key_pem`. Every field MUST be nonempty. `api_v3_key` MUST contain exactly 32 UTF-8 bytes. The merchant certificate fields MUST authorize outbound requests. The platform certificate fields MUST verify inbound callbacks.

SB-A-6B. WeChat Native checkout MUST POST to `/v3/pay/transactions/native`. WeChat H5 checkout MUST POST to `/v3/pay/transactions/h5`. The JSON body MUST contain the configured App ID and merchant ID, immutable order number, description, callback URL, positive integer CNY fen, and `CNY`. H5 checkout MUST also contain the canonical client IP and `scene_info.h5_info.type = Wap`.

SB-A-6C. WeChat authorization MUST use scheme `WECHATPAY2-SHA256-RSA2048`. It MUST sign the exact method, canonical path, timestamp, nonce, and JSON body with the merchant private key. It MUST include merchant ID and merchant certificate serial.

SB-A-7. WeChat callback verification MUST require `Wechatpay-Timestamp`, `Wechatpay-Nonce`, `Wechatpay-Serial`, and `Wechatpay-Signature`. `Wechatpay-Serial` MUST equal the credential platform certificate serial. The signed bytes MUST be the exact timestamp, LF, nonce, LF, raw body, and LF. The timestamp MUST differ from Store Primary time by at most 300 seconds.

SB-A-7A. A WeChat payment callback body MUST be JSON with one nonempty event `id`, `event_type = TRANSACTION.SUCCESS`, and one `resource`. The resource MUST use `original_type = transaction` and `algorithm = AEAD_AES_256_GCM`. The service MUST decrypt it with the 32-byte API v3 key, the exact 12-byte nonce, and the exact associated data.

SB-A-7B. A decrypted WeChat payment resource MUST contain the configured merchant ID, configured App ID, nonempty order number, nonempty transaction ID, `trade_state = SUCCESS`, positive integer amount in fen, and `currency = CNY`. The verified payment event ID MUST equal the outer event `id`.

SB-A-7C. A WeChat platform-verification credential version MAY differ from the payment attempt credential version after platform certificate rotation. The callback MUST bind the attempt by Channel, order number, adapter kind, and the exact merchant-account identity digest. The provider event `credential_version_id` MUST remain the attempt credential version. The allow-listed parsed event MUST record the platform-verification credential version ID.

SB-A-7C1. A WeChat merchant-account identity digest MUST equal the lowercase SHA-256 hexadecimal digest of canonical merchant-side credential bytes. The canonical bytes MUST contain `merchant_id`, `app_id`, `api_v3_key`, `merchant_certificate_serial`, and `merchant_private_key_pem` in that order. Each field MUST be encoded as its unsigned 64-bit big-endian byte length followed by its exact UTF-8 bytes. The canonical bytes MUST exclude `platform_certificate_serial` and `platform_public_key_pem`. A callback MAY use a different verification credential only when its merchant-account identity digest equals the attempt digest.

SB-A-7C2. WeChat callback selection MUST evaluate every stored credential that can verify and decrypt the callback. It MUST bind each verified credential by Channel, order number, adapter kind, and merchant-account identity. Multiple verification credentials that bind the same attempt MUST form one binding. The selected verification credential MUST be the first credential in active-first, creation-time-descending, ID-descending order that belongs to that binding. Zero bound attempts or more than one distinct bound attempt MUST use the unbound manual-review path.

SB-A-7D. An Alipay or WeChat callback MAY bind one attempt whose Provider object ID equals the immutable order number or is null. The Channel, order number, adapter identity, credential identity, and merchant identity rules for that adapter MUST still match. The callback MUST reject zero matches or more than one match. A verified payment projection MUST set a null Provider object ID to the immutable order number.

SB-A-8. WeChat Pay MUST support order query, refund notification, refund query, and bill download. Complaint automation MUST remain unavailable unless the merchant capability test proves it.

SB-A-8A. WeChat payment query MUST send a signed GET to `/v3/pay/transactions/out-trade-no/{order_number}?mchid={merchant_id}`. The path segment and query value MUST use percent encoding. The signed canonical URL MUST equal the transmitted path and query.

SB-A-8B. Every WeChat query response, including `ORDER_NOT_EXIST`, MUST pass platform certificate serial, timestamp, nonce, and RSA signature verification over the exact raw body. A successful response MUST match merchant ID, App ID, order number, CNY amount, and one documented trade state before projection.

SB-A-8C. A WeChat query request MUST use the attempt's historical merchant signing credential. Response verification MUST select a stored platform certificate by `Wechatpay-Serial` from credential versions with the same Channel, adapter kind, and merchant-account identity. Platform certificate rotation MUST NOT replace the historical merchant signing credential.

SB-A-8D. WeChat query state `REFUND` MUST map to `Ambiguous` until a verified refund event or refund query establishes the refund transition. It MUST NOT project payment success or start fulfillment.

SB-A-9. Stripe MUST use hosted Checkout. Monoize MUST NOT collect or store card numbers.

SB-A-10. Stripe MUST create a Checkout Session with the Store order number as idempotency key and metadata reference.

SB-A-10A. Stripe Checkout creation MUST send `mode=payment`, one line item, `client_reference_id=order_number`, `metadata[store_attempt_id]=attempt_id`, `success_url`, and `cancel_url`. It MUST send the order number as `Idempotency-Key`. It MUST authenticate with the credential `secret_key`. It MUST reject a non-HTTPS action URL.

SB-A-10B. A Stripe client-error response MUST be a definite rejection only when its JSON contains one nonempty `error.type` and one nonempty `error.message`. A redirect, server error, malformed body, or unrecognized body MUST be ambiguous.

SB-A-10C. Stripe payment query MUST send an authenticated GET to `/v1/checkout/sessions/{provider_object_id}` with the credential API version. A response with `error.type = invalid_request_error` and `error.code = resource_missing` at HTTP `404` MUST map to `NotFound`. A successful Checkout Session MUST match its ID, client reference, amount, currency, and payment state before projection.

SB-A-10D. A payment query contract MUST contain the exact Provider object ID, immutable merchant order number, integer minor amount, and currency. A missing or mismatched contract field MUST fail verification. It MUST NOT map to a Provider payment state.

SB-A-11. Stripe Webhook verification MUST check signature, configured API version, Checkout Session, PaymentIntent, amount, currency, merchant account, and payment state.

SB-A-11A. A Stripe payment success event MUST use type `checkout.session.completed` or `checkout.session.async_payment_succeeded`. Its `data.object` MUST be a Checkout Session with nonempty `id`, `payment_intent`, `client_reference_id`, and `metadata.store_attempt_id`. `amount_total` MUST be a positive integer. `currency` MUST be `cny` or `usd`. `payment_status` MUST be `paid`. The event `account` MUST equal the credential `account_id`.

SB-A-12. Stripe MUST classify Checkout completion, asynchronous success, asynchronous failure, expiration, refund, dispute, and chargeback as distinct events.

SB-A-13. Stripe amount minimums MUST come from a versioned capability table bound to account country, currency, and configured API version. An unknown combination MUST disable that currency.

SB-A-14. Logs MUST NOT contain provider signature headers, raw callback bodies, merchant credentials, or full provider responses.

SB-A-15. The implementation MUST NOT copy AGPL source, tests, comments, UI, or internal identifiers from `QuantumNous/new-api`. The release MUST record the external commit and a similarity scan.

## 6. Callback Events And Event Ordering

SB-EV-1. A callback endpoint MUST be public and MUST require successful adapter verification before state projection.

SB-EV-2. A callback body MUST be at most 131,072 bytes and MUST complete reading within five seconds.

SB-EV-2A. A verified Stripe callback MUST return HTTP `200` with `{ "received": true }` after an applied, duplicate, or manual-review projection. Invalid signatures and invalid verified fields MUST return HTTP `400`. Missing callback keys MUST return HTTP `503`. A storage or fulfillment error MUST return HTTP `500`.

SB-EV-2B. A verified Alipay callback MUST return HTTP `200` with UTF-8 text `success` after an applied or duplicate projection. A manual-review projection caused by an order, attempt, credential, merchant, amount, or currency mismatch MUST return a non-200 response. The request MUST use `application/x-www-form-urlencoded`. Each decoded field name MUST occur once. An invalid signature, `sign_type`, App ID, seller ID, order number, trade number, amount, or trade state MUST return a non-200 response. A non-200 response MUST NOT contain a credential or raw callback body.

SB-EV-2C. A verified WeChat callback MUST return HTTP `200` with JSON `{ "code": "SUCCESS", "message": "成功" }` after an applied or duplicate projection. A manual-review projection MUST return a non-200 response. The request MUST use `application/json`. An invalid header, signature, timestamp, certificate serial, resource, merchant ID, App ID, order number, transaction ID, amount, currency, or trade state MUST return a non-200 response. A non-200 response MUST NOT contain a credential, decrypted resource, or raw callback body.

SB-EV-3. A callback event MUST store credential version, provider event identity, body digest, allow-listed verified fields, verification result, encrypted raw body, projection state, and state revision.

SB-EV-4. `(credential_version_id, provider_event_id)` MUST be unique. One verified event MUST project to at most one order.

SB-EV-4A. A verified Alipay or WeChat payment callback MAY bind an attempt whose Provider object ID is absent only when exactly one attempt matches the callback Channel, adapter kind, immutable order number, and required credential or merchant-account identity. A candidate with an absent Provider object ID MUST be `created` or `failed` with `failure_kind = provider_rejected`. The payment projection transaction MUST repeat the candidate query after it locks the order row for every Alipay or WeChat payment callback, including a selected attempt with a nonempty Provider object ID. The selected attempt MUST be the only matching candidate in that transaction. The transaction MUST set the absent Provider object ID to the immutable order number. It MUST NOT change a nonempty Provider object ID or bind an attempt from another order.

SB-EV-4B. A verified Alipay or WeChat callback with zero or multiple matching attempts MUST insert one `store_provider_events` row with `projection_state = manual_review` before it returns the adapter-defined non-200 response. The row MUST use the verification credential version, encrypted raw body, body digest, and allow-listed parsed fields. A retry with the same `(credential_version_id, provider_event_id)` MUST NOT insert another event. This path MUST NOT insert `store_order_event_applications`.

SB-EV-4C. Callback projection input MUST carry the attempt credential version and the verification credential version as separate fields. An applied event MUST use the attempt credential version. A lock-time zero-candidate or multiple-candidate event MUST use the verification credential version. A synthetic Provider query MUST set both fields to the same attempt credential version and MUST NOT enter the unbound callback event path.

SB-EV-5. Projection state MUST be `pending`, `applied`, `superseded`, or `manual_review`.

SB-EV-6. A duplicate verified event with the same body digest and immutable parsed identity MUST return the provider success acknowledgement and MUST create no second ledger, recovery, fulfillment, or state transition. A duplicate `(credential_version_id, provider_event_id)` with a different body digest or immutable parsed identity MUST preserve the original event, return manual review, and idempotently create one open `provider_event_identity_conflict` reconciliation case. The case ID MUST be deterministic for the credential version and Provider event identity.

SB-EV-6A. The payment migration MUST create `idx_store_attempt_order_candidates` on `store_payment_attempts`. Its leading columns MUST be `(order_id, channel_id, adapter_kind, created_at DESC, id DESC)`.

SB-EV-7. An event received before prerequisite payment evidence MUST remain pending and trigger provider query. It MUST NOT be discarded because of arrival order.

SB-EV-8. Payment success on `unpaid` or version-2 `closed` MUST set paid. It MUST start fulfillment only when no refund success, lost dispute, or payment hold blocks it.

SB-EV-9. Payment failure or close on `unpaid` MUST query provider and close only after verified unpaid state. It MUST NOT downgrade paid or refunded state.

SB-EV-10. Refund success before payment evidence MUST query the original payment, persist payment evidence, then set refunded without fulfillment.

SB-EV-11. Dispute open before payment evidence MUST remain pending and query by provider object and merchant order. Confirmed payment MUST apply dispute open without fulfillment.

SB-EV-12. Dispute or chargeback during `refund_pending` MUST retain refund state, apply dispute state, share the existing recovery reserve, retain payment hold, and query both operations.

SB-EV-13. Refund success after chargeback MUST set refunded and MUST NOT create another economic recovery.

SB-EV-14. A contradictory terminal event MUST require a provider query and a newer provider object version when the provider exposes versions. Arrival time MUST NOT override terminal state.

SB-EV-15. A database write failure MUST return a non-success callback acknowledgement so the provider can retry.

SB-EV-16. `POST /api/dashboard/store/admin/provider-events/{event_id}/reprocess` MUST require an Admin session, the SB-S-2 Origin check, a Store Primary, and header `X-Store-Reauth-Token` with scope `reprocess`. Its exact JSON body MUST be `{}`. Every success and error response MUST use `Cache-Control: no-store`.

SB-EV-17. Migration `056` MUST expand `store_reauth_grants.scope` to accept `reprocess`. It MUST preserve every existing grant and recreate `uq_store_reauth_token_digest` and `idx_store_reauth_expiry`. Migrations `052`, `054`, and `055` MUST remain unchanged.

SB-EV-18. Reprocess MUST accept only a stored event whose `verification_result` equals `verified` and whose `event_kind` equals `payment_succeeded` or `payment_query_succeeded`. An `applied` event MUST return `duplicate` without another state mutation. A `superseded` event MUST return `event_not_reprocessable`. A `pending` event MAY be processed. A `manual_review` event MAY be processed only when its current binding produces exactly one legal attempt and no open `provider_event_identity_conflict` case references the event identity.

SB-EV-19. Reprocess MUST use the stored verified event as verification evidence. It MUST NOT verify the expired Provider signature again. An event with encrypted raw body fields MUST decrypt them with AAD `store_provider_events:{event_id}:raw_body` and MUST require SHA-256 of the plaintext to equal the stored body digest. A query-synthesized event MUST have no raw body fields. Reprocess MUST strictly parse only the allow-listed immutable fields in `parsed_json`. A malformed, missing, or mismatched raw body, digest, parsed identity, amount, currency, merchant identity, credential binding, Provider object, or Provider transaction MUST change no business state.

SB-EV-20. Reprocess MUST reconstruct binding from current database state and immutable stored evidence. Stripe MUST bind the stored Attempt ID and revalidate its order, credential, Provider object, Provider transaction, amount, currency, and merchant account. Alipay and WeChat Pay MAY bind an absent Provider object only under SB-EV-4A. They MUST use the stored verification credential, Channel, adapter kind, merchant identity, and order number. Candidate count MUST equal one before and after the order lock. Reprocess MUST preserve the attempt credential version and verification credential version as distinct values.

SB-EV-21. Reprocess MUST lock or compare-and-swap the Provider event state revision before projection. It MUST then lock the order, repeat candidate selection, and repeat the open `provider_event_identity_conflict` case check. At most one concurrent callback or reprocess transaction MAY insert `store_order_event_applications` or change payment state. A competing request MUST reread the event and return the current idempotent result. Payment projection and fulfillment MUST remain separate transactions under SB-RC-1.

SB-EV-22. Reprocess MUST NOT create or retry a Provider payment or refund mutation. An event that requires fresh Provider payment evidence MUST return `provider_query_required` and remain pending or manual review.

SB-EV-23. Every authenticated and authorized reprocess request MUST append one immutable `store_access_audits` row with action `provider_event_reprocess`. The audit MUST contain actor ID, the requested event row ID, result, and time. It MUST contain the prior projection state and revision when the event exists. It MUST contain JSON `null` for both prior values when the event ID is invalid or the event does not exist. It MUST NOT contain a raw body, signature, credential, decrypted resource, or Provider response.

SB-EV-24. A successful reprocess response MUST be `{ "event_id": string, "projection": "applied" | "duplicate" | "pending" | "manual_review", "projection_state": string, "state_revision": integer, "order_id": string | null, "attempt_id": string | null }`. `event_id` MUST be the Provider event row ID. Error codes MUST be `invalid_request`, `event_not_found`, `event_not_reprocessable`, `projection_manual_review`, `provider_query_required`, `event_identity_conflict`, `invalid_reauth_grant`, `store_write_rejected`, or `internal_error`.

## 7. Fulfillment, Refunds, Disputes, And Settlement

SB-RC-1. Payment projection and fulfillment MUST use separate transactions. A crash between them MUST leave `paid/pending` for reconciliation.

SB-RC-1a. The Store Primary MUST run an isolated fulfillment-recovery pass once per minute
(the SB-OP-0-permitted subset of `run_once`: no payment queries and no refund operations).
The pass MUST select `payment_state = 'paid'`, `fulfillment_state IN ('pending','failed')`,
`payment_hold = 0` orders (up to 100, `paid_at ASC, id ASC`, first run 30 seconds after
`paid_at`) and attempt fenced fulfillment, scheduling the SB-OP-2 backoff retry row on
failure. A pass failure MUST be logged and retried on the next tick; it MUST NOT stop the
loop. Without this pass a payment callback whose inline fulfillment failed transiently
leaves the order paid/pending forever.

SB-RC-2. Balance fulfillment MUST append one idempotent `store_recharge` ledger credit and update the balance in one transaction.

SB-RC-3. Plan fulfillment MUST insert one entitlement generation and update the current pointer with an expected-generation predicate.

SB-RC-4. A duplicate callback or reconciliation run MUST NOT credit or activate twice.

SB-RC-5. Milestone 1 MUST support full refunds only.

SB-RC-6. A fulfilled plan order MUST NOT be refundable. A paid plan order MAY be refunded only while fulfillment is pending or failed.

SB-RC-7. A fulfilled balance refund MUST reserve the original credited nano USD before calling the provider. The reserve transaction MUST lock order, recovery, and user in that order.

SB-RC-8. Refund, dispute, and chargeback claims for one order MUST share one economic recovery row.

SB-RC-8A. An economic recovery state MUST be `open`, `reserved`, `recovered`, or `released`. A recovery claim state MUST be `open`, `resolved`, or `consumed`. A refund claim MUST be `open` while its refund is `created` or `pending`, `resolved` after definite rejection, and `consumed` after verified success.

SB-RC-8B. Starting the same refund idempotency key again MUST return the existing refund without another balance mutation. Starting a different refund while the order is not `paid` MUST fail before a Provider request.

SB-RC-9. Database constraints MUST enforce nonnegative values and `reserved + recovered <= original` across concurrent claims.

SB-RC-10. Recovery claim identity and recovery ledger keys MUST be unique. One original reward MUST be debited at most once.

SB-RC-10A. Reuse of `(credential_version_id, provider_claim_id, kind)` MUST return the existing claim only when it belongs to the same order. Reuse for another order MUST fail without state mutation.

SB-RC-11. A definite refund rejection MUST release a reserve once only when no unresolved dispute or chargeback claim remains.

SB-RC-12. An ambiguous refund MUST remain `refund_pending`. The reconciler MUST query it before any retry.

SB-RC-13. A verified dispute open MUST set payment hold, create or reuse the shared recovery claim, block pending fulfillment, and suspend an active plan sourced from that order.

SB-RC-13A. A Store plan entitlement MUST store nullable `suspended_at` and `suspension_reason`. Entitlement reads and plan admission MUST exclude a suspended entitlement. A verified dispute win MAY clear a `payment_dispute` suspension only while the original entitlement end time is in the future and no other claim remains open.

SB-RC-14. A verified dispute win MUST resolve that claim. It MAY clear hold only when all claims are resolved and user balance is nonnegative.

SB-RC-15. A verified dispute loss or chargeback MUST consume at most the remaining original reward, MAY make balance negative, revoke the affected plan, and retain payment hold.

SB-RC-16. Payment hold MUST block Store order creation, redemption before lookup, and plan admission. It MUST NOT block login, Store reads, callbacks, refunds, reconciliation, or eligible ordinary-balance API use.

SB-RC-17. An already presented payment MUST remain queryable during hold. Verified payment MUST be recorded, but fulfillment MUST remain pending.

SB-RC-18. Each daily settlement line MUST have a provider-unique identity and class `gross`, `refund`, `dispute`, `fee`, `tax`, `currency_conversion`, or `net`.

SB-RC-18A. `store_settlement_reports` MUST store Channel, credential version, Provider report identity, report date, body digest, and import time. `(credential_version_id, provider_report_id)` MUST be unique.

SB-RC-18B. `store_settlement_lines` MUST store report, credential version, Provider line identity, class, signed integer minor amount, currency, optional Provider transaction identity, optional matched order, and creation time. `(credential_version_id, provider_line_id)` MUST be unique.

SB-RC-19. Settlement import MUST be idempotent. Fees, taxes, and settlement FX MUST NOT change user rewards.

SB-RC-20. An unmatched payment, refund, dispute, or unknown settlement difference MUST create a critical manual case.

SB-RC-21. `POST /api/dashboard/store/admin/orders/{id}/refunds` MUST require an Admin session, the SB-S-2 Origin check, a Store Primary, header `X-Store-Reauth-Token` with scope `refund`, and header `Idempotency-Key`. Its exact JSON body MUST be `{}`. The response MUST return `RefundRecord` and use `Cache-Control: no-store`.

SB-RC-22. `GET /api/dashboard/store/admin/orders/{id}/refunds/{refund_id}` MUST require an Admin session. The refund MUST belong to the path order. The response MUST return `RefundRecord` and use `Cache-Control: no-store`.

SB-RC-23. `POST /api/dashboard/store/admin/orders/{id}/refunds/{refund_id}/query` MUST require an Admin session, the SB-S-2 Origin check, a Store Primary, and header `X-Store-Reauth-Token` with scope `refund`. Its exact JSON body MUST be `{}`. The refund MUST belong to the path order. The response MUST return `RefundRecord` and use `Cache-Control: no-store`.

SB-RC-24. Migration `055` MUST expand the released `store_reauth_grants.scope` constraint to accept `refund`. It MUST preserve every existing grant and recreate `uq_store_reauth_token_digest` and `idx_store_reauth_expiry`. Migrations `052` and `054` MUST remain unchanged.

SB-RC-25. Refund creation MUST call `RecoveryStore.begin_refund` and commit the reserve and `paid -> refund_pending` transition before any Provider request. A fulfilled plan order MUST return `order_not_refundable`. A fulfilled balance order MUST have enough available balance to reserve its complete original reward. A refund MUST use the immutable paid attempt, full order `payment_minor`, and full order `payment_currency`. Amount values passed to or returned from a Provider MUST be base-10 integer strings.

SB-RC-26. Refund creation and query MUST load the immutable paid attempt's adapter kind, Channel ID, credential version ID, merchant-account identity, Provider transaction ID, order number, payment contract version, amount, and currency. They MUST decrypt the historical credential version with record-bound AAD. A missing, undecryptable, malformed, unsupported, or identity-mismatched historical credential MUST return `payment_configuration_unavailable` without another Provider mutation.

SB-RC-27. The local refund ID MUST be the stable Provider idempotency key. Stripe MUST send it as the `Idempotency-Key` header. Alipay MUST send it as `out_request_no`. WeChat Pay MUST send it as `out_refund_no`. Repeating the Admin `Idempotency-Key` for the same order MUST return the existing refund without another reserve or duplicate Provider create request. Reusing it for another order MUST return `idempotency_conflict`. Starting a different key while the order is not `paid` MUST fail before a Provider request.

SB-RC-28. A Provider refund state MUST be `not_found`, `pending`, `succeeded`, `failed`, or `ambiguous`. It MAY contain a nonempty Provider refund ID. `succeeded` MUST call `RecoveryStore.complete_refund`. Definite `failed` or channel-defined definite `not_found` MUST call `RecoveryStore.reject_refund`. `pending` or `ambiguous` MUST retain order state `refund_pending`, MUST persist the Provider refund ID when present, and MUST NOT release the reserve.

SB-RC-29. A refund create transport error, HTTP 5xx response, oversized response, or unrecognized response MUST be treated as ambiguous. Before retrying create, the adapter MUST query by the stable Provider idempotency key. It MAY retry create once only after a verified `not_found` result. An existing local refund MUST NOT be sent as a new create mutation. A replay of a local `created` refund less than 300 seconds old MUST return that record without a Provider query. A replay MAY query a local `created` refund after this recovery interval. A local `pending` refund MUST be queried.

SB-RC-30. Stripe refund creation MUST call `POST /v1/refunds` with the immutable charge or payment transaction ID, full integer minor amount, and Provider idempotency key. Stripe refund query MUST call `GET /v1/refunds/{provider_refund_id}` or use the stable metadata key when the Provider refund ID is unknown. The adapter MUST bind the configured Stripe account and API version.

SB-RC-31. Alipay refund creation MUST call `alipay.trade.refund`. Alipay refund query MUST call `alipay.trade.fastpay.refund.query`. Both operations MUST use RSA2, the immutable order number, full decimal CNY amount derived from integer fen, and stable `out_request_no`. The adapter MUST verify the signed response and merchant identity before accepting a terminal state.

SB-RC-32. WeChat Pay refund creation MUST call `POST /v3/refund/domestic/refunds`. Refund query MUST call `GET /v3/refund/domestic/refunds/{out_refund_no}`. Both operations MUST use the immutable transaction ID or order number, full integer CNY fen amount, stable `out_refund_no`, WeChat merchant-request signing, bounded response bodies, platform-certificate response verification, and merchant identity validation.

SB-RC-33. `RefundOperations` MUST accept an injected `RefundProvider`. The production provider MUST use the shared bounded `reqwest::Client`. The `http` adapter kind MUST return `payment_configuration_unavailable`; Milestone 1 MUST NOT send a custom HTTP adapter refund.

## 8. Redemption Codes And Reauthentication

SB-R-1. New codes MUST use Crockford Base32 alphabet `0123456789ABCDEFGHJKMNPQRSTVWXYZ` and 16 random characters grouped as `XXXX-XXXX-XXXX-XXXX`.

SB-R-2. The database MUST store code format, SHA-256 lookup digest, final four-character hint, encrypted full code, reward snapshot, expiry, state, creator, and redemption data.

SB-R-2A. A v2 unused unexpired code MUST store `code_format_version = 2` and one complete encrypted value bound to AAD `store_redemption_codes:{code_id}:code`. A v1 row MUST have `code_format_version = 1` and no encrypted value. An encrypted value MUST store format version, key ID, nonce, and ciphertext as separate columns. Destruction MUST set every encrypted field to null and set `ciphertext_destroyed_at` in the same transaction.

SB-R-3. A generation response MUST return every full generated code once. The dialog MUST keep them visible until the Admin closes it.

SB-R-3A. Generation, v2 reveal, and v2 export require a loaded Store PaymentKeyRing. When
the key ring is absent, each operation MUST return HTTP `503` with the same fixed code
`store_redemption_encryption_unavailable`. The response MUST NOT return an environment
variable value, key ID, ciphertext field, or backend error text.

SB-R-4. An unused v2 code MAY be revealed or exported only with a reauthentication grant scoped to redemption access and expiring after five minutes.

SB-R-5. The server MUST store only a hash of a reauthentication token. Logout, password change, session rotation, session expiry, role removal, or account disablement MUST invalidate the grant.

SB-R-6. A reveal response MUST contain at most 20 codes. An export MUST contain at most 100 codes.

SB-R-6A. Reauthentication scope `redemption_access` MUST authorize reveal, copy, and export. `POST /api/dashboard/store/admin/redemption-codes/reveal` MUST accept one to 20 code IDs and action `reveal` or `copy`. `POST /api/dashboard/store/admin/redemption-codes/export` MUST accept one to 100 code IDs and return CSV. `POST /api/dashboard/store/admin/redemption-codes/{id}/revoke` MUST revoke one unused code.

SB-R-7. Reveal and export responses MUST set `Cache-Control: no-store`, `Pragma: no-cache`, `Referrer-Policy: no-referrer`, and `X-Content-Type-Options: nosniff`.

SB-R-8. Reveal, copy, and export MUST write an audit containing Admin, action, code IDs, count, IP, User-Agent, and time. It MUST NOT contain full codes.

SB-R-9. Successful redemption and revocation MUST delete encrypted full code in the same transaction. Cleanup MUST delete encrypted full code within 24 hours after expiry.

SB-R-10. Legacy v1 codes MUST remain redeemable through their existing digest and alphabet. They MUST NOT become recoverable.

SB-R-10A. An Admin list record MUST contain `can_reveal` and nullable
`reveal_unavailable_reason`. The reason MUST be `legacy_digest_only` or
`ciphertext_destroyed`. It MUST NOT contain encrypted value fields. A v1 row MUST return
`can_reveal = false` and `reveal_unavailable_reason = legacy_digest_only`. A v2 row MUST
return `can_reveal = true` only when its state is `unused`, its expiry is later than response
generation time, and every encrypted value field is present. A v2 row with any missing
encrypted value field MUST return `can_reveal = false` and
`reveal_unavailable_reason = ciphertext_destroyed`. Every other v2 row MUST return
`can_reveal = false` and MAY return a null reason.

SB-R-11. Redemption normalization MUST uppercase ASCII letters and remove ASCII hyphens. It MUST NOT map look-alike characters.

SB-R-12. Invalid syntax and no match MUST both return HTTP `404` with code `invalid_redemption_code`.

SB-R-13. Redemption MUST lock or serialize the code row and apply the reward and used state in one transaction. It MUST NOT create a payment order or require a payment Channel.

SB-R-13A. A balance redemption MUST accept a canonical signed `users.balance_nano_usd` value, including a negative current balance. It MUST add the nonnegative reward with checked signed integer arithmetic. It MUST update the balance, append the ledger credit, and mark the code used in one transaction.

SB-R-14. Redemption MUST allow at most ten attempts per minute per user and source IP. Five failed attempts in 15 minutes MUST create a 30-minute account-and-IP cooldown.

SB-R-14A. Redemption limits MUST use a persistent lock row keyed by user and SHA-256 source-IP digest plus persistent attempt rows. The service MUST lock the limit row before it counts attempts. A rate-limited or cooldown request MUST perform no code lookup or mutation.

## 9. Plan Quota Admission

SB-Q-1. A quota bucket MUST store nonnegative settled and reserved CNY fen. A reservation MUST store unique request ID, entitlement ID, generation, maximum nano USD, reserved CNY fen, exchange rational, pricing revision, state, admitted time, and terminal time.

SB-Q-1A. A rolling `5h`, `12h`, or `custom` bucket admitted at time `t` MUST use `[t, t + window_seconds)`. Admission at time `u` MUST count each rolling bucket whose end is greater than `u`.

SB-Q-1B. A `day` bucket MUST use one Asia/Shanghai local day. A `week` bucket MUST start at Monday 00:00:00 Asia/Shanghai. A `month` bucket MUST start at local day 1 at 00:00:00. Stored boundaries MUST be UTC instants derived from those local boundaries.

SB-Q-1B-1. A stored quota rule with `window_kind = 'custom'` and
`window_seconds = 86400` MUST be normalized to `window_kind = 'day'` by
migration `m20260916_000104_normalize_quota_day`. After the migration no
quota rule with `window_kind = 'custom'` and `window_seconds = 86400` may
remain. Rolling `custom` windows other than 86400 seconds are unaffected.

SB-Q-2. Admission MUST lock the current entitlement pointer, generation, and applicable buckets in `(window_end, quota_rule_id, bucket_id)` byte order. It MUST require `settled_fen + reserved_fen + reserve_fen <= quota_fen` for every applicable rule.

SB-Q-2A. One successful admission MUST insert one reservation, one link per applicable bucket, and update every applicable bucket in one transaction. Each link MUST store its exact reserved CNY fen. A plan with five applicable rules MUST reserve all five buckets or write none.

SB-Q-2B. Repeating a request ID with identical user, entitlement, generation, maximum, pricing revision, and bucket bindings MUST return the original reservation. A different binding MUST return `409 quota_idempotency_conflict` and change no bucket or reservation.

SB-Q-3. Admission MUST fail with HTTP `402` and code `plan_quota_exhausted` when any rule fails. It MUST write no reservation or bucket change.

SB-Q-3A. Funding admission MUST read the current entitlement generation and lifecycle by user ID without an RFC3339 range predicate. It MUST parse generation start, generation end, suspension, and revocation values as `DateTime<Utc>` before comparison. Start is inclusive and end is exclusive. A missing, not-started, expired, suspended, revoked, or group-inapplicable entitlement MUST select Balance. A malformed persisted time MUST return `quota_storage_error` and MUST NOT select Balance. A payment hold, quota admission block, or non-passed effective quota Gate MUST reject before quota mutation. A rejected request MUST write no reservation and MUST change no bucket.

SB-Q-3B. Quota exhaustion MUST use `plan_quota_exhausted`. A request without a finite positive bound MUST use `plan_request_unbounded`. A payment hold MUST use `plan_payment_hold`. An uncleared quota violation block MUST use `plan_quota_violation_blocked`. A non-passed effective quota Gate MUST use `quota_gate_unavailable`. Admission runtime wrapping MUST preserve these codes and `quota_storage_error`; it MUST NOT replace them with `admission_storage_error`.

SB-Q-4. Settlement MUST subtract the reservation's reserved CNY fen from every bound bucket and add the exact actual CNY fen to every bound bucket in one transaction. Release MUST subtract the reserved amount and add zero settled amount.

SB-Q-4A. Repeating the same terminal settlement or release MUST return the stored terminal result and change no counter. A different terminal kind or different actual amount MUST return `409 quota_terminal_conflict` and change no counter.

SB-Q-4B. Settlement and release MUST use the bucket IDs and generation stored on the reservation. Current pointer replacement and window expiry MUST NOT redirect a terminal operation.

SB-Q-4C. After a request handler creates a plan reservation, every error returned before the handler transfers ownership of that reservation to a response body MUST await release of the reservation before it returns the error. This rule applies to request-log admission, request transformation, HTTP client creation, request encoding, and exhausted routing.

SB-Q-4D. A returned streaming response MUST own its plan reservation until terminal settlement or release. A downstream disconnect MUST NOT stop the internal stream drain that reaches this terminal operation. If the stream ends while the reservation remains reserved, finalization MUST release it and report `plan_settlement_required` internally.

SB-Q-4E. A successful request MAY finish with reservation state `released` only when the selected handler branch explicitly requires zero charge, including `skip_charge` after missing usage is allowed. Such a branch MUST release the reservation before successful finalization. Successful finalization MUST accept `settled`, `violated`, or `released`. A successful request that remains `reserved` MUST release the reservation and return `plan_settlement_required`.

SB-Q-5. When actual CNY fen exceeds reserved CNY fen, settlement MUST apply the full actual amount to every bound bucket, insert one quota violation, and insert one user admission block in the same transaction. The block MUST identify the bound generation and MUST block later plan admission after replacement. Settlement MUST NOT charge a replacement generation.

SB-Q-5A. A quota violation MUST contain reservation ID, request ID, entitlement ID, generation, reserved CNY fen, actual CNY fen, detected time, and critical severity. Repeating the same settlement MUST NOT create a second violation or block.

SB-Q-6. SQLite admission and terminal operations MUST use WAL, foreign keys, `busy_timeout = 5000`, and short write transactions that acquire the database write lock before their first read. `BEGIN IMMEDIATE` or the equivalent bound transaction in DB12a MUST provide this ordering. The deployment MUST retain one Store Primary lease owner. Foreground admissions and terminal operations MAY overlap between the serving Primary and a standby candidate against the same database file during blue-green deployment. Independent pools MUST produce at most one quota allocation when only one reservation fits, and identical terminal replays MUST change counters once. Network calls and price calculation MUST occur outside the transaction.

SB-Q-7. SQLite plan features MUST require one persisted quota-engine compatibility fingerprint. The fingerprint MUST bind quota compatibility ID, schema version, SQLite library version, journal mode, busy timeout, page size, synchronous mode, filesystem identity, and quota manifest digest. It MUST NOT bind the ordinary application version.

SB-Q-7A. The Gate store MUST support `current` and `next` manifests. Cutover MUST promote `next` only when its fingerprint equals the starting process fingerprint. A nonmatching manifest MUST have effective state `pending`.

SB-Q-7B. The starting process environment MUST use compatibility ID `store-plan-quota-v1` and logical schema version `1`. Logical schema version `1` MUST require applied migration records `m20260827_000049_store_billing` and `m20260827_000051_store_payment_core`. The quota manifest digest MUST equal the lowercase SHA-256 digest of the following exact ASCII bytes: `compatibility_id=store-plan-quota-v1\nschema_version=1\nrequired_migrations=m20260827_000049_store_billing,m20260827_000051_store_payment_core\n`.

SB-Q-7C. A live SQLite environment probe MUST read `sqlite_version()`, `PRAGMA journal_mode`, `PRAGMA busy_timeout`, `PRAGMA page_size`, and `PRAGMA synchronous` from the quota write connection. It MUST lowercase ASCII journal mode. It MUST map synchronous values `0`, `1`, `2`, and `3` to `off`, `normal`, `full`, and `extra`. Any other synchronous value MUST make the effective Gate `pending`.

SB-Q-7D. A live SQLite environment probe MUST hold the SQLite write mutex. It MUST set the connection-local busy timeout to `5000` before reading the environment. It MUST restore the connection-local busy timeout to `15000` after success or failure. It MUST NOT open a transaction.

SB-Q-7E. A file-backed SQLite filesystem identity MUST identify the filesystem that contains the SQLite `main` database. On Unix it MUST equal `unix-dev:` followed by the lowercase hexadecimal `st_dev` value. On Windows it MUST equal `windows-volume:` followed by the eight-digit lowercase hexadecimal volume serial number. It MUST NOT bind the database path, inode, or file index. An in-memory SQLite connection MUST use `memory:` followed by a process-generated UUID. DbPool clones MUST retain that identity. A new in-memory connection MUST generate a different identity. An in-memory test database MAY use journal mode `memory`. A file-backed SQLite database MUST use journal mode `wal`.

SB-Q-7F. A SQLite Gate is effective only when the stored row fingerprint, stored manifest fingerprint, stored manifest environment fingerprint, and live process environment fingerprint are equal and the stored manifest environment equals the live process environment. A missing or unreadable live component MUST make the effective Gate `pending`. PostgreSQL MUST NOT require a SQLite quota Gate.

SB-Q-7G. The project MUST provide an offline `monoize-store-ops` binary. `monoize-store-ops quota-gate import --slot <current|next> --manifest <path>` MUST read the target DSN only from `MONOIZE_DATABASE_DSN`. It MUST reject a missing DSN, a non-SQLite DSN, a manifest larger than 65,536 bytes, invalid JSON, unknown JSON fields, a self-inconsistent manifest fingerprint, or a manifest environment that differs from the live SQLite environment. It MUST hold the SQLite write mutex while it probes the live environment and writes the selected Gate slot. A rejected import MUST NOT change either Gate slot. A successful import MUST write one `passed` slot and print its slot and compatibility fingerprint.

SB-Q-7H. `monoize-store-ops quota-gate promote --expected-fingerprint <fingerprint>` MUST read the target DSN only from `MONOIZE_DATABASE_DSN`. It MUST reject a missing DSN, a non-SQLite DSN, an empty expected fingerprint, a non-passed `next` slot, a `next` fingerprint mismatch, or a live environment mismatch. A rejected promotion MUST NOT change either Gate slot. A successful promotion MUST replace `current` with `next` in one transaction, reset `next` to `pending`, and print the promoted compatibility fingerprint. Operators MUST stop the application process before either offline command connects to the target database.

SB-Q-8. A `pending` or `failed` effective SQLite Gate MUST block plan product enablement, catalog availability, order creation, plan code generation, fulfillment, and admission. Balance products MUST remain available when other Store gates pass.

SB-Q-9. A Replica MUST obtain a Primary-signed Ed25519 admission token before routing a plan-funded request. Primary unavailability MUST fail plan admission closed and MUST create no local reservation.

SB-Q-10. A token MUST use compact JWS. Its protected header MUST contain `alg = EdDSA`, `typ = lynshen-plan-admission`, and a nonempty `kid`.

SB-Q-10A. Token claims MUST bind version, issuer, Replica ID audience, token ID, reservation ID, request ID, entitlement ID, generation, maximum nano USD, reserved CNY fen, pricing revision, issued time, not-before time, and expiry.

SB-Q-11. Token expiry MUST equal issued time plus 30 seconds. Not-before MUST equal issued time. Verification MUST reject when `now < nbf - 5 seconds` or `now >= exp + 5 seconds`; no larger skew is allowed. A token observed two minutes before or after its valid interval MUST be rejected.

SB-Q-11A. Key rotation MUST publish a next public key before private-key activation. A prior public key MUST remain verifiable for at least five minutes after deactivation and until every token issued by that key has expired plus five seconds.

SB-Q-12. Before routing, a Replica MUST atomically create and fsync a durable claim marker bound to token ID, reservation ID, request ID, and audience. It MUST fsync the containing directory before routing. An existing marker MUST reject same-node replay. A wrong audience MUST reject cross-node replay before marker creation.

SB-Q-12A. A claim marker MUST remain until the Primary acknowledges one matching settlement or release and until at least five minutes after token expiry. Marker write or fsync failure MUST fail admission closed.

SB-Q-13. A Replica MUST append and fsync one settlement or release record to its terminal spool before it reports terminal billing success. Spool write or fsync failure MUST fail closed and retain the claim marker.

SB-Q-13A. Primary application of a terminal spool record MUST use SB-Q-4A idempotency. A replay with identical content MUST succeed without mutation. A replay with conflicting content MUST return `quota_terminal_conflict`.

SB-Q-14. `store_admission_tokens` MUST persist token ID, audience, request ID, user ID, canonical effective groups JSON, reservation ID, entitlement ID, generation, maximum nano USD, reserved CNY fen, pricing revision, signing key ID, compact JWS, compact JWS SHA-256 digest, issued time, expiry, expiry Unix seconds, and nullable confirmation time. `expires_at_unix` MUST equal the signed 64-bit Unix-second value of the parsed `expires_at` instant. `(audience, request_id)`, token ID, reservation ID, and compact JWS digest MUST each be unique.

SB-Q-14A. Primary issue input MUST contain user ID, Replica audience, external request ID, effective groups, finite positive maximum nano USD, pricing revision, and issue time. Effective groups MUST be sorted by UTF-8 bytes and deduplicated before persistence. Empty identifiers MUST be rejected with `admission_input_invalid`.

SB-Q-14B. Primary issue MUST execute authoritative current-entitlement lookup, group applicability, quota reservation, token signing, provisional admission-token insertion, and active-key expiry update in one write transaction. It MUST lock the current generation and lifecycle rows. It MUST NOT compare RFC3339 timestamp text in SQL. It MUST parse entitlement start, end, suspension, and revocation values as instants and compare `DateTime<Utc>` values in application code. An entitlement applies only when its start time is not later than issue time, its end time is later than issue time, and its lifecycle has neither suspension nor revocation. An entitlement with no groups applies to every group set. A grouped entitlement requires at least one exact effective-group match. A missing, not-started, expired, suspended, revoked, or group-inapplicable current entitlement MUST return `Balance`, MUST NOT load admission signing keys, and MUST create no reservation or token. A malformed persisted generation or lifecycle time MUST return `quota_storage_error`, MUST NOT select Balance, and MUST create no reservation or token. An applicable entitlement MUST return `Plan(IssuedAdmission)` with null confirmation time. Failure MUST roll back every reservation, bucket, token, and key update.

SB-Q-14C. Repeating `(audience, request_id)` with identical user ID, canonical effective groups, maximum nano USD, and pricing revision MUST return the stored compact JWS byte-for-byte and MUST create no reservation, bucket, or token. A changed binding MUST return `admission_issue_conflict`. Issue time MUST NOT participate in retry equality.

SB-Q-14C-1. SQLite issue MUST use one `BEGIN IMMEDIATE` transaction. PostgreSQL issue MUST acquire one transaction-level advisory lock before its first admission-token lookup. The two signed 32-bit lock keys MUST be the first and second big-endian words of SHA-256 over the exact UTF-8 bytes `v=1\naudience=<audience>\nrequest_id=<external request ID>\n`. The lock call MUST be `pg_advisory_xact_lock(key1, key2)`. Therefore concurrent exact retries MUST return one compact JWS, and a concurrent changed binding MUST return `admission_issue_conflict`.

SB-Q-14C-2. Quota reservation request IDs created by admission issue MUST be internal and audience-scoped. The internal value MUST equal `admission:` followed by lowercase SHA-256 hexadecimal over the same bytes defined by SB-Q-14C-1. `store_admission_tokens.request_id`, compact JWS `request_id`, terminal input, and terminal receipt MUST retain the external request ID. Equal external request IDs from different audiences MUST create independent reservations and tokens.

SB-Q-14D. `store_admission_keys` MUST allow states `published`, `active`, and `retired`. A published key MUST have no activation, retirement, last-issued-expiry, or verify-until time. An active key MUST have encrypted seed JSON and activation time, and MUST have no retirement or verify-until time. A retired key MUST have encrypted seed JSON, activation time, retirement time, and verify-until time. At most one key MAY be active. Configuration epoch MUST be nonnegative.

SB-Q-14E. Primary issue MUST read active and retired key rows without RFC3339 text range comparison. It MUST parse their time fields as `DateTime<Utc>` and exclude retired keys whose verify-until instant is not later than issue time before it decrypts seed data. It MUST decrypt the active and retained retired Ed25519 seeds with the configured Store `PaymentKeyRing`. Seed encryption AAD MUST equal `store-admission-key:` followed by key ID and `:seed:v1`. Each decrypted seed MUST contain exactly 32 bytes. Its derived public key MUST equal the canonical base64url-no-padding `public_key_base64` value. Active `last_issued_expires_at`, when present, MUST parse as an instant. Missing wrap keys MUST return `admission_wrap_key_missing`. Missing active keys MUST return `admission_active_key_missing`. Invalid encrypted, public, state, or time data MUST return `admission_key_invalid`.

SB-Q-14E-1. PostgreSQL issue MUST read active and retired signing-key rows with `FOR UPDATE`. While holding the lock, issue MUST parse the active `last_issued_expires_at`, compute the later of that instant and the new token expiry, and write the resulting canonical UTC timestamp without SQL text comparison or `CASE`. The update MUST affect exactly one row. A zero-row or multi-row result MUST return `admission_key_invalid` and roll back reservation and token changes.

SB-Q-14F. The Primary public keyset MUST read active and retired rows without RFC3339 text range comparison. It MUST parse all required state times as `DateTime<Utc>` and filter retired keys by instant in application code. It MUST validate key ID, state/time shape, canonical base64url-no-padding encoding, decoded length of exactly 32 bytes, and Ed25519 verifying-key validity before it excludes an expired retired row. Invalid data in an expired retired row MUST return `admission_key_invalid`. It MUST return only key ID, canonical public key, state, activation time, and verify-until time. It MUST include the active key and retired keys whose verify-until instant is greater than query time. It MUST exclude published and valid expired retired keys. It MUST NOT return encrypted seed JSON, wrap-key ID, configuration epoch, or private data.

SB-Q-15. `store_admission_terminal_receipts` MUST persist one row per token ID. It MUST persist reservation ID, request ID, audience, terminal kind, optional actual nano USD, canonical terminal digest, and applied time. Settlement requires a nonnegative actual amount. Release requires no actual amount.

SB-Q-15A. The canonical terminal payload MUST be the exact ASCII sequence `v=1\ntoken_id=<token>\nreservation_id=<reservation>\nrequest_id=<request>\naudience=<audience>\nkind=<settlement|release>\nactual_nano_usd=<canonical decimal or empty>\n`. Its digest MUST be lowercase SHA-256 hexadecimal. A supplied nonmatching digest MUST return `admission_terminal_digest_invalid` before mutation.

SB-Q-15B. Primary terminal apply MUST lock the stored token row before it reads the terminal receipt. After the token exists, it MUST check receipt state before request binding. When a receipt exists, the same canonical digest MUST return `duplicate`; any changed digest, reservation ID, external request ID, or audience MUST return `admission_terminal_conflict`. Only when no receipt exists MUST a token binding mismatch return `admission_binding_mismatch`. It MUST apply quota settlement or release and insert the terminal receipt in one write transaction. SQLite MUST use `BEGIN IMMEDIATE`; PostgreSQL MUST use the token row `FOR UPDATE` lock. Concurrent exact terminal replays MUST produce one `applied` and one `duplicate`. Concurrent changed replays MUST produce one `applied` and one `admission_terminal_conflict`.

SB-Q-15B-1. Settlement MUST require nonnull admission-token confirmation time. Release MAY apply before or after confirmation. A settlement on an unconfirmed token MUST return `admission_terminal_conflict` and change no quota or receipt. Unconfirmed-token recovery MUST skip a confirmed token and MUST treat any existing receipt as terminal.

SB-Q-15D. RFC3339 values MUST be compared only after parsing to UTC instants. SQL lexical operators, SQL `CASE` expressions, and host-language string comparison MUST NOT decide entitlement activity, key retention, key expiry maxima, or public keyset membership.

SB-Q-15C. Repeating a terminal apply with the same token ID and canonical digest MUST return `duplicate` and MUST perform no quota mutation. Repeating the token ID with a different digest or binding MUST return `admission_terminal_conflict`. Any receipt insertion failure MUST roll back the quota terminal mutation.

## 10. Reconciliation, Manual Cases, And Operations

SB-OP-0. The periodic reconciliation scheduler gate is OPEN as of 2026-09-22: the application implements every scan class in SB-OP-3 with the corresponding verified Provider query, so the Store Primary MUST run the scheduler under SB-OP-2. The application MUST spawn the scheduler only when both the Store Primary lease and the payment key ring are available; a node without a payment key ring MUST NOT run reconciliation. The scheduler subsumes the previously isolated fulfillment-recovery run, and the application MUST NOT spawn a separate fulfillment-recovery loop while the scheduler is active.

SB-OP-1. Only the Store Primary MUST run reconciliation. The reconciler MUST acquire the `store_reconciler` row in `store_reconciliation_leases`. A lease MUST contain an opaque owner ID, a strictly increasing fencing epoch, and an expiry 90 seconds after acquisition. A second owner MUST NOT process work before expiry. Every reconciled fulfillment transaction MUST lock and validate the exact owner and fencing epoch before it changes financial state.

SB-OP-1A. Each fenced transaction MUST compare lease expiry with the run start time plus the elapsed monotonic run duration. It MUST NOT reuse the unadjusted run start time after a Provider call. A run that reaches lease expiry without renewal MUST stop before another state change.

SB-OP-2. After SB-OP-0 opens the scheduler gate, reconciliation MUST run once per minute. One fulfillment-recovery run MUST select at most 100 candidates ordered by `paid_at ASC, id ASC`.

SB-OP-3. Reconciliation MUST scan expired presented attempts, paid/pending orders older than 30 seconds, paid/failed orders, refund-pending orders, and retryable provider events. A refund-pending scan MUST select at most 100 refunds ordered by the due time ascending and refund ID ascending. A refund with no retry row becomes due one minute after its order `refund_pending_at`. A refund with a retry row becomes due when `next_attempt_at <= now`. The scan MUST use `RefundOperations` with the refund's immutable historical payment contract. The reconciliation-only `RefundOperations` Provider query entry point MUST return the Provider query outcome without changing refund, order, recovery, claim, balance, ledger, retry, or case state. A Provider call MUST be followed by a fresh fence time computed from the run start wall time plus elapsed monotonic time. The reconciler MUST validate that fence before state projection. Provider result projection and its retry, case, or terminal cleanup MUST occur in one write transaction after that validation.

SB-OP-3A. An expired presented-attempt scan MUST select at most 100 attempts with `provider_expires_at <= now`, ordered by `provider_expires_at ASC, id ASC`. It MUST query with the attempt's exact historical credential version and immutable order contract.

SB-OP-3AA. The reconciler MUST also query `created` and `failed/provider_rejected` Alipay or WeChat attempts at least 30 seconds after their last update. These adapters MUST use the immutable merchant order number when no Provider object ID was persisted. Stripe attempts without a Checkout Session ID MUST use SB-O-10B and MUST NOT fabricate a query result.

SB-OP-3AB. The expired presented-attempt scan MUST also select `presented` attempts whose `provider_expires_at` is still in the future but whose `updated_at` is more than 120 seconds in the past ("stale presented"). A buyer who pays while the checkout page is closed cannot rely on the Provider callback arriving; without this scan such an order is only discovered after the QR expires. A stale presented attempt follows the exact SB-OP-3A query path and the exact SB-OP-3C projection: a verified `Paid` result is applied immediately; a verified `NotFound`, `Unpaid`, or `Closed` result MUST NOT expire the attempt while `provider_expires_at > now` — the attempt stays `presented` and is re-scanned at most once per 60-second case-retry cutoff until its real expiry applies SB-OP-3C.

SB-OP-3B. A verified `Paid` query result MUST enter the same idempotent payment projection as a verified callback. The projection and any resulting fulfillment MUST validate the active reconciliation owner and fencing epoch before financial state changes.

SB-OP-3C. A verified `NotFound`, `Unpaid`, or `Closed` result for an expired presented attempt MUST change that attempt from `presented` to `expired` and its order from `unpaid` to `closed` in one fenced transaction. A verified `Ambiguous` result, a verification failure, and a transport failure MUST leave both states unchanged. A contract-version 2 order that later receives verified payment MAY transition from `closed` to `paid` under SB-O-16.

SB-OP-3D. A payment projected from a verified query MUST create one deterministic `payment_query_succeeded` provider-event identity per credential version, attempt, and Provider transaction. A repeated query MUST reuse that projection identity and MUST NOT fulfill twice.

SB-OP-3E. A verified `NotFound` or `Closed` result for a `created` or `failed/provider_rejected` attempt MUST change that attempt to `expired`, clear `failure_kind`, and leave the order `unpaid`. This transition permits one new attempt. A verified `Unpaid` result MUST keep the attempt blocked because a Provider object may still accept payment.

SB-OP-3F. A query verification error, credential error, unsupported recovery path, `Ambiguous` result, or blocked `Unpaid` result MUST upsert one open reconciliation case with deterministic ID `payment-query:{attempt_id}`. The case MUST store only an error category and non-secret attempt evidence. A definite terminal result MUST close the case.

SB-OP-3G. An attempt with an open payment-query reconciliation case MUST NOT be queried again until at least 60 seconds after the case `updated_at` time.

SB-OP-4. A paid order without a retry row becomes eligible 30 seconds after `paid_at`. After the first failed reconciliation, the next delay MUST be two minutes. After the second failure, the next delay MUST be ten minutes. After every later failure, the next delay MUST be one hour.

SB-OP-4A. `store_fulfillment_retries` MUST contain one row per order. It MUST store `attempt_count`, `next_attempt_at`, `last_error_category`, and `updated_at`. A successful fulfillment MUST delete this row in the same fenced transaction. A failed fulfillment MUST upsert the next delay without changing the order payment state or user reward.

SB-OP-4B. PostgreSQL MUST store Store order revisions, provider-event revisions, lease epochs, and fulfillment retry counts as signed 64-bit integers. SQLite MUST accept the same signed 64-bit range.

SB-OP-5. Refund query delays MUST be one minute, five minutes, 15 minutes, then hourly. The first query MUST occur no earlier than one minute after the order enters `refund_pending`. After the first completed query retains `refund_pending`, the next delay MUST be five minutes. After the second completed query retains `refund_pending`, the next delay MUST be 15 minutes. After the third and every later completed query retains `refund_pending`, the next delay MUST be one hour. A non-storage query error MUST retain `refund_pending`, persist a stable error category, increment the completed-query count, and use the same delay sequence. A storage error MUST stop the reconciliation run with `ReconciliationError::Storage` and MUST NOT schedule a retry.

SB-OP-5A. Migration `057` MUST create `store_refund_query_retries`. The table MUST contain `refund_id` as its primary key, nonnegative signed 64-bit `attempt_count`, `next_attempt_at`, nullable `last_error_category`, nullable `alerted_at`, and `updated_at`. `refund_id` MUST reference `store_refunds(id)` with cascade deletion. The migration MUST create index `idx_store_refund_query_retries_due` on `(next_attempt_at, refund_id)`. SQLite and PostgreSQL MUST enforce equivalent constraints.

SB-OP-5B. Every reconciliation run MUST perform an alert-only scan independent of the Provider query due scan. The alert-only scan MUST select pending refunds with `refund_pending_at <= now - 15 minutes`, no `alerted_at`, and no open `refund-pending:{refund_id}` case. A selected refund MUST have one open reconciliation case with deterministic ID `refund-pending:{refund_id}`, severity `high`, kind `refund_pending`, and non-secret evidence containing the refund ID and stable error category when one exists. The alert mutation MUST use a fresh reconciliation fence. It MUST set `alerted_at` and open the case in one write transaction. It MUST NOT call the Provider, increment `attempt_count`, or change an existing `next_attempt_at`, `last_error_category`, refund, order, recovery, claim, balance, or ledger value. If no retry row exists, the transaction MUST insert `attempt_count = 0`, `next_attempt_at = refund_pending_at + 1 minute`, a null `last_error_category`, and the alert time. Repeated alert scans MUST NOT create another case or change query scheduling state. A later pending or error query result MUST update the same case and MUST NOT create another case. A `succeeded` or `failed` refund result MUST close the deterministic case when it exists and delete the retry row in the same fenced transaction. Terminal cleanup MUST be idempotent.

SB-OP-6. Production MUST assign one primary Payment Operations Owner, one distinct backup owner, and one Finance Approver.

SB-OP-7. Critical cases MUST be acknowledged within 15 minutes. Exposure growth MUST be contained within 30 minutes. Provider evidence MUST be recorded within four hours.

SB-OP-8. Paid-but-unfulfilled and refund-pending cases MUST be acknowledged within 30 minutes.

SB-OP-9. Case closure, hold clearance, recovery adjustment, and unexplained settlement acceptance MUST require owner reauthentication and approval by a distinct Finance Approver.

SB-OP-10. A case and its audit trail MUST NOT be deleted through Admin.

## 11. Primary Availability

SB-HA-1. Production MUST declare `postgresql_primary` or `sqlite_primary`. Startup MUST reject a profile that does not match the database backend.

SB-HA-2. `postgresql_primary` MUST detect Primary loss within 30 seconds, have RTO at most five minutes, and have committed-state RPO zero.

SB-HA-3. PostgreSQL promotion MUST fence the old process and database writer before the replacement mounts Store endpoints.

SB-HA-4. A Store Primary process MUST hold the database lease row whose `name` is exactly `store_primary`. The lease owner ID MUST contain at least one non-whitespace character. The lease duration MUST equal 15 seconds.

SB-HA-4A. The first `store_primary` acquisition MUST insert epoch `1`. An acquisition MUST succeed only when the row is absent, the stored `expires_at` is less than or equal to the acquisition time, or the stored owner ID equals the requesting owner ID. A same-owner acquisition MUST preserve the stored epoch, including after expiry. A different-owner acquisition after expiry MUST increment the stored signed 64-bit epoch by exactly `1`. A different-owner acquisition MUST fail when the stored lease has not expired. An acquisition that would increment `i64::MAX` MUST fail and MUST leave the row unchanged.

SB-HA-4B. A renewal MUST match the exact stored lease name, owner ID, and epoch. The stored lease MUST be unexpired at the renewal time. A successful renewal MUST preserve the epoch and set `expires_at` to exactly 15 seconds after the renewal time. SQLite renewal MUST execute begin, read, update, commit, and rollback on one bound transaction connection while holding the SQLite writer lock; it MUST NOT use separate pooled-connection statements for transaction boundaries. A transient storage error MUST NOT mark the process lease as lost until bounded retry attempts are exhausted or the remaining lease TTL is at or below the five-second safety margin. Owner mismatch, epoch mismatch, missing row, and expiry MUST mark the process lease as lost immediately.

SB-HA-4C. Each Store Primary validation MUST read committed state from `store_primary_leases`. Validation MUST reject a missing row, a different owner ID, a different epoch, an `expires_at` value less than or equal to the validation time, or a process lease marked lost. SQLite acquisition and renewal MUST use one serialized, bound write transaction while holding the single writer lock; the transaction MUST acquire the SQLite write lock before the first mutation. PostgreSQL acquisition and renewal MUST lock the selected lease row with `FOR UPDATE` before changing it.
SB-HA-4C-1. A renewal loop MUST run every five seconds using a monotonic scheduler. Each renewal attempt MAY retry transient SQLite busy, connection, or transaction-context errors with delays of 100ms, 250ms, and 500ms. One retry round MUST finish within two seconds. The loop MUST continue after a recovered round and MUST include the retry attempt number in transient-failure logs. The loop MUST stop before lease expiry when retries are exhausted and MUST transition the process to `lease_lost`.
SB-HA-4C-2. A process in `lease_lost` or `recovering` MUST reject Store mutations, payment callbacks, plan admission, and reconciliation writes with `store_primary_unavailable`. It MUST stop background writers. Except during an explicit handover under SB-HA-4C-4, a terminal renewal failure MUST set the shared application shutdown signal after the lease is marked lost. The HTTP server MUST observe that signal within 250 milliseconds and start graceful shutdown. The process MUST reacquire a new fencing epoch only after process restart or an explicitly controlled recovery that confirms the old lease cannot write; two processes MUST never be writable at the same time.
SB-HA-4C-3. A transaction commit or rollback failure MUST be logged, MUST trigger a connection health check, and MUST be returned to the caller before another renewal attempt. The health-check result MUST distinguish a responsive connection from a failed connection.
SB-HA-4C-4. An explicit SIGHUP handover under deployment BG11 MUST take precedence over
a concurrent renewal failure. The renewal loop MUST check the handover flag again after
each retry round and before requesting application shutdown. Handover MUST stop renewal
without setting `background_shutdown`, including when the concurrent retry round fails.

SB-HA-4G. `GET /api/dashboard/store/admin/primary-status` MUST require an Admin session and MUST return `Cache-Control: no-store`. A Primary response MUST contain `state`, `owner_id`, `epoch`, `expires_at`, `last_successful_renewal_at`, `consecutive_failures`, and `last_failure_kind`. `state` MUST be `healthy` when the local lease is valid and has no failures, `degraded` when the lease is valid with one or more transient failures, and `lease_lost` when the local lease is marked lost or the committed lease is missing, fenced, or expired. A standby Primary under SB-HA-4D-1 MUST report `state = lease_lost` with null lease fields. A Replica response MUST contain `state = replica` and null lease fields. The endpoint MUST NOT return payment credentials or database connection details.

SB-HA-4D. Primary startup MUST generate an opaque owner ID, acquire `store_primary` after Store migrations complete, and fail startup when acquisition fails. Primary startup MUST start one renewal task before it returns the application state. The task MUST attempt renewal every five seconds and MUST continue after a transient error that is recovered within the bounded retry round. The task MUST stop when `background_shutdown` is true. When the lease transitions to `lease_lost`, the task MUST set `background_shutdown` before it stops, except during an explicit handover under SB-HA-4C-4. A Replica MUST NOT acquire or renew `store_primary`.

SB-HA-4D-1. When `MONOIZE_BOOT_STANDBY_LEASE` is exactly `1`, a Primary whose startup acquisition fails because the stored lease is held by a different unexpired owner MUST NOT fail startup. The process MUST enter standby: it MUST serve every non-lease HTTP surface, MUST reject the SB-HA-4E and SB-HA-4F surfaces with `store_primary_unavailable` unless deployment forwarding is configured under `standby-deployment.spec.md`, MUST run no Store background writer, and MUST retry acquisition at least once per second until acquisition succeeds or `background_shutdown` is true. A storage error during a standby acquisition attempt MUST log at `warn` and retry on the next tick; it MUST NOT fail the process. Any other acquisition failure MUST fail startup as SB-HA-4D requires. On acquisition the process MUST start the renewal task and every lease-gated background duty (unconfirmed-admission reaper, daily retention, the SB-OP-0 reconciliation scheduler, session cleanup, request-log retention, billing-plan grants, and revenue daily settlement) exactly once, using the shared lease handle that the lease-validating surfaces read. The default (`MONOIZE_BOOT_STANDBY_LEASE` absent or any other value) keeps SB-HA-4D fail-fast startup unchanged.

SB-HA-4E. Deployment forwarding under SD-2 MUST execute before this middleware; a forwarded request MUST use the previous process's lease validation. The Store mutation middleware MUST validate the committed Primary lease after it verifies that the node is not a Replica. A missing, lost, or expired Primary lease MUST return HTTP `503` with code `store_primary_unavailable`. A Replica Store mutation MUST continue to return HTTP `503` with code `store_write_rejected`.

SB-HA-4F. Deployment forwarding under SD-2 MUST execute before these local guards; a forwarded request MUST use the previous process's guards. Every public payment callback MUST validate the committed Primary lease before rate limiting, body extraction, Provider verification, or a database mutation. Every internal plan admission issue or confirmation request MUST validate the committed Primary lease before a database mutation. Each background unconfirmed-admission recovery iteration MUST validate the committed Primary lease before it mutates the database and MUST stop after validation fails. A missing, lost, or expired lease on either HTTP surface MUST return HTTP `503` with code `store_primary_unavailable`.

SB-HA-5. `sqlite_primary` MUST have process-restart RTO at most two minutes on a healthy host, host-loss RTO at most 30 minutes, and RPO at most 60 seconds.

SB-HA-6. Production SQLite MUST use encrypted off-host replication or backup with measured lag at most 60 seconds. An application-host-only backup MUST fail readiness.

SB-HA-7. During Primary outage, checkout, polling mutations, refund mutations, redemption, and plan admission MUST fail closed.

SB-HA-8. Recovery MUST reconcile every ambiguous attempt from the recovery boundary before checkout resumes.

SB-HA-9. Production enablement MUST require a current failover drill that proves fencing, provider retry recovery, declared RTO/RPO, and zero dual-Primary writes.

## 12. Privacy, Retention, And Access

SB-PR-1. Production checkout and callbacks MUST require a current accepted, versioned Store privacy record. Review interval MUST NOT exceed 365 days.

SB-PR-2. The privacy record MUST define jurisdiction, allowed storage regions, legal basis, retention values, reviewer, approval evidence digest, approval time, and next review time.

SB-PR-3. Encrypted raw callback bodies MUST be retained for 30 days.

SB-PR-4. Source IP and User-Agent MUST be retained for 90 days and then deleted. A retained pseudonym MUST be keyed, non-reversible, purpose-bound, and use a separate rotating key.

SB-PR-5. Parsed provider events, orders, ledger entries, refunds, disputes, chargebacks, settlement reports, recovery claims, and financial audits MUST be retained for seven years unless the accepted jurisdiction record defines another period.

SB-PR-6. Redemption reveal, copy, and export audits MUST be retained for two years.

SB-PR-7. Reauthentication grant hashes MUST be deleted no later than 24 hours after expiry.

SB-PR-8. Raw callback bodies MUST be readable only by assigned Payment Operations during an incident and authorized Privacy or Security reviewers.

SB-PR-9. Every evidence read, export, retention override, and legal-hold operation MUST write an immutable access audit with actor, role, scope, reason, time, and result.

SB-PR-10. Payment data, callback evidence, backups, logs, and encryption keys MUST remain inside privacy-record allowed regions.

SB-PR-11. A Store Primary scheduler MUST attempt one retention run at 03:00 UTC each day. A Replica MUST NOT run the scheduler. Each run MUST use the current accepted privacy record at its start time. A missing current record MUST produce a failed run with policy version `unavailable` and error category `privacy_policy_unavailable`. An invalid retention document MUST produce a failed run with error category `privacy_policy_invalid`.

SB-PR-11A. A retention run state MUST be `running`, `succeeded`, or `failed`. A run MUST record its ID, worker owner ID, policy version, counts by data class, oldest remaining time, error category, start time, and completion time. `running` MUST have no completion time or error category. `succeeded` MUST have a completion time and no error category. `failed` MUST have both. The five count keys MUST be `raw_callback_bodies`, `network_metadata`, `financial_records`, `redemption_audits`, and `expired_reauth_grants`. A count MUST equal the number of root records deleted or cleared by the committed run.

SB-PR-11B. One run MUST inspect and mutate at most 500 unheld root records in each count class. Associated child rows deleted with one financial root record do not consume another root-record slot. The run MUST select candidates in ascending retention timestamp and ID order. The run MUST exclude an identifier when `store_legal_hold_items` links it to a hold for the same data class and `starts_at <= run_time < expires_at`. The selection and mutation MUST occur in one transaction. A failure MUST roll back every deletion in that run. Repeating a run against unchanged data MUST not change already cleared fields and MUST not fail because an earlier run deleted a candidate.

SB-PR-11C. `raw_callback_bodies` MUST clear `raw_format_version`, `raw_key_id`, `raw_nonce_base64`, and `raw_ciphertext_base64` on `store_provider_events` whose `received_at` is at least 30 days old. `network_metadata` MUST clear `source_ip` and `user_agent` on events whose `received_at` is at least 90 days old. The event row, body digest, parsed event, verification result, and projection state MUST remain.

SB-PR-11D. `expired_reauth_grants` MUST delete grants after `expires_at + expired_reauth_grant_hours <= run_time`. `redemption_audits` MUST delete access audits with action `redemption_reveal`, `redemption_copy`, or `redemption_export` after `created_at + 730 days <= run_time`.

SB-PR-11E. `financial_records` MUST apply `financial_records_days` to terminal orders, provider events, billing ledger entries, refunds, recovery claims, settlement reports, and non-redemption access audits. A terminal order has payment state `closed` or `refunded`. Deleting an order root MUST also delete its payment attempts, event applications, refunds, refund-query retries, reward-recovery rows, and recovery-claim rows that are not independently held. Deleting a settlement-report root MUST delete its unheld settlement lines. Active entitlements and their generation records MUST NOT be deleted by this job.

SB-PR-11F. `oldest_remaining_at` MUST be the minimum retention timestamp among callback evidence, grants, redemption audits, and financial root records remaining after the run. It MUST be null only when no such record remains. The run MUST write one immutable `retention_run` access audit with system actor `_monoize_retention_job`, system role, the run ID and counts as scope, reason `scheduled_retention`, the completion time, and result `succeeded` or `failed`. An Admin-triggered run MUST instead record that Admin as actor, role `admin`, and the submitted reason.

SB-PR-11G. The retention runtime state MUST persist one current worker claim, the last run ID, consecutive failure count, checkout pause flag, active critical alert ID, latest containment ID, and update time. A worker claim MUST prevent two runs from deleting concurrently. A claim owned by another Store Primary owner MUST be finalized as failed with error category `interrupted` before the new owner starts a run.

SB-PR-12. A succeeded run MUST set the persisted consecutive failure count to zero. A failed or interrupted run MUST increment it and MUST NOT clear an existing checkout pause. When a failed run makes the count at least three while checkout is not already paused, the transaction MUST create one critical retention alert and set `checkout_paused = true`. Process restart MUST preserve the counter, alert, and pause.

SB-PR-12A. When `checkout_paused = true`, a new order or a new payment attempt MUST fail before insertion with HTTP `503` and code `store_retention_paused`. An idempotent replay of an existing order or terminal payment attempt MAY return its existing result. Callback ingestion, reconciliation, refunds, and retention operations MUST remain available.

SB-PR-12B. Containment MUST require an Admin session, a five-minute reauthentication grant with scope `retention_operation`, a nonempty reason of at most 2000 Unicode scalar values, and a lowercase SHA-256 evidence digest. It MUST create an immutable containment record with actor, reason, evidence digest, active alert ID, and creation time. It MUST mark that alert contained and set `checkout_paused = false`. It MUST NOT change the consecutive failure count. A later failed run while the count is at least three MUST create a new critical alert and pause checkout again. A successful run MUST NOT clear a pause without containment.

SB-PR-13. A legal hold data class MUST be one of the five count keys in SB-PR-11A. A hold MUST contain between 1 and 100 distinct identifiers. Each identifier MUST be a nonempty trimmed string of at most 255 bytes. The exact JSON request MUST contain `data_class`, `identifiers`, `reason`, `requesting_authority`, `requester_id`, `approver_role`, `expires_at`, and `extends_hold_id`. `approver_role` MUST be `privacy` or `legal`. The authenticated Admin is the approver and MUST differ from `requester_id`. `reason` and `requesting_authority` MUST be nonempty trimmed strings of at most 2000 and 500 Unicode scalar values, respectively. `expires_at` MUST be a future RFC3339 instant. The server MUST set `starts_at` and `created_at` to its current time.

SB-PR-13A. A legal-hold approval MUST require an Admin session, the SB-S-2 Origin check, a Store Primary, and a five-minute reauthentication grant with scope `legal_hold`. Creation MUST atomically insert the immutable hold, one normalized item per identifier, and a `legal_hold_create` access audit. The audit actor MUST be the approver. Its role MUST equal `approver_role`. Its scope MUST contain the hold ID, data class, identifiers, requester ID, requesting authority, and extended hold ID. Its reason and result MUST equal the submitted reason and `succeeded`.

SB-PR-14. A hold applies only while `starts_at <= evaluation_time < expires_at`. Expiry MUST require no mutation. A hold MUST NOT restore or recreate deleted data. `extends_hold_id = null` creates an initial approval. A nonnull `extends_hold_id` MUST reference an existing hold with identical data class and identifiers, and the new expiry MUST be later than the referenced expiry. An extension MUST insert a new hold and approval audit. No API operation may update, delete, reactivate, or extend an existing hold row.

SB-PR-14A. `GET /api/dashboard/store/admin/retention` MUST require an Admin session, use `Cache-Control: no-store`, and return `{ "status": StoreRetentionStatus, "runs": StoreRetentionRun[], "holds": StoreLegalHold[], "containments": StoreRetentionContainment[] }`. Each list MUST use descending creation order and contain at most 100 records. Hold objects MUST contain an `active` Boolean evaluated at request time.

SB-PR-14B. `POST /api/dashboard/store/admin/retention/runs` MUST require an Admin session, the SB-S-2 Origin check, a Store Primary, and a `retention_operation` reauthentication grant. Its exact JSON body MUST be `{ "reason": string }`, where `reason` is nonempty after trimming and contains at most 2000 Unicode scalar values. It MUST start one bounded run. An active worker claim MUST return HTTP `409` with code `retention_run_active`.

SB-PR-14B1. A run that started MUST return HTTP `201` with the complete `StoreRetentionRun` record, including when its `state` is `failed`. The status code reports that the run record was created; the record's `state` and `error_category` report the outcome. The endpoint MUST NOT report a failed outcome through an error status, because the caller needs the created record to identify the failure, and MUST NOT omit `error_category` for a failed run. A caller MUST read `state` to determine the outcome; HTTP `201` alone MUST NOT be treated as a successful run.

SB-PR-14C. `POST /api/dashboard/store/admin/retention/containment` MUST enforce SB-PR-12B. Its exact JSON body MUST be `{ "reason": string, "evidence_digest": string }`. No active uncontained critical alert MUST return HTTP `409` with code `retention_containment_unavailable`.

SB-PR-14D. `POST /api/dashboard/store/admin/retention/legal-holds` MUST enforce SB-PR-13 and SB-PR-13A. It MUST return the created hold with HTTP `201`. An invalid extension, duplicate identifier, self-approval, past expiry, unknown data class, unknown field, or invalid reauthentication grant MUST create no hold item.

## 13. Security And API Surface

SB-S-1. Callback rate limiting MUST allow at most 600 requests per minute per Channel and source IP. Signature verification remains mandatory.

SB-S-2. Every cookie-authenticated Store mutation MUST require JSON or the documented multipart icon type. One mutation-only Router middleware MUST enforce Origin and the SB-S-9 repository write guard before Axum runs any JSON, multipart, or other body extractor. An `Origin` header that is present MUST equal the configured public origin; a malformed or nonmatching `Origin` MUST return HTTP `403` with code `store_origin_invalid` before body parsing or repository mutation. An absent `Origin` header MUST be accepted, because the dashboard session cookie is `HttpOnly; Secure; SameSite=Strict`: a cross-site request cannot carry it, so a cookie-authenticated mutation without `Origin` cannot be a cross-site forgery (in-app webview browsers omit `Origin` on same-origin POSTs). The cookie attributes are normative: if the session cookie is ever changed away from `SameSite=Strict`, an absent `Origin` MUST return HTTP `403` with code `store_origin_invalid` again. An `Authorization: Bearer` authenticated mutation MUST NOT require `Origin`, including when a Cookie header is also present.

SB-S-2A. `DELETE /api/dashboard/store/admin/products/{id}`, `DELETE /api/dashboard/store/admin/payment-channels/{id}`, and `POST /api/dashboard/store/admin/redemption-codes/{id}/revoke` MUST require `Content-Type: application/json` and an exact empty JSON object `{}`. Each body type MUST use `deny_unknown_fields`. A missing body, non-JSON content type, malformed JSON, JSON `null`, array, scalar, or object with any field MUST return HTTP `400` with code `invalid_request`. The SB-S-2 middleware MUST run before this validation.

SB-S-3. Reveal, export, refund, reprocess, exchange-rate recovery, key rotation, credential update, hold clearance, and financial case closure MUST require a five-minute scoped reauthentication grant.

SB-S-4. Sensitive responses MUST set `Cache-Control: no-store`. Logs and API responses MUST redact secrets, signatures, full codes, and reauthentication tokens.

SB-S-5. Session user endpoints MUST include:

- `GET /api/dashboard/store/catalog`
- `GET /api/dashboard/store/exchange-rate`
- `GET /api/dashboard/store/entitlement`
- `GET /api/dashboard/store/orders`
- `POST /api/dashboard/store/orders`
- `GET /api/dashboard/store/orders/{id}`
- `POST /api/dashboard/store/orders/{id}/attempts`
- `POST /api/dashboard/store/redeem`
- `GET /api/dashboard/store/icons/{id}`

SB-S-6. Public provider endpoints MUST include `POST /api/store/callbacks/{channel_id}`. They MUST NOT require a dashboard session.

SB-S-7. Admin endpoints MUST include product, settings, Channel, credential, compliance, capability, order query, event reprocess, unpaid close, refund, redemption generation, reveal, export, revocation, reconciliation case, rate governance, privacy, retention, and readiness operations under `/api/dashboard/store/admin/`.

SB-S-8. `POST /api/dashboard/store/admin/orders/{id}/complete` MUST NOT exist.

SB-S-9. User and Admin mutations MUST call the Store repository Primary write guard before mutation. A read-only Store repository MUST return `StoreBillingError::WriteRejected`. Every Store mutation route on a Replica MUST map this result to HTTP `503` with code `store_write_rejected` and MUST commit no business-table mutation. A Replica MUST mount only these Store mutation routes under `/api/dashboard/store`; Store reads and every other dashboard route remain disabled by `primary-replica-deployment.spec.md` D1.

## 14. Frontend

SB-UI-1. `/dashboard/store` MUST contain Balance, Plan, and Redemption modes in one sliding segmented control.

SB-UI-2. Balance and Plan MUST use a two-column purchase area. Payment methods MUST occupy one independent full-width row at the bottom of the left column.

SB-UI-3. Redemption MUST render no payment method and no order summary.

SB-UI-4. The order summary MUST keep stable dimensions between Balance and Plan.

SB-UI-5. CNY and USD selection MUST be one shared state for balance, usage, product, bonus, actual received, plan price, plan quota, and order summary.

SB-UI-6. Simplified Chinese MUST use `实得` for recharge plus bonus.

SB-UI-7. Store and Store Management reads MUST use SWR and initial skeletons. User mutations MUST use optimistic cache updates with rollback when the mutation can be reversed locally.

SB-UI-8. Payment creation MUST NOT optimistically credit balance or activate a plan.

SB-UI-9. An unpaid order MUST poll every two seconds until payment state is `paid`, `refunded`, or `closed`, fulfillment state is `fulfilled` or `failed`, expiration, or unmount. Completion MUST revalidate order, balance, user, and entitlement data.

SB-UI-9A. The browser MUST persist one pending checkout fingerprint, order idempotency key, attempt idempotency key, and optional order ID in tab-scoped session storage. A retry with the same user and canonical purchase input MUST reuse these values. Only `payment_configuration_unavailable` and `payment_provider_rejected` MAY rotate the attempt key after the order ID is known. Network failures, `internal_error`, `payment_provider_ambiguous`, and unrecognized errors MUST retain both keys.

SB-UI-10. Alipay and Stripe MUST use provider redirects. WeChat desktop MUST use a QR modal. WeChat mobile MUST use H5 redirect.

SB-UI-10A. The WeChat Native QR modal MUST render the exact verified Provider payload as a scannable SVG QR code. It MUST NOT display the payload as plain text instead of a QR code.

SB-UI-10B. A QR modal MUST name the payment method the attempt was created with, taken from
the selected payment option rather than from a fixed string. An EPay Channel may enable both
`alipay` and `wxpay`, and the returned code is bound to the `type` sent upstream, so scanning
it with the other wallet is rejected by the gateway with a method-not-supported error. Naming
the wrong method therefore directs the payer into a guaranteed failure. When the selected
option carries no method, the modal MUST use an instruction that names no method. The modal
MUST NOT apply an exchange rate, alter, or re-encode the payload.

SB-UI-10C. While the QR dialog is open and order polling is active, the dialog MUST display
the elapsed whole seconds since polling began and the number of status checks issued, both
updating at least once per second. When polling observes `payment_state = paid`, the dialog
MUST display a success indicator for 1800 ms and then close. When polling reaches any other
terminal condition (expiry, failure, or a 404 order), the dialog MUST close immediately
without a success indicator.

SB-P-Q1. The buyer MUST be able to trigger a provider payment query for their own order
through `POST /dashboard/store/orders/{id}/query`. The endpoint MUST reject a caller who does
not own the order with the same not-found response used for an unknown order. It MUST reject
an order whose `contract_version` is not 2, and an order whose `payment_state` is not
`unpaid`, without contacting the provider.

Waiting only for the provider callback leaves the buyer with no signal: one observed order
received its callback 2 minutes 14 seconds after creation, while another received it in 12
seconds. The provider is authoritative about payment either way, so the buyer polling loop
asks it directly instead of waiting.

SB-P-Q2. A buyer query MUST contact the provider at most once per `SB-P-Q3` interval per
Attempt, regardless of how many callers request it. The last provider contact time MUST be
persisted on the Attempt as `buyer_query_at`, so the limit survives a restart and cannot be
bypassed by opening more tabs. When the interval has not elapsed, the endpoint MUST return
the current order without contacting the provider.

SB-P-Q3. The minimum interval between provider contacts for one Attempt is 8000 ms.

SB-P-Q4. A buyer query whose Channel lacks a `supported` `payment_query` merchant capability
MUST return the current order without contacting the provider, and MUST NOT fail.

SB-P-Q5. When the provider reports the payment as paid, a buyer query MUST apply it through
the same verified-query event path an Admin query uses, so fulfillment, idempotency, and
ledger effects are identical regardless of who triggered the query. A buyer query MUST NOT
close an Attempt or an order under any provider response; closing remains Admin-only.

SB-P-Q6. A buyer query MUST return the order as it stands after the query, using the same
representation as `GET /dashboard/store/orders/{id}`. An `Ambiguous` provider response MUST
return the current order rather than an error, because the buyer cannot act on it.

SB-UI-10E. The success state of the QR dialog MUST render an animated check mark that draws
its ring and its tick in sequence, followed by the success label. Under
`prefers-reduced-motion: reduce` the mark MUST still reach its completed state. The mark is
the confirmation the buyer waits for, so it MUST NOT depend on the dialog remaining open
beyond the SB-UI-10C hold.

SB-UI-10D. In the order history list and in every order detail surface, an order whose
`quote.product.kind` is `balance` MUST title the order with the Coin amount derived from
`quote.product.balance.actual_received_minor`, denominated in `quote.product.price_currency`.
It MUST NOT title the order with `quote.product.name`. An order of any other kind MUST title
the order with `quote.product.name`.

A custom-amount recharge reuses a fixed tier's product row as its carrier, so the snapshot's
`name` and `price_minor` describe the carrier tier rather than the amount bought. Titling a
1 CNY custom recharge with the carrier's name displays `50` for an order that delivered 1
Coin. The balance block is the only part of the snapshot that states what the buyer received.

SB-UI-11. Store Management MUST have Products, Payment Channels, Orders, and Redemption Codes child pages with an animated active indicator.

SB-UI-12. Orders MUST show payment and fulfillment state separately. It MUST expose query, verified event reprocess, close, refund, dispute, hold, and case actions according to role and state. Close MUST be hidden when another Attempt for the order has state `created` or `presented`. Each user-triggered order or refund mutation MUST use an SWR optimistic cache value, roll back that value on error, apply the returned record to the cache, and then revalidate order detail and list data. A successful refund query MUST clear the reauthentication password. It MUST NOT show manual Complete.

SB-UI-13. Generated redemption codes MUST remain fully visible in the generation result
until the Admin closes it. The result MUST render every complete code, one Copy action per
code, and one Copy All action. Closing the result MUST clear plaintext component state.
List rows MUST remain masked until scoped reveal.

SB-UI-14. Each unused Admin list row with `can_reveal = true` MUST expose Reveal and Copy
actions. Either action MUST open a dialog that requires the current Admin password before it
requests a `redemption_access` grant. Reveal MUST call the reveal endpoint with action
`reveal`. Copy MUST call it with action `copy` before it writes the returned code to the
clipboard. The dialog MUST show the complete returned code until it closes. Closing the
dialog MUST clear the password, grant token, and plaintext code from component state. A row
with `reveal_unavailable_reason = legacy_digest_only` MUST show a localized legacy digest-only
label and MUST NOT expose Reveal, Copy, or Export. An unused row MAY expose Revoke.

SB-UI-13A. An unused revealable v2 list row MUST expose a labeled reveal or copy action.
The action MUST obtain a five-minute `redemption_access` reauthentication grant from the
current Admin password. Plaintext MUST render only in a modal and MUST clear when that modal
closes.

SB-UI-13B. A v1 list row MUST render a localized legacy digest-only label. It MUST explain
that the full code cannot be recovered and expose only valid non-reveal actions such as
revoke. The UI MUST NOT fabricate or reconstruct plaintext.

SB-UI-13C. A `store_redemption_encryption_unavailable` response MUST render one localized
readiness message that names `MONOIZE_STORE_PAYMENT_KEYS_JSON` for the operator. It MUST NOT
display the raw backend message or any key value.

SB-UI-14. Main Store cards MUST use a 16-pixel radius. Interactive controls SHOULD use a 12-pixel radius. Product lists MUST expand naturally.

SB-UI-15. Store Management MUST edit Channel metadata and Channel credentials with separate save actions. The credential form MUST render only for an existing official Channel. It MUST start empty whenever the dialog opens. It MUST NOT render a saved credential value. Credential save MUST request the current Admin password, obtain a `credential_update` reauthentication grant, replace the credential, optimistically mark the Channel disabled, and revalidate the Channel list.

SB-UI-16. The Store Management Payment Channels child page MUST expose Privacy Records and official-Channel Readiness as modal dialogs. It MUST NOT add another Store Management child page. The Privacy Records dialog MUST load the SB-C-34 list through SWR, render a skeleton before data arrives, show existing immutable records, and expose the exact SB-C-34 append form. It MUST NOT send a request unless every client-supplied field satisfies the SB-C-34 bounds. A successful append MUST optimistically insert a temporary record, roll back on error, replace it with the server record, and keep the dialog open. Server-bound record fields MUST NOT be editable. The append action MUST be disabled while any append-form field is empty after trimming.

SB-UI-17. Each Alipay, WeChat Pay, or Stripe Channel row MUST expose a Readiness action. An HTTP Adapter row MUST NOT expose that action. The Readiness dialog MUST load SB-C-35 through SWR and render a skeleton before data arrives that matches the ready list-and-form layout. It MUST constrain currency and checkout-action controls to the selected adapter, expose every client-supplied SB-C-35 field, and MUST NOT expose server-bound credential or Admin identifiers as editable fields. When the Privacy Records list is empty, it MUST render an inline empty state that states a privacy record is required and MUST expose a primary action that opens the Privacy Records dialog. It MUST NOT send a request unless the metadata, amount limits, adapter constraints, and validity period satisfy SB-C-28 and SB-C-35. A successful save MUST optimistically replace the SWR readiness value, roll back on error, apply the server profile, and revalidate the Payment Channel list so effective availability updates without a manual refresh.

SB-UI-18. Each Alipay, WeChat Pay, or Stripe Channel row MUST expose a Compliance action. An HTTP Adapter row MUST NOT expose that action. The Compliance dialog MUST load SB-C-20 through SWR and render a skeleton before data arrives that matches the ready terms-and-confirmation layout. It MUST display the Store payment terms acknowledgement summary, `current_terms_version`, and the latest non-invalidated confirmation when present. Confirmation MUST require an explicit terms-review acknowledgment checkbox and the current Admin password with governance-specific labeling and reauthentication help text. It MUST NOT rely on a version hash alone. It MUST NOT expose credential fields. Confirmation MUST obtain a `compliance_confirm` reauthentication grant, and send the exact SB-C-21 body `{ "confirmed": true, "terms_version": current_terms_version }`. A successful confirmation MUST optimistically replace the SWR compliance value, roll back on error, apply the server record, and revalidate the Payment Channel list.

SB-UI-19. Each Alipay, WeChat Pay, or Stripe Channel row MUST expose a Capabilities action. An HTTP Adapter row MUST NOT expose that action. Compliance, Capabilities, and Readiness MUST be reachable through one labeled Governance control with visible item labels on each Channel row. Icon-only controls without visible labels MUST NOT be the sole affordance for those actions. The Capabilities dialog MUST load SB-C-22 through SWR and render a skeleton before data arrives that matches the ready selector, summary list, and editor layout. It MUST expose every SB-C-23 capability kind, show each saved record state, and expose the exact client-supplied SB-C-23 fields for the selected capability. It MUST NOT expose server-bound credential plaintext, encrypted credential fields, or editable `merchant_account_digest`, `verifier_admin_id`, `verified_at`, or `expires_at`. It MUST NOT send a request unless the selected capability input satisfies SB-C-23 bounds. A successful save MUST optimistically replace the saved capability in the SWR list, roll back on error, apply the server record, and revalidate the Payment Channel list.

SB-UI-20. The Store Management Payment Channels child page MUST expose a Retention action that opens a modal Retention dialog. The dialog MUST load SB-PR-14A through SWR and render a skeleton before data arrives that matches the ready status-and-form layout. It MUST display the consecutive failure count, the checkout pause state, the run count, and the legal hold count from the SB-PR-14A response. When `runs` is nonempty, it MUST also display `runs[0].state`, `runs[0].started_at`, and, when `runs[0].error_category` is nonnull, that category, because the failure count alone does not identify why a run failed and the operator cannot fix the cause without it. When `runs` is empty, it MUST display one static localized statement that no run has been recorded. It MUST expose one current Admin password input and one reason input. It MUST render one static localized statement that a retention run permanently deletes records past their retention period, and, while `status.checkout_paused` is `true`, one static localized statement that containment clears the alert and resumes checkout. A run request MUST obtain a `retention_operation` reauthentication grant and send the exact SB-PR-14B body. It MUST NOT send a run request when the trimmed reason is empty. The run action MUST be disabled while the trimmed password or the trimmed reason is empty. The evidence digest input and the containment action MUST render only while `status.checkout_paused` is `true`. A containment request MUST obtain a `retention_operation` reauthentication grant and send the exact SB-PR-14C body. It MUST NOT send a containment request unless the trimmed reason is nonempty and the trimmed evidence digest contains exactly 64 characters. The containment action MUST be disabled unless the trimmed password is nonempty, the trimmed reason is nonempty, and the trimmed evidence digest contains exactly 64 characters. A containment mutation MUST apply an optimistic SWR value that sets `checkout_paused` to `false`, clears `active_alert`, and prepends the containment record. It MUST roll back that value on error, apply the server record, and then revalidate the retention overview. A run mutation MUST NOT apply an optimistic value; it MUST revalidate the retention overview after the request resolves, including when the returned run failed. The run action MUST read `state` from the returned SB-PR-14B1 record. A `succeeded` state MUST report success. A `failed` state MUST report failure and MUST include the returned `error_category`, because an HTTP `201` for a failed run would otherwise read as success and three such runs pause checkout under SB-PR-12. A `failed` run MUST NOT clear the reason input, so the operator can retry after fixing the cause. A successful run or containment MUST clear the password and reason inputs.

SB-UI-21. Each Payment Channel row MUST display the configured enabled state and `effective_available` as separate values. When `effective_available` is `false` and `unavailable_reasons` is nonempty, the row MUST list every reason in ascending code order. A reason whose code is one of the fixed SB-C-25 through SB-C-30 codes, or matches `capability_<kind>_<issue>` with `<issue>` in `missing`, `invalid`, `not_supported`, `expired`, `credential_mismatch`, MUST render a localized label as the primary text with the raw code as secondary text. A reason with any other code MUST render the raw code.

## 15. Migration And Verification

SB-M-1. A read-only preflight MUST report legacy order states, timestamp consistency, orphan references, duplicate order numbers, unknown callback objects, partial payment fields, and schema collisions.

SB-M-2. Preflight MUST abort on unknown order state, inconsistent timestamp, orphan, unknown callback table, partial provider data, or credential format outside the documented legacy contract.

SB-M-3. Legacy `completed` orders MUST migrate to payment `paid`, fulfillment `fulfilled`, contract version 1.

SB-M-4. Legacy `pending` and `cancelled` orders MUST migrate to payment `closed`, fulfillment `pending`, contract version 1.

SB-M-5. New callbacks, query, refund, and late payment MUST reject version-1 closed orders.

SB-M-6. Legacy Channel secrets MUST become retired encrypted credential versions and MUST NOT be used by an official adapter.

SB-M-7. Migration MUST disable every Channel and create a count manifest. Any preflight or count mismatch MUST roll back the transaction.

SB-M-8. SQLite and PostgreSQL MUST implement equivalent state, recovery, immutability, and uniqueness constraints.

SB-M-9. Local automated verification MUST use SQLite and MUST NOT start PostgreSQL on the user's computer.

SB-M-10. Production Channel enablement MUST require passing migration, callback, adapter, event-order, recovery, reconciliation, quota, key rotation, capability, failover, privacy, retention, backup, and restore gates from the design.

SB-M-11. Code deployment and production Channel enablement are separate approvals. A deployed Channel MUST remain disabled until its provider-specific controlled payment and refund pass.

SB-M-12. Before the first real provider payment, rollback MAY restore the old image and matching database backup. After a real payment or refund, database rollback is forbidden; operations MUST stop checkout and reconcile or forward-fix every provider transaction.

SB-M-13. Migration `058` down MUST remove `idx_store_legal_holds_expiry` and `idx_store_retention_runs_started` before it returns. One migration `058` down followed by one migration `058` up MUST succeed and MUST recreate both indexes exactly once.

SB-M-14. SQLite migration `051` MUST succeed with `PRAGMA foreign_keys = ON` when one or more legacy `store_orders` rows reference legacy `store_payment_channels` rows. It MUST preserve every order and its Channel reference, replace both legacy table shapes, disable every migrated Channel, and leave `PRAGMA foreign_key_check` empty.

SB-M-15. Migration `059` MUST repair a database in which the released legacy migration `049` is recorded as applied, `store_plan_entitlements` exists, and `store_plan_entitlement_generations`, `store_plan_entitlement_current`, and `store_plan_entitlement_lifecycle` do not exist. It MUST migrate each legacy entitlement to generation `1`, preserve its user, product, name, time range, Group JSON, quota JSON, source kind, and source ID, convert `cny_per_usd` to positive canonical integer rate numerator and denominator values, and reduce the values by their greatest common divisor. It MUST create one unsuspended lifecycle row and one current row, and remove `store_plan_entitlements`. It MUST reject a partial or mixed legacy/current entitlement schema. On SQLite and PostgreSQL, it MUST normalize every migration-`051` order expiry whose date and time are separated by one space to the same UTC instant in RFC3339 form. It MUST leave `PRAGMA foreign_key_check` empty on SQLite. A database that already has the complete current entitlement schema and no legacy table MUST retain its entitlement rows.
### 3.1 EPay Protocol

SB-EP-1. One EPay Channel MUST store one gateway base URL, merchant ID, encrypted merchant secret, callback configuration, and independent enabled switches for `alipay` and `wxpay`.

SB-EP-2. Checkout MUST submit `POST {gateway}/mapi.php` as `application/x-www-form-urlencoded`. It MUST include the documented `pid`, `type`, `out_trade_no`, `notify_url`, `return_url`, `name`, `money`, `clientip`, `device`, `param`, `sign`, and `sign_type` fields when applicable.

SB-EP-3. An outbound `money` field MUST contain exactly two decimal places produced from integer CNY fen. EPay checkout MUST reject every non-CNY order.

SB-EP-3A. An inbound `money` value, in a callback or a query response, MUST be parsed as CNY
yuan with an optional fractional part of zero, one, or two digits, and MUST convert to
integer fen exactly. `"1000"` MUST parse as `100000` fen, `"1000.5"` as `100050`, and
`"1000.50"` as `100050`. A value with three or more fractional digits, an empty integer part,
an empty fractional part after a decimal point, a sign, exponent, whitespace, separator, or
any non-digit MUST be rejected. Parsing MUST NOT use `f32` or `f64` per SB-0.9.

The inbound grammar MUST NOT reuse the outbound two-decimal requirement of SB-EP-3. A
gateway reports a whole-yuan amount without a fractional part, so requiring two decimals on
input rejects the callback for every whole-yuan order while accepting one whose amount
happens to carry fen. That admits the smallest test payment and rejects every ordinary
recharge, which is indistinguishable from the gateway never calling back.

SB-EP-4. EPay signing and verification MUST remove `sign`, `sign_type`, and empty values; sort remaining field names by ascending ASCII bytes; join unencoded `key=value` pairs with `&`; append the merchant secret without another separator; and compute lowercase MD5.

SB-EP-5. A successful create response MUST contain `code = 1` and exactly one usable action from `qrcode`, `payurl`, or `urlscheme`. An absent action MUST fail creation and MUST NOT fulfill the order.

SB-EP-6. The EPay callback MUST accept GET parameters and MUST process success only when `trade_status = TRADE_SUCCESS`. It MUST verify signature, merchant ID, local order number, provider trade number, exact amount, and expected method before applying payment.

SB-EP-7. A callback MUST return plain text `success` only after verified evidence and the idempotent payment transition commit. Invalid or transiently uncommitted callbacks MUST NOT return `success`.

SB-EP-8. Callback, order query, and reconciliation MUST use the same state-transition and fulfillment services. Repeated or concurrent evidence MUST fulfill one order at most once.

SB-EP-9. After a timeout, disconnect, or HTTP `5xx`, the service MUST query the original provider order before retrying payment creation or refund.

SB-EP-10. The EPay adapter MUST support merchant validation, order query, full refund, available-method discovery, and reconciliation. It MUST support refund query and settlement retrieval when the configured gateway reports those capabilities.

SB-EP-11. The gateway URL MUST reject credentials in the URL, loopback, link-local, and private destinations. Outbound requests MUST reject a cross-host redirect and MUST validate resolved addresses before each connection.

SB-C-37. The required merchant capability set depends on the adapter kind. A Channel MUST prove
`payment_query`, `refund`, `refund_query`, and `settlement_report` as `supported`, except that
an `epay` Channel MUST prove none of them. The EPay protocol defines no refund-status query,
and settlement retrieval exists only on gateways that implement `act=settle`, so requiring
either as `supported` would make every EPay Channel permanently unavailable. EPay availability
is decided by the readiness profile alone (SB-C-38), so no capability verification is required.
An Admin MAY still record a capability for a non-EPay Channel as `unsupported` or `manual`, and
such a record MUST NOT block availability.

SB-C-38. The licence, runtime, availability, and privacy gates all require a SHA-256 digest of a
document issued by a third party — a card-network attestation, a payment licence, an audited
availability report, a privacy review. Such documents exist for a regulated acquirer (for
example Stripe), so an adapter of any kind other than `epay` MUST supply a current readable
`store_channel_readiness_profiles` row whose `privacy_record_id` references a current record and
whose `license_evidence_digest`, `runtime_evidence_digest`, and `availability_evidence_digest`
are all valid. An `epay` adapter MUST NOT be required to supply any of them, because EPay is a
self-hosted gateway protocol with no issuer for those documents; requiring them would leave
every EPay Channel permanently unavailable with reasons its operator can never clear. An
`epay` Channel MUST be gated only on facts the deployment itself establishes: the Channel is
enabled, an active credential matches the adapter, the terms are confirmed at the current
version, and a current readiness profile supplies the currency, amount, and checkout-action
metadata. On write, an adapter subject to the gates MUST supply every attestation and an exempt
adapter MUST supply none; a partially filled profile MUST be rejected rather than stored, so an
exempt Channel can never carry a digest that no gate will read.

SB-EP-10A. The EPay refund protocol defines `code = 1` as success and every other value as failure without defining an error-code taxonomy. A refund response with HTTP 200 and `code = 1` MUST map to a succeeded refund. A refund response with HTTP 200 and any other `code` MUST map to an ambiguous refund and MUST remain reconcilable. It MUST NOT map to a terminal refund failure, because an undocumented code can report a transient gateway condition, an unsynchronized order, or an already-refunded order, and a terminal rejection would abandon a refundable amount with no later correction path.

SB-EP-11A. The gateway URL scheme MUST be `https`. An `http` gateway URL MUST fail configuration validation. The EPay query endpoints (`act=query`, `act=order`, `act=orders`, `act=settle`, `act=paytype`) send the merchant secret as the `key` query parameter, so an `http` gateway would expose that secret in plaintext to every intermediate node. The merchant secret MUST NOT be moved to a POST body to permit `http`, because the documented EPay protocol defines those endpoints as GET.

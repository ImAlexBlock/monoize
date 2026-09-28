# CNY Accounting Implementation Plan

> **For agentic workers:** Use subagent-driven-development or executing-plans. Execute the checked tasks in order; parallelize independent reads and isolated implementation scopes.

**Goal:** Make CNY the native wallet and billing currency without changing the CNY value of existing assets at activation or interrupting streaming requests.

**Architecture:** Implement explicit currency and epoch amounts. Publish a USD-compatible bridge before activating the CNY epoch with one immutable exchange-rate snapshot. Keep original historical contracts and normalize their values at defined boundaries.

**Tech Stack:** Rust, SeaORM, SQLite/PostgreSQL, React, TypeScript, Bun.

**Spec:** `spec/accounting-currency.spec.md`.

## Global Constraints

- One CNY equals 1,000,000,000 nano-CNY.
- Existing USD assets use the actual activation snapshot, signed half-away-from-zero rounding, and checked i128 results.
- Do not use floating point, rename source vendor prices, or rewrite signed historical contracts.
- All production updates use the blue-green swap and preserve existing connections.
- Write ordinary project artifacts only inside the repository.
- Keep affected subsystem specs, frontend contracts, and all four documentation locales aligned.

## Review Focus

- A long request admitted in USD settles after activation without losing or repeating its charge.
- Rounding a converted wallet must not silently break the historical ledger chain.
- An old durable message replays with its original identity and currency.
- Refunds return exactly the accounting amount charged across an exchange-rate update.
- Native CNY operations remain available when no current FX snapshot exists.

## Task 1: Exact currency amounts

- [ ] Define the currency/epoch envelope and exact signed nano conversion API in `src/accounting/money.rs`.
- [ ] Add tests for `-1 * 0.5 == -1`, `i128::MIN * 1 == i128::MIN`, and `i128::MAX * 1 == i128::MAX`.
- [ ] Add a case where multiplication overflows i128 but the divided result fits, and one where the final result does not fit.
- [ ] Use a widened integer intermediate; retain canonical decimal rates and no floating-point conversions.
- [ ] Verify conversion and existing money tests.

## Task 2: Epoch persistence and activation barrier

- [ ] Introduce immutable accounting epoch records and explicit legacy/current schema layouts.
- [ ] Serialize monetary operations against activation with a short accounting gate.
- [ ] Add loopback control status and idempotent activation operations with a verified deployment barrier.
- [ ] Capture the actual valid FX snapshot inside activation and persist its exact rational and source time.
- [ ] Test failed activation rollback, duplicate activation, unsupported writers, and stale cache invalidation.

## Task 3: Wallet and ledger normalization

- [ ] Route user/key balances, grants, transfers, limits, recovery amounts, and administration through epoch-aware storage.
- [ ] Convert each mutable row once within its first CNY transaction; add a unique redenomination event per wallet.
- [ ] Preserve historical ledger rows and return explicitly denominated projections.
- [ ] Test negative and unlimited balances, sub-account transfers, paired recovery amounts, and concurrent backfill.

## Task 4: Pricing and long-request settlement

- [ ] Capture accounting currency and epoch in request pricing, admission, pending spend, and terminal state.
- [ ] Normalize USD source line charges by multiplication and leave CNY source charges unchanged in CNY mode.
- [ ] Normalize legacy USD terminal charges at the immutable activation rate.
- [ ] Keep old plan reservations on their original CNY-fen contract; use direct fen arithmetic for new CNY reservations.
- [ ] Test missing FX, mixed-currency maximum reservations, and requests spanning activation.

## Task 5: Replay, logs, and compatibility services

- [ ] Version durable money envelopes without changing old signatures or deduplication keys.
- [ ] Normalize request logs and revenue with a fixed historical conversion policy.
- [ ] Convert CNY to USD at Codex/DeepSeek boundaries and preserve availability before rounding.
- [ ] Add an explicit Studio currency contract and refund-to-debit binding; update the in-repository consumer.
- [ ] Test old spool replay, duplicate deltas, legacy terminal messages, and exact refunds.

## Task 6: Frontend and documentation

- [ ] Introduce exact CNY formatters and remove live FX multiplication from CNY wallets and charges.
- [ ] Update API types, editors, organization limits, exports, and balance/usage surfaces together.
- [ ] Update the affected subsystem specs and documentation in en, zh, zh-TW, and ja.
- [ ] Refresh documented screenshots in English and Chinese where the flow changes.
- [ ] Verify frontend tests, lint/build, and docs build.

## Task 7: Review and delivery

- [ ] Rehearse cross-epoch activation against a local database with concurrent synthetic long requests.
- [ ] Run the complete backend suite and focused end-to-end financial checks.
- [ ] Review the diff for monetary fields without a currency, unaudited rounding, and unsupported rollback.
- [ ] Commit and push verified changes to the authorized GitHub branch.
- [ ] Execute production activation only if the user has requested the online cutover.

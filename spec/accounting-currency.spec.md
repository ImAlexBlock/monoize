# Accounting Currency Transition

## Intent and units

AC-1. The target accounting currency is CNY. One CNY equals 1,000,000,000 nano-CNY.
Before activation the bridge MUST retain the existing USD rules. After activation,
wallets, internal charges, grants, spending limits, and new ledger entries MUST use signed
integer nano-CNY. Floating-point arithmetic MUST NOT participate in accounting.

AC-2. Existing USD assets MUST be converted with one exchange-rate snapshot selected at
activation. Let that snapshot be `R = N / D` CNY per USD, with positive integers `N` and `D`.
The snapshot, source timestamp, activation timestamp, and migration identifier MUST remain
immutable after activation. Retrying activation MUST NOT select a different rate.

AC-2a. Store the activation record at `state_records` key
`(tenant_id='__monoize_accounting', kind='currency_epoch', id='active')` with no expiry.
Its schema version is 1. It includes migration ID, epoch 1, currency CNY, the original
decimal rate, reduced numerator and denominator, source timestamp, refresh timestamp,
and activation timestamp. The rate MUST satisfy SB-FX-5, SB-FX-6, and SB-FX-13 at
activation. A missing record means legacy epoch 0. An invalid record is a storage error,
not permission to fall back to USD. Persist this record in the schema transition transaction.

AC-3. A conversion from integer nano-USD `x` to nano-CNY MUST equal
`sign(x) * floor(abs(x) * N / D + 1/2)`. The reverse conversion uses `D / N`.
The computation MUST support every input whose rounded result fits `i128`, including
`i128::MIN`. An intermediate product overflow MUST NOT reject a representable result.
An unrepresentable result MUST produce an explicit error without changing stored state.

AC-4. Every accounting amount crossing a request, persistence, replay, or service boundary MUST have
an explicit currency. An epoch identifies the denomination and conversion policy that
produced the amount. Epoch 0 denotes the legacy USD accounting system. Epoch 1 denotes
CNY accounting. A currency that contradicts its epoch MUST be rejected.
The wire envelope is `{ "amount": "<canonical signed i128>", "currency": "USD|CNY",
"epoch": 0|1 }`. Reject negative zero, noncanonical integers, missing denomination, and
out-of-range amounts. Source prices and payment contracts have independent currencies;
they are not accounting epoch envelopes.

## New accounting operations

AC-5. In epoch 1, for each token or meter line, compute quantity multiplied by the source unit price
before currency conversion. CNY source charges remain unchanged. USD source charges use
`round(source_charge * R_request)`. Capture `R_request` at most once before dispatch.
Sum normalized line charges and apply the existing multiplier rounding rule once.

AC-6. In epoch 1, CNY source prices MUST remain usable without an exchange-rate snapshot. A USD
source price without a valid snapshot MUST fail pricing validation before dispatch.
Reservations MUST compare normalized charges, not source-currency unit prices.

AC-7. In epoch 1, credit a quoted received CNY amount of `f` fen with
`f * 10,000,000` nano-CNY. Credit a quoted received USD amount of `c` cents with
`round(c * 10,000,000 * R_order)` nano-CNY. The source amount is the immutable
`quote.product.balance.actual_received_minor`, including its quoted bonus. It is not
the discounted `payment_minor`. An existing
order MUST retain its quoted payment currency and immutable order exchange rate.
Refund recovery MUST use the original credited accounting amount, not a later rate.

AC-8. New CNY quota reservations MUST reserve `ceil(maximum_nano / 10,000,000)` fen.
Settle them with `round(actual_nano / 10,000,000)` fen. Existing USD reservations
MUST retain their original entitlement rate and signed admission contract.
Commission balances, withdrawal amounts, and existing CNY quota buckets do not convert.

AC-9. Formatting a CNY wallet in CNY MUST NOT read or multiply by an exchange rate.
An optional USD reference display MUST divide by the selected display rate. Source
vendor prices, external payment amounts, and exchange-rate provider contracts retain
their explicitly declared currencies.

## History and continuity

AC-10. Preserve original historical ledger amounts, price breakdowns, signed tokens,
terminal digests, and payment evidence with their original currencies. A CNY historical
projection of an epoch 0 accounting amount MUST use `R`, never the current display rate.
Aggregate projected amounts after rounding each canonical record exactly once.

AC-11. On first CNY mutation of a legacy wallet, normalize its opening balance once.
Append one uniquely identified redenomination event with the original balance, CNY
opening balance, migration identifier, and exact conversion remainder. The subsequent
CNY ledger chain MUST reconcile from that opening balance. Do not independently round
every historical ledger delta and claim that it reconstructs the converted wallet.

AC-12. Freeze the accounting epoch and pricing snapshot at admission. A legacy request
that finishes after activation MUST settle its original USD charge through `R` exactly
once. A request MUST NOT change its pricing snapshot during execution. Track pending
deductions and reservations by epoch so that release subtracts the original amount.

AC-13. Durable request logs, metering deltas, admission claims, and terminal messages
MUST preserve currency, epoch, and their original deduplication identities. Decode old
unversioned money records explicitly as epoch 0 USD. Verify old signatures and digests
before conversion. Replaying an already applied operation MUST have no monetary effect.

## Deployment barrier

AC-14. Startup MUST NOT activate CNY on an existing USD database. First deploy a bridge
runtime through the unmodified blue-green continuity ordering. Legacy processes MUST
finish their connections and terminal tasks before currency activation. Do not stop
them at a time limit. Do not reload Caddy. Do not share request-log spools.

AC-15. Activation MUST require proof that no USD-only runtime can write the database.
For the initial SQLite deployment, require one serving bridge Primary, its valid
`store_primary` lease, no overlapping old or candidate instance, and completed recovery
of retired spools. A migration-version comparison alone is insufficient proof.

AC-16. The bridge MUST serialize the schema/epoch transition against short monetary
reads and writes. Do not retain that lock for the lifetime of an upstream stream.
Commit the schema layout, immutable rate, and active epoch atomically. Invalidate all
money caches before releasing the transition lock. A failure leaves the USD layout
and epoch unchanged if the database transaction has not committed. After a committed
activation, recovery MUST load the committed CNY state even if the process failed before
updating its in-memory state or returning a response. Startup MUST load accounting state
before creating stores, replaying spools, or starting background tasks. No CNY value may
be written into a column declared as USD.

AC-16a. The gate MUST cover authentication, reports, cache population, background tasks,
and all SQL or ORM accesses to renamed columns. Select layout after acquiring the gate;
retain the operation context through decoding. Nested helpers MUST reuse that context.
Activation MUST acquire the accounting write gate before the database writer lock.

AC-17. Convert mutable rows lazily in the transaction that first changes them, or in
bounded background batches. Store row epoch with its monetary fields. Conditional
updates MUST prevent repeated conversion under concurrent mutation or resumed backfill.
Preserve null spending limits, zero values, negative balances, and unlimited flags.

AC-18. After activation, rollback to a USD-only executable or automatic restore of a
pre-activation database MUST be rejected. Recover through a forward-compatible runtime.

## Compatibility boundaries

AC-19. The Codex and DeepSeek-compatible USD balance contracts retain their USD unit.
Convert the effective CNY balance only at those response boundaries. Availability is
determined from the sign of the internal amount before display rounding. A missing
required FX snapshot produces an explicit service error, not a mislabeled amount.

AC-20. Studio money messages MUST declare their currency. A refund MUST reference its
original debit and return the exact accounting amount debited. An idempotent retry MUST
retain the original conversion snapshot. A legacy USD request must never be interpreted
as a CNY request solely because the server's active epoch changed.

## Verification

AC-21. Tests MUST cover signed half ties, both i128 bounds, representable values with
overflowing intermediate products, invalid rates, and fixed-rate stability after a live
FX change. Integration tests MUST cover activation rollback and retry, cross-epoch long
request settlement, concurrent backfill and deduction, old spool replay, deduplication,
legacy signed admission settlement, exact refunds, and per-wallet ledger reconciliation.

# PostgreSQL Dashboard Self-Check

This assessment covers the successor to `20261001-dashboard-pg`.

1. The new migration widens only `firewall_events.created_at_unix_ms`.
   It reconstructs each value from `created_at`. It does not change payloads or IDs.
2. The previous application expects i64 for this field, so widening is compatible
   with overlapping old and candidate readers and writers.
3. The migration uses a transaction and a five-second lock timeout.
   Invalid timestamps or lock acquisition failures must abort candidate startup.
4. Before candidate startup, back up the database and the firewall table.
   Record an ordered checksum of firewall rows excluding the repaired timestamp.
5. The candidate must pass profile, metadata, announcements, firewall, Store,
   and revenue GET checks before new connections are switched.
6. Preserve old streams and the Store lease until normal blue-green handover.
   Do not reload Caddy or stop an instance with accepted connections.
7. Post-change evidence must distinguish concurrent new events from changes to
   the original row set. Never report unrelated row growth as data corruption.
8. Do not downgrade the repaired timestamp type. Binary rollback retains the
   widened schema; historical migration records may be newer than the old binary.
9. The public-interface matrix must report permissions separately from failures.
   Errors must not be rendered as successful empty results in the model workbench.
10. The user-management page must show a retryable error when users, groups, or
    billing plans fail to load. This error takes precedence over empty results.
    Hide editing controls until these dependencies have loaded without errors.
    Retry must revalidate these three reads and must not submit a mutation.
11. The Groups page must distinguish a failed Groups read from an empty list.
    The Providers page must reject failed or pending Groups, settings, transform
    registry, and model-metadata dependencies before exposing editing controls.
    Both pages must provide read-only retry actions for their failed dependencies.
12. Wallet summary reads must distinguish unavailable usage or entitlement from
    zero usage or no plan. Pending reads show a skeleton; failed reads show a
    retryable alert. Summary retry revalidates usage, entitlement, and exchange rate
    without submitting redemption or other financial mutations.
13. Settings and transform-registry read failures must hide settings save controls
    and expose a read-only retry. Recovery must preserve unsaved local settings.
14. Request-log query errors must expose a retryable alert, not an empty-result table.
    Previously loaded rows remain visible during query failure.
    Retry revalidates the current query without changing its filters or pagination.

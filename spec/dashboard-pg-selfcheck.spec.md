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

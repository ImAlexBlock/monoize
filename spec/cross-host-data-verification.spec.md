# Cross-Host Data Verification

## Scope

This specification covers the PostgreSQL verification tool used before a cross-host production switch.
It does not authorize a traffic switch, writer shutdown, or database replacement.

## Requirements

CHV1. The tool MUST execute only read-only SQL against the selected database.

CHV2. Table data MUST use one repeatable-read, read-only transaction.
Each table digest MUST cover every row and every column.
JSONB row text MUST use C collation ordering before hashing.
The digest MUST use SHA-256 over the PostgreSQL COPY text stream.
Duplicate rows MUST remain in the stream.

CHV3. The report MUST include row counts and table digests.
It MUST also cover columns, constraints, indexes, extensions, and sequences.
Reports MUST contain no row content, credentials, or connection strings.

CHV4. PostgreSQL sequences are not MVCC snapshot data.
Final comparison MUST occur while application writers are stopped on both databases.
The operator MUST prevent schema changes throughout verification.

CHV5. A database error, malformed stream, or subprocess failure MUST fail verification.
A partial report MUST NOT replace an existing report.

CHV6. Cross-host acceptance requires matching verification payloads.
Container names and capture times MAY differ.
An online source report is diagnostic only; it cannot prove a final synchronized state.

CHV7. Application candidates MUST NOT modify the restored final database before comparison.
Preserve earlier test restores separately from the final restore.
Preserve original databases, backups, spool directories, and deployment configuration.

CHV8. This tool does not verify filesystem data or roles.
The deployment MUST separately verify file checksums, required roles, and database access grants.
No production-complete claim follows from a matching database manifest alone.

## Drain Observation

CHV9. The drain observer MUST NOT change routing, signal containers, or stop connections.
It MUST count all nonterminal accepted sockets on the monitored application and proxy ports.
It MUST inspect the active request-log spool without reading its contents.
Three consecutive empty observations MAY finish observation; they do not authorize a later stop.
The deployment MUST recheck connections and writers immediately before final synchronization.
Malformed socket output, a missing spool, or a stopped production container MUST fail observation.

#!/bin/sh
set -eu
docker exec -i migration-monoize-postgres psql -X -q -U postgres -d migration_final -At -v ON_ERROR_STOP=1 <<'SQL'
DELETE FROM sessions WHERE id='d04d2295-62e7-4769-a564-86daacd999b2';
SELECT 'BROWSER_DIAGNOSTIC_SESSION_REMOVED' WHERE NOT EXISTS(
SELECT 1 FROM sessions WHERE id='d04d2295-62e7-4769-a564-86daacd999b2');
SQL

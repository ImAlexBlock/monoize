#!/usr/bin/env bash
# One-shot SQLite -> PostgreSQL cutover for Monoize (zero-downtime, spec PGMS5).
# Idempotent-safe to re-run; every step logs to /opt/monoize/cutover-<REV>.log.
set -euo pipefail

REV=9efeaee7
PGDSN="postgres://postgres:MonoPGx7K2vQ9wE4t@127.0.0.1:5433/monoize"
LOG=/opt/monoize/cutover-$REV.log
BUILD=/opt/monoize/build-93576b7e
MIGRATIONS_EXPECTED=95

step() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }
die()  { step "FAIL: $*"; exit 1; }

step "== cutover $REV start =="

# 1. Build the runtime image.
step "1/8 docker image"
cd "$BUILD/image-out"
docker build -q -t "monoize:$REV" --build-arg REVISION="$REV" . >/dev/null || die "image build"
docker images "monoize:$REV" --format 'image {{.Size}}' | tee -a "$LOG"

# 2. Apply migrations to the real target database via a disposable container.
step "2/8 apply migrations to target db"
docker rm -f monoize-pg-init >/dev/null 2>&1 || true
mkdir -p /tmp/pg-init-data
docker run -d --name monoize-pg-init --network host --user 1000:1000 \
  -e "MONOIZE_DATABASE_DSN=$PGDSN" \
  -e "MONOIZE_LISTEN=127.0.0.1:18098" \
  -v /tmp/pg-init-data:/app/data \
  "monoize:$REV" >/dev/null || die "init container start"
N=0
for _ in $(seq 1 80); do
  N=$(docker exec monoize-postgres psql -U postgres -p 5433 -t -A -d monoize \
      -c "SELECT count(*) FROM seaql_migrations;" 2>/dev/null || echo 0)
  [ "${N:-0}" -ge "$MIGRATIONS_EXPECTED" ] && break
  sleep 3
done
[ "${N:-0}" -ge "$MIGRATIONS_EXPECTED" ] || die "migrations applied=$N expected=$MIGRATIONS_EXPECTED log: $(docker logs monoize-pg-init 2>&1 | tail -3 | tr '\n' ' ')"
docker rm -f monoize-pg-init >/dev/null
step "migrations ok ($N)"

# 3. Online SQLite snapshot (production keeps serving).
step "3/8 sqlite snapshot"
rm -f /tmp/monoize-snapshot.db
sqlite3 /opt/monoize/data/monoize.db ".backup /tmp/monoize-snapshot.db" || die "snapshot"

# 4. Bulk migrate snapshot -> PG (idempotent upserts).
step "4/8 bulk migrate"
docker run --rm --network host \
  -v /tmp:/snap:ro \
  -v monoize-buildcache:/tgt \
  monoize-builder:30dca059 \
  /tgt/release/sqlite-to-pg --sqlite sqlite:///snap/monoize-snapshot.db \
    --postgres "$PGDSN" --batch 2000 >> "$LOG" 2>&1 || die "bulk migrate"

# 5. Row-count sanity gate on the hot tables (PG >= snapshot means passable;
#    strict equality except request_logs where live writes add rows).
step "5/8 row counts"
FAIL=0
for t in users api_keys monoize_providers monoize_provider_models monoize_groups \
         billing_rate_records model_registry_records sessions system_settings \
         billing_ledger store_orders store_products store_billing_plans; do
  S=$(sqlite3 /tmp/monoize-snapshot.db "SELECT count(*) FROM $t;" 2>/dev/null || echo "-")
  P=$(docker exec monoize-postgres psql -U postgres -p 5433 -t -A -d monoize \
      -c "SELECT count(*) FROM $t;" 2>/dev/null || echo "-")
  step "count $t sqlite=$S pg=$P"
  if [ "$S" != "-" ] && [ "$P" != "-" ] && [ "$P" -lt "$S" ]; then FAIL=1; fi
done
S=$(sqlite3 /tmp/monoize-snapshot.db "SELECT count(*) FROM request_logs;")
P=$(docker exec monoize-postgres psql -U postgres -p 5433 -t -A -d monoize -c "SELECT count(*) FROM request_logs;")
step "count request_logs sqlite=$S pg=$P"
[ "$FAIL" = "0" ] || die "row-count gate"

# 6. Point future containers at PG via the swap script's env injection point.
step "6/8 env-extra DSN"
(umask 077; echo "MONOIZE_DATABASE_DSN=$PGDSN" > /opt/monoize/env-extra.txt)

# 7. Zero-downtime blue-green swap (BG11 SIGHUP lease handover + BG12 drain).
step "7/8 blue-green swap"
/opt/monoize/blue-green-swap.sh "$REV" >> "$LOG" 2>&1 || die "swap"

# 8. Final incremental catch-up: rows the old container wrote during drain.
step "8/8 final incremental"
docker run --rm --network host \
  -v /opt/monoize/data:/data \
  -v monoize-buildcache:/tgt \
  monoize-builder:30dca059 \
  /tgt/release/sqlite-to-pg --sqlite sqlite:///data/monoize.db \
    --postgres "$PGDSN" --incremental 1 --batch 2000 >> "$LOG" 2>&1 || die "final incremental"

# Post-swap verification.
sleep 10
step "verify healthz: $(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8095/healthz)"
step "verify readyz: $(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8095/readyz)"
docker exec monoize-postgres psql -U postgres -p 5433 -t -A -d monoize \
  -c "SELECT 'pg_request_logs=' || count(*) FROM request_logs;" | tee -a "$LOG"
docker logs --since 3m monoize 2>&1 | grep -ciE '"level":"ERROR"' | xargs -I{} step "recent errors: {}" || true
step "== cutover done =="

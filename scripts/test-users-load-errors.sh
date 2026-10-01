#!/bin/sh
set -eu
umask 077
root=/opt/monoize/dashboard-pg-repair
tar -xzf /opt/migration-20260930/users-load-errors-patch.tar.gz -C "$root"
docker run --rm --name monoize-users-load-errors-tests --network none --cpus 2 --memory 3g \
  -v "$root:/src" -w /src/frontend monoize-builder:30dca059 \
  bash -c 'bun test tests/users-load-errors.test.ts tests/model-workbench-errors.test.ts && bun run typecheck'

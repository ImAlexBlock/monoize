#!/bin/sh
set -eu
umask 077
root=/opt/monoize/dashboard-pg-repair
archive=/opt/migration-20260930/dashboard-read-recovery-source.tar.gz
test -d "$root/src"
test -f "$archive"
test "$(docker inspect monoize-dashboard-guards-image-build --format '{{.State.Status}}')" = exited
test "$(docker inspect monoize-dashboard-guards-image-build --format '{{.State.ExitCode}}')" = 0
if docker inspect monoize-dashboard-read-recovery-build >/dev/null 2>&1; then
  printf 'Existing build container: inspect it instead of restarting\n' >&2
  exit 1
fi
sha256sum "$archive"
tar -xzf "$archive" -C "$root"
docker run --name monoize-dashboard-read-recovery-build --network host --cpus 2 --memory 10g --memory-swap 10g \
  -v "$root:/src" -v monoize-cargo:/usr/local/cargo -v monoize-buildcache:/src/target \
  -w /src monoize-builder:30dca059 \
  bash -ec 'source /root/.cargo/env; cargo build --locked --release -j 1 --bin monoize; mkdir -p /src/dashboard-read-recovery-image; cp target/release/monoize /src/dashboard-read-recovery-image/monoize'
install -m 644 /opt/migration-20260930/Dockerfile.dashboard-repair "$root/dashboard-read-recovery-image/Dockerfile"
docker build -t monoize:20261002-dashboard-read-recovery "$root/dashboard-read-recovery-image"
docker save -o /opt/migration-20260930/monoize-dashboard-read-recovery.tar monoize:20261002-dashboard-read-recovery
sha256sum /opt/migration-20260930/monoize-dashboard-read-recovery.tar
printf 'DASHBOARD_READ_RECOVERY_IMAGE_BUILT\n'

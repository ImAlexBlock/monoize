#!/bin/sh
set -eu
umask 077
test "$(docker image inspect monoize:20261002-dashboard-read-recovery --format '{{.Id}}')" = sha256:71023c43bc3a808ca9d43ee97cb11112fd3701f14f8e4b5e6933709181d7d6d9
test "$(docker inspect monoize --format '{{.Config.Image}}')" = monoize:20261001-org-usage
test "$(docker inspect monoize --format '{{.State.Health.Status}}')" = healthy
test "$(docker inspect monoize-before-20261001-org-usage --format '{{.State.Status}}')" = exited
test "$(docker inspect monoize-before-20261001-org-usage --format '{{.State.ExitCode}}')" = 0
exec /opt/monoize/blue-green-swap.sh 20261002-dashboard-read-recovery

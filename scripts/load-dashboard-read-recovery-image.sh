#!/bin/sh
set -eu
umask 077
archive=/opt/migration-20260930/monoize-dashboard-read-recovery.tar
printf '%s  %s\n' 'da627a82c6a0fc33848ba45e33534c2e07f81f5743c7073a8ef36cd26f80f615' "$archive" | sha256sum -c -
docker load -i "$archive"
docker image inspect monoize:20261002-dashboard-read-recovery --format 'IMAGE={{.Id}}'

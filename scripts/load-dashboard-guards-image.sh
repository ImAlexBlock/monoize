set -eu
umask 077
archive=/opt/migration-20260930/monoize-dashboard-guards.tar
test -f "$archive"
printf '%s  %s\n' '2cb5174cec4ba01b70d3543301b69dbb97d75321db29d87290f8677b1871884f' "$archive" | sha256sum -c -
docker load -i "$archive"
docker image inspect monoize:20261002-dashboard-guards --format 'IMAGE={{.Id}}'

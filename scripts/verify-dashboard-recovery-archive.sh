#!/bin/sh
set -eu
python3 - <<'PY'
import json
import subprocess
import tarfile

path = "/opt/migration-20260930/monoize-dashboard-read-recovery.tar"
with tarfile.open(path) as archive:
    manifests = json.load(archive.extractfile("manifest.json"))
    entries = [entry for entry in manifests
               if "monoize:20261002-dashboard-read-recovery" in entry.get("RepoTags", [])]
    assert len(entries) == 1
    config = entries[0]["Config"]
    digest = config.rsplit("/", 1)[-1].removesuffix(".json")
    data = json.loads(subprocess.check_output(
        ["docker", "image", "inspect", "monoize:20261002-dashboard-read-recovery"], text=True))[0]
    assert data["Id"] == "sha256:" + digest, "Loaded config differs from archive"
    print("ARCHIVE_CONFIG_MATCH", data["Id"])
PY

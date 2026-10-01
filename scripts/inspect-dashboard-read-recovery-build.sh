#!/bin/sh
set -eu
python3 - <<'PY'
import hashlib
import json
import pathlib
import subprocess

name = "monoize-dashboard-read-recovery-build"
r = subprocess.run(["docker", "inspect", name], capture_output=True, text=True)
if r.returncode:
    raise SystemExit("BUILD_CONTAINER_NOT_FOUND")
c = json.loads(r.stdout)[0]
s = c["State"]
print("BUILD", s["Status"], "EXIT", s["ExitCode"], "OOM", s.get("OOMKilled"), flush=True)
if s["Running"]:
    subprocess.run(["docker", "top", name, "-eo", "pid,etime,pcpu,pmem,comm"], check=True)
root = pathlib.Path("/opt/migration-20260930")
log = root / "build-dashboard-read-recovery.log"
if log.exists():
    lines = log.read_text(errors="replace").splitlines()
    print("LAST_LOG_LINES", "\n".join(lines[-8:]), flush=True)
    complete = "DASHBOARD_READ_RECOVERY_IMAGE_BUILT" in lines
    print("PACKAGING_COMPLETE", complete, flush=True)
    archive = root / "monoize-dashboard-read-recovery.tar"
    if complete and archive.exists():
        with archive.open("rb") as f:
            print("ARCHIVE_SHA256", hashlib.file_digest(f, "sha256").hexdigest(), flush=True)
        subprocess.run(["docker", "image", "inspect", "monoize:20261002-dashboard-read-recovery",
                        "--format", "IMAGE={{.Id}}"], check=True)
PY

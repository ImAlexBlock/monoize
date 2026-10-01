#!/bin/sh
set -eu
python3 - <<'PY'
import json
import pathlib
import subprocess
import time

name = "monoize-dashboard-read-recovery-build"
deadline = time.monotonic() + 110
while True:
    result = subprocess.run(["docker", "inspect", name], capture_output=True, text=True, check=True)
    state = json.loads(result.stdout)[0]["State"]
    if not state["Running"] or time.monotonic() >= deadline:
        print("BUILD", state["Status"], "EXIT", state["ExitCode"], "OOM", state.get("OOMKilled"), flush=True)
        break
    time.sleep(10)
if state["Running"]:
    subprocess.run(["docker", "top", name, "-eo", "pid,etime,pcpu,pmem,comm"], check=True)
log = pathlib.Path("/opt/migration-20260930/build-dashboard-read-recovery.log")
if log.exists():
    print("\n".join(log.read_text(errors="replace").splitlines()[-10:]), flush=True)
PY

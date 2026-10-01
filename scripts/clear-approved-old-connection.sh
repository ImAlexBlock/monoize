#!/bin/sh
set -eu
python3 - <<'PY'
import json
import pathlib
import subprocess
import time

def run(args):
    return subprocess.check_output(args, text=True)

old = json.loads(run(["docker", "inspect", "monoize-prev"]))[0]
assert old["State"]["Pid"] == 540775 and old["State"]["Running"]
assert old["Config"]["Image"] == "monoize:20261001-backend-selfcheck"
assert b"blue-green-pg-swap.py" in pathlib.Path("/proc/597542/cmdline").read_bytes()
selector = "( src 127.0.0.1 sport = :8080 dst 127.0.0.1 dport = :33908 )"
lines = run(["ss", "-Htnp", selector]).splitlines()
assert len(lines) == 1 and "pid=540775," in lines[0], "Approved socket identity changed"
subprocess.run(["ss", "-K", selector], check=True)
assert not run(["ss", "-Htn", selector]).strip(), "Approved socket still exists"
print("APPROVED_SOCKET_CLEARED", flush=True)
for _ in range(20):
    p = subprocess.run(["docker", "inspect", "monoize"], capture_output=True, text=True)
    if p.returncode == 0:
        c = json.loads(p.stdout)[0]
        print("CURRENT", c["Config"]["Image"], c["State"]["Status"], flush=True)
        break
    time.sleep(5)
print("\n".join(pathlib.Path("/opt/migration-20260930/deploy_org_usage_repair.log").read_text().splitlines()[-8:]))
PY

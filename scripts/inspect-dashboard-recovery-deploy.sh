#!/bin/sh
set -eu
python3 - <<'PY'
import json
import pathlib
import subprocess
import urllib.request

for name in ["monoize", "monoize-next", "monoize-prev", "monoize-before-20261002-dashboard-read-recovery"]:
    result = subprocess.run(["docker", "inspect", name], capture_output=True, text=True)
    if result.returncode:
        continue
    c = json.loads(result.stdout)[0]
    print("INSTANCE", name, c["Config"]["Image"], c["State"]["Status"],
          c["State"].get("Health", {}).get("Status"), "EXIT", c["State"]["ExitCode"], flush=True)
    if c["Config"]["Image"] == "monoize:20261002-dashboard-read-recovery" and c["State"]["Running"]:
        env = dict(v.split("=", 1) for v in c["Config"]["Env"] if "=" in v)
        client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        req = urllib.request.Request("http://" + env["MONOIZE_LISTEN"] + "/internal/deployment/status",
            headers={"Authorization": "Bearer " + env["MONOIZE_DEPLOYMENT_CONTROL_TOKEN"]})
        with client.open(req, timeout=10) as response:
            status = json.load(response)
        print("RECOVERY_STATUS", status.get("mode"), "LEASE_OWNED", status.get("lease_owned"), flush=True)
p = pathlib.Path("/proc/685216/cmdline")
print("SUPERVISOR_MATCH", p.exists() and b"blue-green-pg-swap.py" in p.read_bytes(), flush=True)
print("ROUTE", pathlib.Path("/opt/monoize/blue-green-route.state").read_text().strip(), flush=True)
print("OLD_CONNECTIONS", flush=True)
subprocess.run(["ss", "-Htn", "( sport = :8081 )"], check=True)
log = pathlib.Path("/opt/migration-20260930/deploy-dashboard-read-recovery.log")
print("\n".join(log.read_text().splitlines()[-20:]), flush=True)
PY

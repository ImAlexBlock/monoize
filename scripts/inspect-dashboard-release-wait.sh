#!/bin/sh
set -eu
python3 - <<'PY'
import collections
import json
import pathlib
import subprocess

root = pathlib.Path("/opt/migration-20260930")
for name in ["monoize", "monoize-next", "monoize-prev"]:
    result = subprocess.run(["docker", "inspect", name], capture_output=True, text=True)
    if result.returncode:
        continue
    data = json.loads(result.stdout)[0]
    print("INSTANCE", name, data["Config"]["Image"], data["State"]["Status"],
          data["State"].get("Health", {}).get("Status"), flush=True)
print("OLD_SOCKET_OWNERS", flush=True)
subprocess.run(["ss", "-tnp", "( sport = :8080 or dport = :8080 )"], check=True)
for pid, label in [(597542, "SWAP"), (649875, "MATRIX")]:
    process = pathlib.Path(f"/proc/{pid}/cmdline")
    try:
        command = process.read_bytes()
    except FileNotFoundError:
        print(label, "PROCESS_ABSENT", flush=True)
        continue
    print(label, "PROCESS_PRESENT", "swap_script" if b"blue-green-pg-swap.py" in command else "other", flush=True)
log = root / "audit_dashboard_matrix.log"
text = log.read_text() if log.exists() else ""
print("MATRIX_FINISHED", "MATRIX_FINISHED 50" in text, flush=True)
print("DIAGNOSTIC_SESSION_REMOVED", "DIAGNOSTIC_SESSION_REMOVED" in text, flush=True)
reports = sorted((root / "final").glob("dashboard-selfcheck-*.json"), key=lambda p: p.stat().st_mtime)
if reports:
    report = json.loads(reports[-1].read_text())
    print("REPORT", reports[-1].name, report["at"],
          dict(collections.Counter(row.get("status", 0) for row in report["results"])), flush=True)
    for row in report["results"]:
        if row.get("status") not in (200, 403):
            print("FAILED_PATH", row["path"], row.get("status"), row.get("transport_error"), flush=True)
PY

"""Observe source application drain without modifying routing or processes."""

import collections
import datetime
import json
import pathlib
import subprocess
import time

PORTS = {8080, 8081, 7869, *range(7874, 7884)}
EXCLUDED = {"LISTEN", "TIME-WAIT", "CLOSED"}


def connections(text):
    found = collections.Counter()
    queued = 0
    for line in text.splitlines():
        parts = line.split()
        if len(parts) < 5:
            raise ValueError("Malformed ss output")
        if parts[0] in EXCLUDED:
            continue
        port = int(parts[3].rsplit(":", 1)[1])
        if port in PORTS:
            found[f"{port}:{parts[0]}"] += 1
            queued += int(parts[2])
    return dict(found), queued


def main():
    empty_samples = 0
    while True:
        counts, queued = connections(subprocess.check_output(["ss", "-tanH"], text=True))
        config = json.loads(subprocess.check_output(["docker", "inspect", "monoize"]))[0]
        if not config["State"]["Running"]:
            raise RuntimeError("Production container is not running; inspect manually")
        env = dict(item.split("=", 1) for item in config["Config"]["Env"] if "=" in item)
        spool = env["MONOIZE_REQUEST_LOG_SPOOL_DIR"]
        if not spool.startswith("/app/data/"):
            raise RuntimeError("Unexpected spool")
        base = pathlib.Path("/opt/monoize/data").resolve()
        root = (base / spool.removeprefix("/app/data/")).resolve()
        if not root.is_relative_to(base) or not root.is_dir():
            raise RuntimeError("Spool missing or outside data directory")
        files = sum(path.is_file() for path in root.rglob("*"))
        empty_samples = empty_samples + 1 if not counts and files == 0 else 0
        print(json.dumps({
            "at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "connections": counts, "send_queue_bytes": queued,
            "spool_files": files, "consecutive_empty_samples": empty_samples,
        }), flush=True)
        if empty_samples >= 3:
            print("DRAIN_OBSERVED_RECHECK_REQUIRED_BEFORE_STOP", flush=True)
            return
        time.sleep(15)


if __name__ == "__main__":
    main()

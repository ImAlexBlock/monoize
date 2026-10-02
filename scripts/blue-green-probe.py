"""Record public-origin availability throughout a blue-green cutover."""

import json
import ipaddress
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.parse


def curl_arguments(url, address):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != "https" or not parsed.hostname or parsed.username is not None
            or parsed.password is not None or parsed.fragment
            or any(c.isspace() for c in url)):
        raise ValueError("public probe requires an HTTPS URL without credentials or fragments")
    ip = ipaddress.ip_address(address)
    host = f"[{parsed.hostname}]" if ":" in parsed.hostname else parsed.hostname
    target = f"[{ip}]" if ip.version == 6 else str(ip)
    return ["curl", "--disable", "--silent", "--show-error", "--output", os.devnull,
            "--write-out", "%{http_code}", "--max-time", "5", "--noproxy", "*",
            "--resolve", f"{host}:{parsed.port or 443}:{target}", url]


def main():
    log_path, ready_path, stop_path = map(Path, sys.argv[1:])
    args = curl_arguments(os.environ.get("MONOIZE_SWAP_PUBLIC_URL", "https://www.lynshen.org/"),
                          os.environ.get("MONOIZE_SWAP_PUBLIC_IP", "64.90.22.212"))
    failures = 0
    samples = 0
    with log_path.open("a", buffering=1) as log:
        while not stop_path.exists():
            started = time.monotonic()
            result = subprocess.run(args, capture_output=True, text=True)
            ok = result.returncode == 0 and len(result.stdout) == 3 and result.stdout.startswith("2")
            samples += 1
            failures += int(not ok)
            log.write(json.dumps({"time": time.time(), "status": result.stdout,
                                  "curl_exit": result.returncode, "ok": ok}) + "\n")
            if samples == 1:
                if not ok:
                    return 1
                ready_path.touch()
            time.sleep(max(0, 0.4 - (time.monotonic() - started)))
        log.write(json.dumps({"samples": samples, "failures": failures}) + "\n")
    return int(failures > 0 or samples == 0)


if __name__ == "__main__":
    sys.exit(main())

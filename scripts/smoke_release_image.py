"""Verify a staged release inside its isolated runtime image (PSC11a)."""

import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import subprocess
import time
import uuid


def run(*args, timeout=30):
    return subprocess.run(args, check=True, capture_output=True, timeout=timeout).stdout


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def migration_digest(directory):
    digest = hashlib.sha256()
    files = sorted(directory.rglob("*"), key=lambda path: path.relative_to(directory).as_posix())
    require(files and all(not path.is_symlink() for path in files), "Invalid migration tree")
    for path in files:
        if path.is_file():
            digest.update(path.relative_to(directory).as_posix().encode() + b"\0")
            digest.update(hashlib.sha256(path.read_bytes()).digest() + b"\0")
    return digest.hexdigest()


class ModuleScripts(HTMLParser):
    def __init__(self):
        super().__init__()
        self.sources = []

    def handle_starttag(self, tag, attributes):
        attributes = dict(attributes)
        if tag == "script" and attributes.get("type") == "module" and "src" in attributes:
            self.sources.append(attributes["src"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-dir", type=Path, required=True)
    directory = parser.parse_args().artifact_dir.resolve()
    revision = (directory / "REVISION").read_text().strip()
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "Expected full source revision")
    checksum = hashlib.sha256((directory / "monoize").read_bytes()).hexdigest()
    require((directory / "monoize.sha256").read_text().strip() == checksum + "  monoize",
            "Staged executable checksum mismatch")
    result = {"revision": revision, "binary_sha256": checksum,
              "migration_tree_sha256": migration_digest(directory / "src/migration")}
    identity = "monoize-runtime-smoke-" + uuid.uuid4().hex
    image = identity + ":test"
    try:
        subprocess.run(["docker", "build", "--build-arg", "REVISION=" + revision,
                        "--build-arg", "VERSION=selfcheck", "-t", image, str(directory)],
                       check=True, timeout=480)
        result["image_id"] = run("docker", "image", "inspect", image,
                                 "--format", "{{.Id}}").decode().strip()
        run("docker", "run", "-d", "--name", identity, "--network", "none",
            "--user", "1000:1000", "--tmpfs", "/app/data:rw,uid=1000,gid=1000,mode=0700",
            "-e", "MONOIZE_LISTEN=127.0.0.1:8080",
            "-e", "MONOIZE_DATABASE_DSN=sqlite:///app/data/runtime-smoke.db",
            "-e", "MONOIZE_REQUEST_LOG_SPOOL_DIR=/app/data/request-log-spool",
            image)

        def get(path):
            response = run("docker", "exec", identity, "curl", "--disable", "--fail",
                           "--silent", "--show-error", "--noproxy", "*", "--max-time", "3",
                           "--write-out", "\n%{content_type}", "http://127.0.0.1:8080" + path)
            body, content_type = response.rsplit(b"\n", 1)
            return body, content_type.decode().split(";", 1)[0]

        deadline = time.monotonic() + 120
        while True:
            require(run("docker", "inspect", identity, "--format", "{{.State.Running}}")
                    .strip() == b"true", "Runtime exited before readiness")
            try:
                ready = json.loads(get("/readyz")[0])
                if (ready.get("status") == "ready" and ready.get("database_backend") == "sqlite"
                        and ready.get("database_reachable") is True and ready.get("role") == "primary"):
                    result["readiness"] = ready
                    break
            except (subprocess.CalledProcessError, json.JSONDecodeError):
                pass
            require(time.monotonic() < deadline, "Runtime readiness exceeded 120 seconds")
            time.sleep(1)

        require(get("/healthz")[0].strip() == b"ok", "Unexpected liveness response")
        homepage, content_type = get("/")
        require(content_type == "text/html" and b'<div id="root">' in homepage,
                "Embedded frontend HTML is missing")
        modules = ModuleScripts()
        modules.feed(homepage.decode())
        require(modules.sources, "Embedded frontend has no module scripts")
        assets = {}
        for path in modules.sources:
            require(re.fullmatch(r"/assets/[A-Za-z0-9_.-]+\.js", path), "Unexpected module script URL")
            body, content_type = get(path)
            require(body and content_type in {"application/javascript", "text/javascript"},
                    "Embedded module script is not JavaScript")
            assets[path] = hashlib.sha256(body).hexdigest()
        result["asset_sha256"] = assets
        run("docker", "kill", "--signal=SIGTERM", identity)
        deadline = time.monotonic() + 30
        while run("docker", "inspect", identity, "--format", "{{.State.Running}}").strip() == b"true":
            require(time.monotonic() < deadline, "Runtime graceful shutdown exceeded 30 seconds")
            time.sleep(0.5)
        result["exit_code"] = int(run("docker", "inspect", identity, "--format", "{{.State.ExitCode}}"))
        require(result["exit_code"] == 0, "Runtime did not exit successfully")
        result["status"] = "passed"
        (directory / "runtime-smoke.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result))
    finally:
        subprocess.run(["docker", "rm", "-f", identity], capture_output=True, timeout=30)
        subprocess.run(["docker", "image", "rm", image], capture_output=True, timeout=30)


if __name__ == "__main__":
    main()

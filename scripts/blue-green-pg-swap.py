#!/usr/bin/env python3
"""PostgreSQL deployment adapter preserving streams and the Store lease."""

import datetime
import hashlib
import importlib.util
import ipaddress
import json
import os
import pathlib
import re
import secrets
import signal
import subprocess
import sys
import time
import urllib.request
import urllib.parse
import uuid

ROOT = pathlib.Path("/opt/monoize")
PROBE_SCRIPT = pathlib.Path(__file__).with_name("blue-green-probe.py")
probe_spec = importlib.util.spec_from_file_location("monoize_cutover_probe", PROBE_SCRIPT)
probe_module = importlib.util.module_from_spec(probe_spec)
probe_spec.loader.exec_module(probe_module)
CONTROL_ENV = {"MONOIZE_LISTEN", "MONOIZE_BOOT_STANDBY_LEASE",
               "MONOIZE_REQUEST_LOG_SPOOL_DIR", "MONOIZE_DEPLOYMENT_PREVIOUS_URL",
               "MONOIZE_DEPLOYMENT_CONTROL_TOKEN", "MONOIZE_DATABASE_DSN"}


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs).stdout


def inspect(name):
    return json.loads(run(["docker", "inspect", name]))[0]


def log(message):
    print(time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), message, flush=True)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def private_file(path, text):
    with path.open("x", encoding="utf-8") as output:
        output.write(text)
    path.chmod(0o600)


def extra_environment(path):
    if not path.exists():
        return {}
    values = {}
    for line in path.read_text().splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        key, separator, value = line.partition("=")
        require(separator and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key),
                "Invalid env-extra entry")
        require(key not in CONTROL_ENV and key not in values,
                "env-extra duplicates or overrides a protected deployment setting")
        require("\x00" not in value, "Invalid env-extra value")
        values[key] = value
    return values


def migration_tree_digest(path):
    require(path.is_dir() and not path.is_symlink(), "Migration source tree is missing")
    digest = hashlib.sha256()
    entries = sorted(path.rglob("*"), key=lambda entry: entry.relative_to(path).as_posix())
    require(all(not entry.is_symlink() for entry in entries), "Symlink in migration source tree")
    files = [entry for entry in entries if entry.is_file()]
    require(files, "Migration source tree is empty")
    for entry in files:
        digest.update(entry.relative_to(path).as_posix().encode())
        digest.update(b"\0")
        digest.update(hashlib.sha256(entry.read_bytes()).digest())
        digest.update(b"\0")
    return digest.hexdigest()


def verify_migrations(old, candidate_image, revision):
    old_tag = (old["Config"].get("Labels") or {}).get("io.monoize.deployment.revision")
    old_tag = old_tag or old["Config"]["Image"].removeprefix("monoize:")
    require(re.fullmatch(r"[A-Za-z0-9_.-]+", old_tag), "Unknown serving image tag")
    trees = [ROOT / f"build-{tag}" / "src/migration" for tag in (old_tag, revision)]
    digests = [migration_tree_digest(tree) if tree.exists() else None for tree in trees]
    if all(digests):
        require(digests[0] == digests[1], "Migration changes require a separate compatibility assessment")
        return digests[0]
    manifest_path = ROOT / "migration-manifest.json"
    metadata = manifest_path.stat()
    require(metadata.st_uid == 0 and metadata.st_mode & 0o022 == 0
            and not manifest_path.is_symlink(), "Migration manifest must be protected and owned by root")
    manifest = json.loads(manifest_path.read_text())["images"]
    declared = [manifest[image_id]["migration_tree_sha256"]
                for image_id in (old["Image"], candidate_image["Id"])]
    require(all(isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value)
                for value in declared), "Invalid migration manifest digest")
    require(declared[0] == declared[1], "Migration manifest describes different source trees")
    require(all(actual is None or actual == expected for actual, expected in zip(digests, declared)),
            "Migration manifest disagrees with available source tree")
    return declared[0]


def restart_policy(old):
    policy = old["HostConfig"]["RestartPolicy"]
    name = policy.get("Name") or "no"
    require(name in {"no", "always", "unless-stopped", "on-failure"}, "Invalid restart policy")
    retries = policy.get("MaximumRetryCount", 0)
    require(isinstance(retries, int) and retries >= 0, "Invalid restart retry count")
    return f"on-failure:{retries}" if name == "on-failure" and retries else name


def database_environment(dsn):
    parsed = urllib.parse.urlsplit(dsn)
    require(parsed.scheme in {"postgres", "postgresql"} and parsed.hostname
            and parsed.username and parsed.path not in {"", "/"} and not parsed.fragment,
            "Serving PostgreSQL DSN requires an explicit host, user, and database")
    values = {"PGHOST": urllib.parse.unquote(parsed.hostname),
              "PGPORT": str(parsed.port or 5432), "PGUSER": urllib.parse.unquote(parsed.username),
              "PGDATABASE": urllib.parse.unquote(parsed.path[1:]), "PGCONNECT_TIMEOUT": "10"}
    if parsed.password is not None:
        values["PGPASSWORD"] = urllib.parse.unquote(parsed.password)
    aliases = {"sslmode": "PGSSLMODE", "ssl-mode": "PGSSLMODE",
               "application_name": "PGAPPNAME", "application-name": "PGAPPNAME",
               "options": "PGOPTIONS"}
    for key, value in urllib.parse.parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True):
        require(key in aliases and aliases[key] not in values, "Unsupported or duplicate PostgreSQL DSN option")
        values[aliases[key]] = value
    require(all(not any(c in value for c in "\r\n\0") for value in values.values()), "Invalid PostgreSQL DSN value")
    return values


class DatabaseClient:
    def __init__(self, dsn, backup, extra_hosts):
        values = database_environment(dsn)
        image = os.environ.get("MONOIZE_SWAP_PG_CLIENT_IMAGE")
        if image:
            image = json.loads(run(["docker", "image", "inspect", image]))[0]["Id"]
        else:
            container = os.environ.get("MONOIZE_SWAP_PG_CONTAINER")
            require(container, "Set MONOIZE_SWAP_PG_CLIENT_IMAGE or MONOIZE_SWAP_PG_CONTAINER")
            image = inspect(container)["Image"]
        envfile = backup / "database-client.env"
        private_file(envfile, "".join(f"{key}={value}\n" for key, value in values.items()))
        self.base = ["docker", "run", "--rm", "-i", "--network", "host", "--env-file", str(envfile)]
        for host in extra_hosts:
            self.base.extend(["--add-host", host])
        self.image = image

    def command(self, binary, *args):
        return self.base + ["--entrypoint", binary, self.image, *args]

    def sql(self, statement):
        return run(self.command("psql", "-X", "-q", "-At", "-v", "ON_ERROR_STOP=1"),
                   input=statement, text=True).strip()

    def backup(self, directory):
        path = directory / "database.dump"
        with path.open("xb") as output:
            subprocess.run(self.command("pg_dump", "-Fc"), stdout=output,
                           stderr=subprocess.PIPE, check=True)
        require(path.stat().st_size > 0, "PostgreSQL backup is empty")
        with path.open("rb") as source:
            subprocess.run(self.command("pg_restore", "--list"), stdin=source,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=True)
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        (directory / "database.sha256").write_text(digest + "\n")


def public_configuration():
    names = ("MONOIZE_SWAP_PUBLIC_URL", "MONOIZE_SWAP_READY_URL", "MONOIZE_SWAP_PUBLIC_IP")
    require(all(os.environ.get(name) for name in names), "Explicit public probe URLs and target IP are required")
    url, ready, address = [os.environ[name] for name in names]
    probe_module.curl_arguments(url, address)
    probe_module.curl_arguments(ready, address)
    return url, ready, address


def public_readiness(config, evidence):
    result = subprocess.run(probe_module.curl_arguments(config[1], config[2]),
                            capture_output=True, text=True)
    with evidence.open("a") as output:
        output.write(json.dumps({"time": time.time(), "status": result.stdout,
                                 "curl_exit": result.returncode}) + "\n")
    require(result.returncode == 0 and result.stdout == "200", "Public readiness failed; retain both instances")


class CutoverProbe:
    def __init__(self, directory, config):
        self.stop = directory / "probe.stop"
        self.ready = directory / "probe.ready"
        self.path = directory / "probe.jsonl"
        self.config = config
        self.process = None

    def start(self):
        env = dict(os.environ, MONOIZE_SWAP_PUBLIC_URL=self.config[0], MONOIZE_SWAP_PUBLIC_IP=self.config[2])
        self.process = subprocess.Popen([sys.executable, str(PROBE_SCRIPT), str(self.path),
                                         str(self.ready), str(self.stop)], env=env,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            require(self.process.poll() is None, "Public probe failed before cutover")
            if self.ready.exists():
                return
            time.sleep(0.1)
        raise RuntimeError("Public probe did not produce a successful first sample")

    def finish(self):
        self.stop.touch()
        require(self.process.wait(timeout=10) == 0, "Public probe failed; retain both instances")

    def close(self):
        if self.process is not None:
            self.stop.touch()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                self.process.wait(timeout=10)


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def connections(port):
    text = run(["ss", "-Htan", f"( sport = :{port} )"], text=True)
    lines = text.splitlines()
    if any(len(line.split()) < 5 for line in lines):
        raise RuntimeError("Invalid connection snapshot")
    return [line for line in lines if line.split()[0] not in {"LISTEN", "TIME-WAIT", "CLOSED"}]


def caddy_configuration():
    service = os.environ.get("MONOIZE_SWAP_CADDY_SERVICE", "caddy")
    filename = os.environ.get("MONOIZE_SWAP_CADDYFILE", "/etc/caddy/Caddyfile")
    admin = os.environ.get("MONOIZE_SWAP_CADDY_ADMIN_URL", "http://127.0.0.1:2019")
    require(len(service) <= 255 and re.fullmatch(
        r"[A-Za-z0-9][A-Za-z0-9_-]*(?:@[A-Za-z0-9][A-Za-z0-9_-]*)?(?:\.service)?", service),
        "Invalid Caddy service name")
    path = pathlib.PurePosixPath(filename)
    require(filename.startswith("/") and not filename.startswith("//")
            and str(path) == filename and ".." not in path.parts and "\\" not in filename
            and not any(c.isspace() or not c.isprintable() for c in filename),
            "Caddyfile must be a normalized absolute POSIX path")
    require(not any(c.isspace() or not c.isprintable() for c in admin),
            "Invalid Caddy admin origin")
    try:
        parsed = urllib.parse.urlsplit(admin)
        address = ipaddress.ip_address(parsed.hostname or "")
        port = parsed.port
    except ValueError as error:
        raise RuntimeError("Caddy admin origin requires a loopback IP and explicit port") from error
    host = f"[{address.compressed}]" if address.version == 6 else str(address)
    require(parsed.scheme == "http" and address.is_loopback and "%" not in str(address)
            and port is not None and 1 <= port <= 65535 and parsed.username is None
            and parsed.password is None and not parsed.path and not parsed.query and not parsed.fragment
            and admin == f"http://{host}:{port}", "Invalid Caddy admin origin")
    return service, filename, admin


def verify_caddy(stable, uid, caddy_config):
    service, filename, admin = caddy_config
    config = pathlib.Path(filename).read_text()
    require(set(re.findall(r"127\.0\.0\.1:(808[01])", config)) == {str(stable)},
            "Caddyfile disagrees with the stable route")
    user = run(["systemctl", "show", service, "--property=User", "--value"], text=True).strip()
    require(user and int(run(["id", "-u", user], text=True)) == uid, "Caddy service UID mismatch")
    pid = int(run(["systemctl", "show", service, "--property=MainPID", "--value"], text=True))
    require(pid > 0 and pathlib.Path(f"/proc/{pid}").stat().st_uid == uid, "Caddy process UID mismatch")
    run(["systemctl", "is-enabled", "--quiet", "monoize-routing.service"])
    config = json.loads(run(["curl", "--disable", "-fsS", "--noproxy", "*", "--max-time", "5",
                             admin + "/config/apps/http/servers"]))
    ports = set()

    def visit(value):
        if isinstance(value, dict):
            if value.get("dial") in {"127.0.0.1:8080", "127.0.0.1:8081"}:
                ports.add(int(value["dial"].rsplit(":", 1)[1]))
            for item in value.values():
                visit(item)
        elif isinstance(value, list):
            for item in value:
                visit(item)

    visit(config)
    require(ports == {stable}, "Live Caddy configuration disagrees with the stable route")


def drain_and_handover(get, active, token):
    threshold = int(os.environ.get("MONOIZE_SWAP_DRAIN_MAX_SECONDS", "14400"))
    require(threshold > 0, "Drain alert threshold must be positive")
    started = time.monotonic()
    alerted = False
    handover_attempted = False
    paused = False
    try:
        while True:
            get("/readyz")
            remaining = connections(active)
            if remaining:
                log(f"Draining original port: {len(remaining)} connections")
                if not alerted and time.monotonic() - started >= threshold:
                    log(f"ALERT: drain threshold exceeded with {len(remaining)} connections; continuing to wait")
                    alerted = True
                time.sleep(15)
                continue
            paused = True
            status = get("/internal/deployment/pause", credential=token, method="POST", timeout=None)[1]
            require(status["mode"] == "paused" and status["lease_owned"] is False, "Forwarding pause unconfirmed")
            if connections(active):
                status = get("/internal/deployment/resume", credential=token, method="POST")[1]
                require(status["mode"] == "forwarding" and status["lease_owned"] is False,
                        "Forwarding resume unconfirmed")
                paused = False
                time.sleep(15)
                continue
            # A failed signal command has an uncertain outcome; forwarding must remain gated.
            handover_attempted = True
            run(["docker", "kill", "--signal=SIGHUP", "monoize-prev"])
            break
        for _ in range(60):
            status = get("/internal/deployment/status", credential=token)[1]
            if status["mode"] == "local" and status["lease_owned"] is True:
                return
            time.sleep(1)
        raise RuntimeError("Lease handover unconfirmed; retain both instances")
    finally:
        if paused and not handover_attempted:
            try:
                get("/internal/deployment/resume", credential=token, method="POST")
            except Exception:
                log("Forwarding resume failed; inspect the retained candidate")


def perform_cutover(probe, switch_route, verify_route, public_check, get, active, token):
    try:
        probe.start()
        switch_route()
        public_check()
        verify_route()
        status = get("/internal/deployment/status", credential=token)[1]
        require(status["mode"] == "forwarding" and status["lease_owned"] is False,
                "Candidate forwarding status changed during cutover")
        time.sleep(2)
        probe.finish()
        drain_and_handover(get, active, token)
    finally:
        probe.close()


def main():
    import fcntl

    os.umask(0o077)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    if len(sys.argv) != 2 or not re.fullmatch(r"[A-Za-z0-9_.-]+", sys.argv[1]):
        raise SystemExit("usage: blue-green-swap.sh <image-revision>")
    revision = sys.argv[1]
    caddy_config = caddy_configuration()
    with (ROOT / "blue-green-swap.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        old = inspect("monoize")
        require(old["State"]["Running"] and old["HostConfig"]["NetworkMode"] == "host",
                "Serving container must run with host networking")
        require(run(["docker", "exec", "monoize", "id", "-u"], text=True).strip() == "1000"
                and run(["docker", "exec", "monoize", "id", "-g"], text=True).strip() == "1000",
                "Serving application must use UID/GID 1000")
        run(["docker", "exec", "monoize", "sh", "-c",
             "grep -aq 'handing over the store_primary lease' /usr/local/bin/monoize"])
        for name in ["monoize-next", "monoize-prev"]:
            require(subprocess.run(["docker", "inspect", name], capture_output=True).returncode != 0,
                    "Unfinished deployment exists")
        candidate_image = "monoize:" + revision
        image = json.loads(run(["docker", "image", "inspect", candidate_image]))[0]
        migration_digest = verify_migrations(old, image, revision)
        public_config = public_configuration()
        extra = extra_environment(ROOT / "env-extra.txt")
        env = dict(v.split("=", 1) for v in old["Config"]["Env"] if "=" in v)
        require(env["MONOIZE_DATABASE_DSN"].startswith(("postgres://", "postgresql://")), "PostgreSQL DSN required")
        require(env["MONOIZE_LISTEN"] in {"127.0.0.1:8080", "127.0.0.1:8081"}, "Invalid serving listen address")
        active = int(env["MONOIZE_LISTEN"].rsplit(":", 1)[1])
        candidate = 8081 if active == 8080 else 8080
        require(not run(["ss", "-Hltn", f"( sport = :{candidate} )"], text=True).strip(), "Candidate port occupied")
        uid = int(run(["id", "-u", "caddy"], text=True).strip())
        require(uid != 1000, "Caddy and Monoize must have distinct UIDs")
        state = ROOT / "blue-green-route.state"
        stable, recorded, recorded_uid = map(int, state.read_text().split())
        require(stable in {8080, 8081} and recorded == active and recorded_uid == uid, "Route state mismatch")
        verify_caddy(stable, uid, caddy_config)
        run([str(ROOT / "blue-green-route.sh"), "--check", str(stable), str(active), str(uid)])
        data = pathlib.Path(next(m["Source"] for m in old["Mounts"] if m["Destination"] == "/app/data"))
        backup = ROOT / "backups" / ("pg-" + revision + "-" + str(time.time_ns()))
        backup.mkdir(mode=0o700, parents=True)
        private_file(backup / "previous-container.json", json.dumps(old))
        (backup / "migration-tree.sha256").write_text(migration_digest + "\n")
        database = DatabaseClient(env["MONOIZE_DATABASE_DSN"], backup, old["HostConfig"].get("ExtraHosts") or [])
        database.backup(backup)
        sql = database.sql
        previous_owner = sql("SELECT owner_id FROM store_primary_leases WHERE name='store_primary';")
        require(previous_owner and "\n" not in previous_owner, "Previous Store lease owner is missing")
        private_file(backup / "previous-lease-owner.txt", previous_owner + "\n")
        log("PostgreSQL backup complete")

        spool = "request-log-spool-" + revision + "-" + uuid.uuid4().hex[:8]
        (data / spool).mkdir(mode=0o700)
        os.chown(data / spool, 1000, 1000)
        token = secrets.token_hex(32)
        env.update(MONOIZE_LISTEN=f"127.0.0.1:{candidate}", MONOIZE_BOOT_STANDBY_LEASE="1",
                   MONOIZE_REQUEST_LOG_SPOOL_DIR="/app/data/" + spool,
                   MONOIZE_DEPLOYMENT_PREVIOUS_URL=f"http://127.0.0.1:{active}",
                   MONOIZE_DEPLOYMENT_CONTROL_TOKEN=token)
        env.update(extra)
        require(all(not any(c in value for c in "\r\n\0") for value in env.values()), "Invalid environment value")
        envfile = backup / "candidate.env"
        private_file(envfile, "".join(k + "=" + v + "\n" for k, v in env.items()))
        command = ["docker", "run", "-d", "--name", "monoize-next", "--network", "host",
                   "--user", "1000:1000", "--restart", restart_policy(old),
                   "--label", "io.monoize.deployment.revision=" + revision,
                   "--env-file", str(envfile), "-v", str(data) + ":/app/data",
                   "--log-opt", "max-size=20m", "--log-opt", "max-file=5",
                   "--ulimit", "nofile=65536:65536",
                   "--health-cmd", f"curl -fsS http://127.0.0.1:{candidate}/healthz >/dev/null",
                   "--health-interval", "10s", "--health-timeout", "3s", "--health-retries", "3"]
        for host in old["HostConfig"].get("ExtraHosts") or []:
            command.extend(["--add-host", host])
        run(command + [image["Id"]])
        client = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def get(path, port=candidate, credential=None, method="GET", timeout=3):
            headers = {"Authorization": "Bearer " + credential} if credential else {}
            with client.open(urllib.request.Request(f"http://127.0.0.1:{port}" + path,
                             headers=headers, method=method), timeout=timeout) as response:
                return response.status, json.load(response)

        for _ in range(120):
            try:
                if get("/readyz")[0] == 200:
                    break
            except Exception:
                pass
            time.sleep(1)
        else:
            raise RuntimeError("Candidate not ready; original serving instance retained")
        status = get("/internal/deployment/status", credential=token)[1]
        require(status["mode"] == "forwarding" and status["lease_owned"] is False, "Candidate is not forwarding")
        # Probe only read endpoints. Never create an order or invoke a payment adapter.
        session_id, raw_token = str(uuid.uuid4()), "urp_session_" + secrets.token_hex(32)
        hashed = hashlib.sha256(raw_token.encode()).hexdigest()
        user = sql("SELECT id FROM users WHERE role IN ('admin','super_admin') AND enabled=1 ORDER BY role,created_at LIMIT 1;")
        require(str(uuid.UUID(user)) == user, "No enabled administrator with a valid ID")
        now = datetime.datetime.now(datetime.timezone.utc)
        expiry = now + datetime.timedelta(minutes=5)
        paths = ["/api/dashboard/store/catalog", "/api/dashboard/store/exchange-rate",
                 "/api/dashboard/store/entitlement", "/api/dashboard/store/orders",
                 "/api/dashboard/store/admin/products", "/api/dashboard/store/admin/payment-channels",
                 "/api/dashboard/admin/revenue/daily", "/api/dashboard/admin/revenue/exclusions",
                 "/api/dashboard/model-metadata", "/api/dashboard/billing-rates",
                 "/api/dashboard/billing-rates/profiles", "/api/dashboard/firewall/stats",
                 "/api/dashboard/firewall/events", "/api/dashboard/announcements"]
        try:
            sql(f"INSERT INTO sessions(id,user_id,token,created_at,expires_at) VALUES('{session_id}','{user}','{hashed}','{now.isoformat()}','{expiry.isoformat()}');")
            for path in paths:
                code, body = get(path, credential=raw_token)
                require(code == 200, "Candidate dashboard GET failed")
                log("Candidate GET passed: " + path)
        finally:
            sql(f"DELETE FROM sessions WHERE id='{session_id}';")
        orgs = json.loads(sql(
            "SELECT coalesce(json_agg(t),'[]') FROM "
            "(SELECT id,owner_user_id FROM orgs ORDER BY id LIMIT 20) t;"
        ))
        for org in orgs:
            org_id, owner_id = org["id"], org["owner_user_id"]
            require(str(uuid.UUID(org_id)) == org_id and str(uuid.UUID(owner_id)) == owner_id, "Invalid organization IDs")
            org_session, org_token = str(uuid.uuid4()), "urp_session_" + secrets.token_hex(32)
            org_hash = hashlib.sha256(org_token.encode()).hexdigest()
            now = datetime.datetime.now(datetime.timezone.utc)
            expiry = now + datetime.timedelta(minutes=5)
            try:
                sql(f"INSERT INTO sessions(id,user_id,token,created_at,expires_at) VALUES('{org_session}','{owner_id}','{org_hash}','{now.isoformat()}','{expiry.isoformat()}');")
                code, body = get(f"/api/dashboard/orgs/{org_id}/member-usage?range_hours=720&buckets=24",
                                 credential=org_token)
                require(code == 200 and isinstance(body.get("members"), list)
                        and isinstance(body.get("removed_members"), list), "Candidate organization GET failed")
                log("Candidate organization member-usage GET passed")
            finally:
                sql(f"DELETE FROM sessions WHERE id='{org_session}';")
        route = [str(ROOT / "blue-green-route.sh"), str(stable), str(candidate), str(uid)]

        def verify_route():
            require(state.read_text().split() == [str(stable), str(candidate), str(uid)], "Route state changed")
            run([route[0], "--check", *route[1:]])

        def switch_route():
            run(["docker", "rename", "monoize", "monoize-prev"])
            temp = ROOT / "blue-green-route.state.next"
            temp.write_text(f"{stable} {candidate} {uid}\n")
            os.chmod(temp, 0o644)
            os.replace(temp, state)
            run(route)
            log("New Caddy connections routed to candidate; old streams remain")

        probe = CutoverProbe(backup, public_config)
        public_check = lambda: public_readiness(public_config, backup / "public-readiness.jsonl")
        perform_cutover(probe, switch_route, verify_route, public_check, get, active, token)
        require(not connections(active), "Original connections reappeared; do not stop")
        verify_route()
        owner = sql("SELECT owner_id FROM store_primary_leases WHERE name='store_primary';")
        require(owner and "\n" not in owner and owner != previous_owner, "Store lease owner did not change")
        private_file(backup / "candidate-lease-owner.txt", owner + "\n")
        run(["docker", "update", "--restart=no", "monoize-prev"])
        run(["docker", "kill", "--signal=SIGTERM", "monoize-prev"])
        for _ in range(120):
            if not inspect("monoize-prev")["State"]["Running"]:
                break
            time.sleep(1)
        else:
            raise RuntimeError("Graceful shutdown pending; retain both instances")
        require(inspect("monoize-prev")["State"]["ExitCode"] == 0, "Previous container did not exit cleanly")
        run(["docker", "rename", "monoize-prev", "monoize-before-" + revision + "-" + str(time.time_ns())])
        run(["docker", "rename", "monoize-next", "monoize"])
        serving = inspect("monoize")
        require(serving["State"]["Running"] and serving["Image"] == image["Id"], "Final serving image mismatch")
        require(get("/readyz")[0] == 200, "Final candidate readiness failed")
        status = get("/internal/deployment/status", credential=token)[1]
        require(status["mode"] == "local" and status["lease_owned"] is True, "Final Store lease ownership failed")
        verify_route()
        public_check()
        log("SUCCESS: candidate owns Store lease; old instance retained after natural drain")


if __name__ == "__main__":
    main()

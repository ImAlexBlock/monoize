#!/usr/bin/env python3
"""PostgreSQL deployment adapter preserving streams and the Store lease."""

import fcntl
import datetime
import hashlib
import json
import os
import pathlib
import secrets
import subprocess
import sys
import time
import urllib.request
import uuid

ROOT = pathlib.Path("/opt/monoize")


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs).stdout


def inspect(name):
    return json.loads(run(["docker", "inspect", name]))[0]


def log(message):
    print(time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), message, flush=True)


def connections(port):
    text = run(["ss", "-Htan", f"( sport = :{port} )"], text=True)
    lines = text.splitlines()
    if any(len(line.split()) < 5 for line in lines):
        raise RuntimeError("Invalid connection snapshot")
    return [line for line in lines if line.split()[0] not in {"LISTEN", "TIME-WAIT", "CLOSED"}]


def main():
    os.umask(0o077)
    if len(sys.argv) != 2 or not all(c.isalnum() or c in "-_." for c in sys.argv[1]):
        raise SystemExit("usage: blue-green-swap.sh <image-revision>")
    revision = sys.argv[1]
    with (ROOT / "blue-green-swap.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        old = inspect("monoize")
        assert old["State"]["Running"] and old["HostConfig"]["NetworkMode"] == "host"
        for name in ["monoize-next", "monoize-prev"]:
            assert subprocess.run(["docker", "inspect", name], capture_output=True).returncode != 0, "Unfinished deployment exists"
        candidate_image = "monoize:" + revision
        run(["docker", "image", "inspect", candidate_image])
        env = dict(v.split("=", 1) for v in old["Config"]["Env"] if "=" in v)
        assert env["MONOIZE_DATABASE_DSN"].startswith(("postgres://", "postgresql://"))
        assert env["MONOIZE_LISTEN"] in {"127.0.0.1:8080", "127.0.0.1:8081"}
        active = int(env["MONOIZE_LISTEN"].rsplit(":", 1)[1])
        candidate = 8081 if active == 8080 else 8080
        assert not run(["ss", "-Hln", f"( sport = :{candidate} )"], text=True).strip()
        uid = int(run(["id", "-u", "caddy"], text=True).strip())
        assert uid != 1000
        state = ROOT / "blue-green-route.state"
        stable, recorded, recorded_uid = map(int, state.read_text().split())
        assert recorded == active and recorded_uid == uid
        run([str(ROOT / "blue-green-route.sh"), "--check", str(stable), str(active), str(uid)])
        data = pathlib.Path(next(m["Source"] for m in old["Mounts"] if m["Destination"] == "/app/data"))
        backup = ROOT / "backups" / ("pg-" + revision + "-" + str(time.time_ns()))
        backup.mkdir(mode=0o700, parents=True)
        (backup / "previous-container.json").write_text(json.dumps(old))
        with (backup / "database.dump").open("xb") as output:
            subprocess.run(["docker", "exec", "migration-monoize-postgres", "pg_dump", "-U", "postgres",
                            "-d", "migration_final", "-Fc"], stdout=output, check=True)
        with (backup / "database.dump").open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        (backup / "database.sha256").write_text(digest + "\n")
        log("PostgreSQL backup complete")

        spool = "request-log-spool-" + revision + "-" + uuid.uuid4().hex[:8]
        (data / spool).mkdir(mode=0o700)
        os.chown(data / spool, 1000, 1000)
        token = secrets.token_hex(32)
        env.update(MONOIZE_LISTEN=f"127.0.0.1:{candidate}", MONOIZE_BOOT_STANDBY_LEASE="1",
                   MONOIZE_REQUEST_LOG_SPOOL_DIR="/app/data/" + spool,
                   MONOIZE_DEPLOYMENT_PREVIOUS_URL=f"http://127.0.0.1:{active}",
                   MONOIZE_DEPLOYMENT_CONTROL_TOKEN=token)
        assert all("\n" not in value and "\r" not in value for value in env.values())
        envfile = backup / "candidate.env"
        envfile.write_text("".join(k + "=" + v + "\n" for k, v in env.items()))
        command = ["docker", "run", "-d", "--name", "monoize-next", "--network", "host",
                   "--user", old["Config"]["User"], "--restart", "unless-stopped",
                   "--env-file", str(envfile), "-v", str(data) + ":/app/data",
                   "--log-opt", "max-size=20m", "--log-opt", "max-file=5",
                   "--ulimit", "nofile=65536:65536",
                   "--health-cmd", f"curl -fsS http://127.0.0.1:{candidate}/healthz >/dev/null",
                   "--health-interval", "10s", "--health-timeout", "3s", "--health-retries", "3"]
        for host in old["HostConfig"].get("ExtraHosts") or []:
            command.extend(["--add-host", host])
        run(command + [candidate_image])
        client = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def get(path, port=candidate, credential=None, method="GET", timeout=10):
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
        assert status["mode"] == "forwarding" and status["lease_owned"] is False
        # Probe only read endpoints. Never create an order or invoke a payment adapter.
        session_id, raw_token = str(uuid.uuid4()), "urp_session_" + secrets.token_hex(32)
        hashed = hashlib.sha256(raw_token.encode()).hexdigest()
        sqlbase = ["docker", "exec", "-i", "migration-monoize-postgres", "psql", "-X", "-q",
                   "-U", "postgres", "-d", "migration_final", "-At", "-v", "ON_ERROR_STOP=1"]

        def sql(statement):
            return run(sqlbase, input=statement, text=True).strip()

        user = sql("SELECT id FROM users WHERE role='admin' AND enabled=1 ORDER BY created_at LIMIT 1;")
        assert str(uuid.UUID(user)) == user
        now = datetime.datetime.now(datetime.timezone.utc)
        expiry = now + datetime.timedelta(minutes=5)
        sql(f"INSERT INTO sessions(id,user_id,token,created_at,expires_at) VALUES('{session_id}','{user}','{hashed}','{now.isoformat()}','{expiry.isoformat()}');")
        paths = ["/api/dashboard/store/catalog", "/api/dashboard/store/exchange-rate",
                 "/api/dashboard/store/entitlement", "/api/dashboard/store/orders",
                 "/api/dashboard/store/admin/products", "/api/dashboard/store/admin/payment-channels",
                 "/api/dashboard/admin/revenue/daily", "/api/dashboard/admin/revenue/exclusions",
                 "/api/dashboard/model-metadata", "/api/dashboard/billing-rates",
                 "/api/dashboard/billing-rates/profiles", "/api/dashboard/firewall/stats",
                 "/api/dashboard/firewall/events", "/api/dashboard/announcements"]
        try:
            for path in paths:
                code, body = get(path, credential=raw_token)
                assert code == 200
                log("Candidate GET passed: " + path)
        finally:
            sql(f"DELETE FROM sessions WHERE id='{session_id}';")
        run(["docker", "rename", "monoize", "monoize-prev"])
        temp = ROOT / "blue-green-route.state.next"
        temp.write_text(f"{stable} {candidate} {uid}\n")
        os.chmod(temp, 0o644)
        os.replace(temp, state)
        run([str(ROOT / "blue-green-route.sh"), str(stable), str(candidate), str(uid)])
        log("New Caddy connections routed to candidate; old streams remain")
        handover_sent = False
        paused = False
        try:
            while True:
                get("/readyz")
                remaining = connections(active)
                if remaining:
                    log(f"Draining original port: {len(remaining)} connections")
                    time.sleep(15)
                    continue
                paused = True
                status = get("/internal/deployment/pause", credential=token, method="POST", timeout=None)[1]
                assert status["mode"] == "paused" and status["lease_owned"] is False
                if connections(active):
                    get("/internal/deployment/resume", credential=token, method="POST")
                    paused = False
                    time.sleep(15)
                    continue
                handover_sent = True
                run(["docker", "kill", "--signal=SIGHUP", "monoize-prev"])
                break
            for _ in range(90):
                status = get("/internal/deployment/status", credential=token)[1]
                if status["mode"] == "local" and status["lease_owned"]:
                    break
                time.sleep(1)
            else:
                raise RuntimeError("Lease handover unconfirmed; retain both instances")
            assert not connections(active), "Original connections reappeared; do not stop"
            run(["docker", "update", "--restart=no", "monoize-prev"])
            run(["docker", "kill", "--signal=SIGTERM", "monoize-prev"])
            for _ in range(120):
                if not inspect("monoize-prev")["State"]["Running"]:
                    break
                time.sleep(1)
            else:
                raise RuntimeError("Graceful shutdown pending; retain both instances")
            assert inspect("monoize-prev")["State"]["ExitCode"] == 0
            run(["docker", "rename", "monoize-prev", "monoize-before-" + revision])
            run(["docker", "rename", "monoize-next", "monoize"])
            assert get("/readyz")[0] == 200
            log("SUCCESS: repaired candidate owns Store lease; old instance retained stopped")
        finally:
            if paused and not handover_sent:
                get("/internal/deployment/resume", credential=token, method="POST")


if __name__ == "__main__":
    main()

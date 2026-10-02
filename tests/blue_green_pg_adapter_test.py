import importlib.util
import pathlib
import os
import json
import signal
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import MagicMock, Mock, patch

path = pathlib.Path(__file__).parents[1] / "scripts/blue-green-pg-swap.py"
spec = importlib.util.spec_from_file_location("pgswap", path)
pgswap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pgswap)
PROJECT = pathlib.Path(__file__).parents[1]


class ConnectionTests(unittest.TestCase):
    def test_listening_and_timewait_are_not_active_requests(self):
        with patch.object(pgswap, "run", return_value=(
            "LISTEN 0 4096 127.0.0.1:8080 0.0.0.0:*\n"
            "TIME-WAIT 0 0 127.0.0.1:8080 127.0.0.1:40000\n"
        )):
            self.assertEqual(pgswap.connections(8080), [])

    def test_half_closed_and_established_connections_must_drain(self):
        text = (
            "ESTAB 0 50 127.0.0.1:8080 127.0.0.1:40000\n"
            "CLOSE-WAIT 1 0 127.0.0.1:8080 127.0.0.1:40001\n"
        )
        with patch.object(pgswap, "run", return_value=text):
            self.assertEqual(len(pgswap.connections(8080)), 2)

    def test_malformed_output_is_not_drain_success(self):
        with patch.object(pgswap, "run", return_value="invalid snapshot"):
            with self.assertRaises(RuntimeError):
                pgswap.connections(8080)


class ConfigurationTests(unittest.TestCase):
    def setUp(self):
        (PROJECT / "local-test").mkdir(exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=PROJECT / "local-test")
        self.root = pathlib.Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)

    def test_extra_environment_rejects_every_control_override(self):
        path = self.root / "env-extra.txt"
        for key in pgswap.CONTROL_ENV:
            path.write_text(f"{key}=unsafe\n")
            with self.assertRaises(RuntimeError):
                pgswap.extra_environment(path)

    def test_extra_environment_accepts_only_unique_assignments(self):
        path = self.root / "env-extra.txt"
        self.assertEqual(pgswap.extra_environment(path), {})
        path.write_text("# comment\nMONOIZE_FEATURE=enabled\nEMPTY=\n")
        self.assertEqual(pgswap.extra_environment(path), {"MONOIZE_FEATURE": "enabled", "EMPTY": ""})
        for text in ["KEY=a\nKEY=b\n", "export KEY=a", "KEY=one\ncontinuation", "KEY=bad\0value"]:
            path.write_text(text)
            with self.assertRaises(RuntimeError):
                pgswap.extra_environment(path)

    def test_database_environment_preserves_the_serving_connection(self):
        result = pgswap.database_environment("postgres://operator:p%40ss%3Dword@[::1]:5544/my%20db?ssl-mode=require")
        self.assertEqual(result, {"PGHOST": "::1", "PGPORT": "5544", "PGUSER": "operator",
                                  "PGPASSWORD": "p@ss=word", "PGDATABASE": "my db",
                                  "PGCONNECT_TIMEOUT": "10", "PGSSLMODE": "require"})

    def test_database_environment_rejects_ambiguous_connections(self):
        for dsn in ["sqlite://file", "postgres://user@host/", "postgres://host/db",
                    "postgres://user@host/db?unknown=x", "postgres://user@host/db?sslmode=require&ssl-mode=disable",
                    "postgres://user:one%0Atwo@host/db"]:
            with self.assertRaises((ValueError, RuntimeError)):
                pgswap.database_environment(dsn)

    def test_database_client_keeps_credentials_out_of_commands(self):
        dsn = "postgres://operator:private-password@127.0.0.1:5544/actual_database"
        with patch.dict(os.environ, {"MONOIZE_SWAP_PG_CLIENT_IMAGE": "postgres:17"}), \
                patch.object(pgswap, "run", return_value=json.dumps([{"Id": "sha256:client"}])):
            client = pgswap.DatabaseClient(dsn, self.root, ["database:127.0.0.1"])
        command = client.command("pg_dump", "-Fc")
        self.assertNotIn("private-password", " ".join(command))
        self.assertIn("sha256:client", command)
        self.assertIn("host", command)
        env = (self.root / "database-client.env").read_text()
        self.assertIn("PGDATABASE=actual_database\n", env)
        self.assertIn("PGPASSWORD=private-password\n", env)

    def test_restart_policy_preserves_retry_limit(self):
        self.assertEqual(pgswap.restart_policy({"HostConfig": {"RestartPolicy": {
            "Name": "on-failure", "MaximumRetryCount": 7}}}), "on-failure:7")

    def test_different_migration_trees_fail_before_deployment(self):
        for revision in ["old", "new"]:
            tree = self.root / f"build-{revision}/src/migration"
            tree.mkdir(parents=True)
            (tree / "mod.rs").write_text(revision)
        old = {"Config": {"Image": "monoize:old"}, "Image": "sha256:old"}
        with patch.object(pgswap, "ROOT", self.root), patch.object(pgswap, "run") as run:
            with self.assertRaises(RuntimeError):
                pgswap.verify_migrations(old, {"Id": "sha256:new"}, "new")
            run.assert_not_called()
        (self.root / "build-new/src/migration/mod.rs").write_text("old")
        with patch.object(pgswap, "ROOT", self.root):
            self.assertEqual(len(pgswap.verify_migrations(old, {"Id": "sha256:new"}, "new")), 64)

    def test_missing_migration_evidence_fails(self):
        old = {"Config": {"Image": "monoize:old"}, "Image": "sha256:old"}
        with patch.object(pgswap, "ROOT", self.root):
            with self.assertRaises(FileNotFoundError):
                pgswap.verify_migrations(old, {"Id": "sha256:new"}, "new")

    def test_manifest_is_tied_to_both_immutable_images(self):
        path = self.root / "migration-manifest.json"
        digest = "a" * 64
        path.write_text(json.dumps({"images": {"sha256:old": {"migration_tree_sha256": digest},
                                               "sha256:new": {"migration_tree_sha256": digest}}}))
        path.chmod(0o600)
        old = {"Config": {"Image": "monoize:old"}, "Image": "sha256:old"}
        original_stat = pathlib.Path.stat

        def root_owned(file, *args, **kwargs):
            result = original_stat(file, *args, **kwargs)
            if file == path:
                return Mock(st_uid=0, st_mode=0o100600)
            return result

        with patch.object(pgswap, "ROOT", self.root), patch.object(pathlib.Path, "stat", root_owned):
            self.assertEqual(pgswap.verify_migrations(old, {"Id": "sha256:new"}, "new"), digest)
            with self.assertRaises(KeyError):
                pgswap.verify_migrations(old, {"Id": "sha256:unverified"}, "new")

    def test_public_probe_verifies_tls_for_the_explicit_host_and_ip(self):
        command = pgswap.probe_module.curl_arguments("https://api.example.com/readyz", "40.160.141.21")
        self.assertEqual(command[:2], ["curl", "--disable"])
        self.assertIn("api.example.com:443:40.160.141.21", command)
        self.assertIn("--noproxy", command)
        self.assertNotIn("--insecure", command)
        for url in ["http://api.example.com/", "https://user:password@example.com/", "https://example.com/#fragment"]:
            with self.assertRaises(ValueError):
                pgswap.probe_module.curl_arguments(url, "127.0.0.1")

    def test_backup_failure_does_not_report_a_valid_digest(self):
        client = object.__new__(pgswap.DatabaseClient)
        client.base = ["docker", "run"]
        client.image = "sha256:client"

        def fail_restore(command, **kwargs):
            if "pg_dump" in command:
                kwargs["stdout"].write(b"incomplete backup")
                return subprocess.CompletedProcess(command, 0)
            raise subprocess.CalledProcessError(1, command)

        with patch.object(pgswap.subprocess, "run", side_effect=fail_restore):
            with self.assertRaises(subprocess.CalledProcessError):
                client.backup(self.root)
        self.assertFalse((self.root / "database.sha256").exists())


class HandoverTests(unittest.TestCase):
    def setUp(self):
        self.events = []
        self.run = Mock(side_effect=lambda args: self.events.append(args))
        self.addCleanup(patch.stopall)
        patch.object(pgswap, "run", self.run).start()
        patch.object(pgswap.time, "sleep").start()
        patch.object(pgswap, "log").start()

    def get(self, path, **kwargs):
        self.events.append(path)
        if path.endswith("pause"):
            self.assertIsNone(kwargs["timeout"])
            return 200, {"mode": "paused", "lease_owned": False}
        if path.endswith("resume"):
            return 200, {"mode": "forwarding", "lease_owned": False}
        return 200, {"mode": "local", "lease_owned": True}

    def test_handover_occurs_only_after_paused_recheck(self):
        with patch.object(pgswap, "connections", side_effect=lambda port: self.events.append("connections") or []):
            pgswap.drain_and_handover(self.get, 8080, "secret")
        self.assertEqual(self.events, ["/readyz", "connections", "/internal/deployment/pause", "connections",
                                       ["docker", "kill", "--signal=SIGHUP", "monoize-prev"],
                                       "/internal/deployment/status"])

    def test_failed_socket_query_never_signals_old_instance(self):
        with patch.object(pgswap, "connections", side_effect=RuntimeError("socket query failed")):
            with self.assertRaises(RuntimeError):
                pgswap.drain_and_handover(self.get, 8080, "secret")
        self.run.assert_not_called()

    def test_failed_paused_recheck_resumes_without_signaling(self):
        with patch.object(pgswap, "connections", side_effect=[[], RuntimeError("socket query failed")]):
            with self.assertRaises(RuntimeError):
                pgswap.drain_and_handover(self.get, 8080, "secret")
        self.assertIn("/internal/deployment/resume", self.events)
        self.run.assert_not_called()

    def test_sigterm_during_pause_attempt_resumes_without_signaling(self):
        def get(path, **kwargs):
            if path.endswith("pause"):
                pgswap.interrupted(signal.SIGTERM, None)
            return self.get(path, **kwargs)

        with patch.object(pgswap, "connections", return_value=[]):
            with self.assertRaises(SystemExit) as stopped:
                pgswap.drain_and_handover(get, 8080, "secret")
        self.assertEqual(stopped.exception.code, 143)
        self.assertIn("/internal/deployment/resume", self.events)
        self.run.assert_not_called()

    def test_connections_reappearing_after_pause_resume_before_retry(self):
        with patch.object(pgswap, "connections", side_effect=[[], ["ESTAB"], [], []]):
            pgswap.drain_and_handover(self.get, 8080, "secret")
        resume = self.events.index("/internal/deployment/resume")
        signal_index = next(i for i, event in enumerate(self.events) if isinstance(event, list))
        self.assertLess(resume, signal_index)
        self.assertEqual(self.events.count("/internal/deployment/pause"), 2)

    def test_alert_threshold_continues_waiting_for_connections(self):
        with patch.dict(os.environ, {"MONOIZE_SWAP_DRAIN_MAX_SECONDS": "1"}), \
                patch.object(pgswap.time, "monotonic", side_effect=[0, 10]), \
                patch.object(pgswap, "connections", side_effect=[["ESTAB"], [], []]), \
                patch.object(pgswap, "log") as log:
            pgswap.drain_and_handover(self.get, 8080, "secret")
            self.assertTrue(any("ALERT" in call.args[0] for call in log.call_args_list))
        pgswap.time.sleep.assert_called_once_with(15)
        self.run.assert_called_once_with(["docker", "kill", "--signal=SIGHUP", "monoize-prev"])

    def test_handover_signal_failure_does_not_resume_the_old_primary(self):
        self.run.side_effect = RuntimeError("uncertain signal outcome")
        with patch.object(pgswap, "connections", return_value=[]):
            with self.assertRaises(RuntimeError):
                pgswap.drain_and_handover(self.get, 8080, "secret")
        self.assertNotIn("/internal/deployment/resume", self.events)

    def test_handover_timeout_never_stops_old_container(self):
        def get(path, **kwargs):
            if path.endswith("status"):
                return 200, {"mode": "paused", "lease_owned": False}
            return self.get(path, **kwargs)

        with patch.object(pgswap, "connections", return_value=[]):
            with self.assertRaises(RuntimeError):
                pgswap.drain_and_handover(get, 8080, "secret")
        self.run.assert_called_once_with(["docker", "kill", "--signal=SIGHUP", "monoize-prev"])
        self.assertNotIn("/internal/deployment/resume", self.events)


class CutoverTests(unittest.TestCase):
    def test_probe_and_route_failures_retain_instances_without_handover(self):
        for failing in ["start", "switch", "public", "verify", "finish"]:
            with self.subTest(failing=failing):
                probe = Mock()
                switch, verify, public = Mock(), Mock(), Mock()
                target = {"start": probe.start, "switch": switch, "public": public,
                          "verify": verify, "finish": probe.finish}[failing]
                target.side_effect = RuntimeError("failed check")
                get = Mock(return_value=(200, {"mode": "forwarding", "lease_owned": False}))
                with patch.object(pgswap, "drain_and_handover") as drain, \
                        patch.object(pgswap, "run") as run, patch.object(pgswap.time, "sleep"):
                    with self.assertRaises(RuntimeError):
                        pgswap.perform_cutover(probe, switch, verify, public, get, 8080, "secret")
                    drain.assert_not_called()
                    run.assert_not_called()
                probe.close.assert_called_once()
                if failing == "start":
                    switch.assert_not_called()

    def test_probe_finishes_before_unbounded_drain(self):
        events = []
        probe = Mock()
        probe.start.side_effect = lambda: events.append("probe-start")
        probe.finish.side_effect = lambda: events.append("probe-finish")
        probe.close.side_effect = lambda: events.append("probe-close")
        get = Mock(return_value=(200, {"mode": "forwarding", "lease_owned": False}))
        with patch.object(pgswap, "drain_and_handover", side_effect=lambda *args: events.append("drain")), \
                patch.object(pgswap.time, "sleep"):
            pgswap.perform_cutover(probe, lambda: events.append("switch"), lambda: events.append("verify"),
                                   lambda: events.append("public"), get, 8080, "secret")
        self.assertEqual(events, ["probe-start", "switch", "public", "verify", "probe-finish", "drain", "probe-close"])

    def test_socket_query_failure_propagates(self):
        with patch.object(pgswap, "run", side_effect=subprocess.CalledProcessError(1, "ss")):
            with self.assertRaises(subprocess.CalledProcessError):
                pgswap.connections(8080)


class DeploymentFlowTests(unittest.TestCase):
    def exercise(self, probe_failure=False, handover_marker_failure=False, candidate_get_failure=False):
        events = []
        phase = {"handover": False}
        with tempfile.TemporaryDirectory(dir=PROJECT / "local-test") as directory:
            root = pathlib.Path(directory)
            data = root / "data"
            data.mkdir()
            (root / "blue-green-route.state").write_text("8080 8080 999\n")
            old = {"Image": "sha256:old", "State": {"Running": True, "ExitCode": 0},
                   "Config": {"Image": "monoize:old", "Env": [
                       "MONOIZE_DATABASE_DSN=postgres://test:secret@127.0.0.1/actual",
                       "MONOIZE_LISTEN=127.0.0.1:8080"], "User": "1000:1000"},
                   "HostConfig": {"NetworkMode": "host", "ExtraHosts": [],
                                  "RestartPolicy": {"Name": "on-failure", "MaximumRetryCount": 3}},
                   "Mounts": [{"Source": str(data), "Destination": "/app/data"}]}

            def inspect(name):
                if name == "monoize-prev":
                    return {"State": {"Running": False, "ExitCode": 0}}
                if phase["handover"]:
                    return {"State": {"Running": True}, "Image": "sha256:new"}
                return old

            def run(command, **kwargs):
                events.append(command)
                if command[:3] == ["docker", "image", "inspect"]:
                    return json.dumps([{"Id": "sha256:new"}])
                if command[:3] == ["docker", "exec", "monoize"]:
                    if command[-1] in {"-u", "-g"}:
                        return "1000\n"
                    if handover_marker_failure:
                        raise subprocess.CalledProcessError(1, command)
                if command[:2] == ["id", "-u"]:
                    return "999\n"
                if "--signal=SIGHUP" in command:
                    phase["handover"] = True
                return ""

            def external(command, **kwargs):
                if command[:2] == ["docker", "inspect"]:
                    return subprocess.CompletedProcess(command, 1)
                if command[0] == "curl":
                    events.append("public-ready")
                    return subprocess.CompletedProcess(command, 0, stdout="200")
                raise AssertionError(command)

            database = Mock()
            database.backup.side_effect = lambda _: events.append("backup")

            def sql(statement):
                events.append("sql:" + statement.split()[0])
                if "SELECT owner_id" in statement:
                    return "new-owner" if phase["handover"] else "old-owner"
                if "SELECT id FROM users" in statement:
                    return "11111111-1111-4111-8111-111111111111"
                if "json_agg" in statement:
                    return "[]"
                return ""

            database.sql.side_effect = sql
            client = Mock()

            def open_request(request, timeout=10):
                path = pgswap.urllib.parse.urlsplit(request.full_url).path
                events.append(path)
                if candidate_get_failure and path == "/api/dashboard/store/catalog":
                    raise RuntimeError("candidate GET failed")
                mode = "local" if phase["handover"] else "forwarding"
                if path.endswith("pause"):
                    mode = "paused"
                response = MagicMock()
                response.status = 200
                response.read.return_value = json.dumps({"mode": mode, "lease_owned": phase["handover"]})
                response.__enter__.return_value = response
                return response

            client.open.side_effect = open_request
            probe = Mock()
            probe.start.side_effect = lambda: events.append("probe-start")

            def finish():
                events.append("probe-finish")
                if probe_failure:
                    raise RuntimeError("cutover probe failed")

            probe.finish.side_effect = finish
            with patch.object(pgswap, "ROOT", root), patch.object(pgswap, "inspect", side_effect=inspect), \
                    patch.object(pgswap, "run", side_effect=run), patch.object(pgswap.subprocess, "run", side_effect=external), \
                    patch.object(pgswap, "DatabaseClient", return_value=database), \
                    patch.object(pgswap, "CutoverProbe", return_value=probe), \
                    patch.object(pgswap, "verify_migrations", return_value="a" * 64), \
                    patch.object(pgswap, "verify_caddy"), patch.object(pgswap, "connections", return_value=[]), \
                    patch.object(pgswap.urllib.request, "build_opener", return_value=client), \
                    patch.object(pgswap.os, "chown", create=True), patch.object(pgswap.os, "umask"), \
                    patch.object(pgswap.signal, "signal"), patch.object(pgswap.time, "sleep"), patch.object(pgswap, "log"), \
                    patch.object(sys, "argv", ["blue-green-swap.sh", "new"]), \
                    patch.dict(sys.modules, {"fcntl": types.SimpleNamespace(flock=Mock(), LOCK_EX=1, LOCK_NB=2)}), \
                    patch.dict(os.environ, {"MONOIZE_SWAP_PUBLIC_URL": "https://www.example.com/",
                                            "MONOIZE_SWAP_READY_URL": "https://api.example.com/readyz",
                                            "MONOIZE_SWAP_PUBLIC_IP": "40.160.141.21"}):
                if probe_failure or handover_marker_failure or candidate_get_failure:
                    with self.assertRaises((RuntimeError, subprocess.CalledProcessError)):
                        pgswap.main()
                else:
                    pgswap.main()
            return events, database, probe

    def test_complete_flow_backs_up_before_start_and_drains_before_stop(self):
        events, database, probe = self.exercise()
        start = next(i for i, event in enumerate(events) if isinstance(event, list) and event[:2] == ["docker", "run"])
        handover = events.index(["docker", "kill", "--signal=SIGHUP", "monoize-prev"])
        stop = events.index(["docker", "kill", "--signal=SIGTERM", "monoize-prev"])
        self.assertLess(events.index("backup"), start)
        self.assertLess(events.index("probe-finish"), handover)
        self.assertLess(handover, stop)
        self.assertEqual(events.count("public-ready"), 2)
        self.assertIn("on-failure:3", events[start])
        self.assertEqual(events[start][-1], "sha256:new")
        probe.close.assert_called_once()

    def test_failed_public_cutover_probe_retains_both_instances(self):
        events, database, probe = self.exercise(probe_failure=True)
        self.assertIn(["docker", "rename", "monoize", "monoize-prev"], events)
        self.assertFalse(any(isinstance(event, list) and event[:2] == ["docker", "kill"] for event in events))
        self.assertNotIn(["docker", "rename", "monoize-next", "monoize"], events)
        probe.close.assert_called_once()

    def test_missing_handover_capability_never_starts_or_signals_a_container(self):
        events, database, probe = self.exercise(handover_marker_failure=True)
        self.assertFalse(any(isinstance(event, list) and event[:2] in [["docker", "kill"], ["docker", "run"]]
                             for event in events))
        database.backup.assert_not_called()
        probe.start.assert_not_called()

    def test_candidate_get_failure_removes_session_before_aborting(self):
        events, database, probe = self.exercise(candidate_get_failure=True)
        self.assertIn("sql:INSERT", events)
        self.assertIn("sql:DELETE", events)
        self.assertFalse(any(isinstance(event, list) and event[:2] == ["docker", "kill"] for event in events))
        self.assertNotIn(["docker", "rename", "monoize", "monoize-prev"], events)
        probe.start.assert_not_called()


if __name__ == "__main__":
    unittest.main()

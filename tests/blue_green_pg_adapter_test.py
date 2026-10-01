import importlib.util
import pathlib
import subprocess
import unittest
from unittest.mock import patch

path = pathlib.Path(__file__).parents[1] / "scripts/blue-green-pg-swap.py"
spec = importlib.util.spec_from_file_location("pgswap", path)
pgswap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pgswap)


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

    def test_socket_query_failure_propagates(self):
        with patch.object(pgswap, "run", side_effect=subprocess.CalledProcessError(1, "ss")):
            with self.assertRaises(subprocess.CalledProcessError):
                pgswap.connections(8080)


if __name__ == "__main__":
    unittest.main()

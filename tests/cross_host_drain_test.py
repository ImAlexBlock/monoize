import importlib.util
import pathlib
import unittest

path = pathlib.Path(__file__).parents[1] / "scripts/cross-host-drain-watch.py"
spec = importlib.util.spec_from_file_location("drain", path)
drain = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drain)


class DrainTests(unittest.TestCase):
    def test_listeners_and_terminal_states_are_excluded(self):
        text = "LISTEN 0 4096 127.0.0.1:8080 0.0.0.0:*\nTIME-WAIT 0 0 127.0.0.1:8080 127.0.0.1:1234"
        self.assertEqual(drain.connections(text), ({}, 0))

    def test_half_closed_and_queued_connections_are_retained(self):
        text = "ESTAB 0 2500000 127.0.0.1:8080 127.0.0.1:1234\nCLOSE-WAIT 2 3 [::1]:7883 [::1]:1235"
        self.assertEqual(drain.connections(text), ({"8080:ESTAB": 1, "7883:CLOSE-WAIT": 1}, 2500003))

    def test_outgoing_socket_is_not_an_accepted_connection(self):
        self.assertEqual(drain.connections("ESTAB 0 0 127.0.0.1:40000 127.0.0.1:8080"), ({}, 0))

    def test_malformed_output_is_not_zero_connections(self):
        with self.assertRaises(ValueError):
            drain.connections("bad output")


if __name__ == "__main__":
    unittest.main()

import base64
import hashlib
import importlib.util
import io
import pathlib
import unittest

path = pathlib.Path(__file__).parents[1] / "scripts/cross-host-db-manifest.py"
spec = importlib.util.spec_from_file_location("manifest", path)
manifest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manifest)


class ManifestTests(unittest.TestCase):
    def section(self, label, rows):
        return b"CHV_BEGIN " + base64.b64encode(label.encode()) + b"\n" + rows + b"CHV_END\n"

    def test_rows_are_counted_and_hashed_without_parsing(self):
        data = b'{"a":"escaped\\\\n"}\n{"a":"escaped\\\\n"}\n'
        result = manifest.consume(io.BytesIO(self.section("table:a", data)), ["table:a"])
        self.assertEqual(result["table:a"]["rows"], 2)
        self.assertEqual(result["table:a"]["sha256"], hashlib.sha256(data).hexdigest())

    def test_empty_table(self):
        result = manifest.consume(io.BytesIO(self.section("table:a", b"")), ["table:a"])
        self.assertEqual(result["table:a"]["rows"], 0)

    def test_truncation_fails(self):
        with self.assertRaises(RuntimeError):
            manifest.consume(io.BytesIO(b"CHV_BEGIN dGFibGU6YQ==\n"), ["table:a"])

    def test_missing_section_fails(self):
        with self.assertRaises(RuntimeError):
            manifest.consume(io.BytesIO(b""), ["table:a"])

    def test_unexpected_output_fails(self):
        with self.assertRaises(RuntimeError):
            manifest.consume(io.BytesIO(b"ERROR\n"), [])

    def test_wrong_order_fails(self):
        with self.assertRaises(RuntimeError):
            manifest.consume(io.BytesIO(self.section("table:b", b"")), ["table:a"])

    def test_identifier_escaping(self):
        self.assertEqual(manifest.identifier('a"b'), '"a""b"')
        self.assertIn('COLLATE "C"', manifest.table_query("public", "users"))

    def test_identifier_newline_is_rejected(self):
        with self.assertRaises(ValueError):
            manifest.identifier("table\n\\echo unsafe")


if __name__ == "__main__":
    unittest.main()

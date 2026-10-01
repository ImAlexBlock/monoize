#!/bin/sh
set -eu
python3 - <<'PY'
import collections
import hashlib
from html.parser import HTMLParser
import json
import pathlib
import urllib.parse
import urllib.request

class Scripts(HTMLParser):
    def __init__(self):
        super().__init__()
        self.paths = []
    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if tag == "script" and attributes.get("type") == "module" and attributes.get("src"):
            self.paths.append(attributes["src"])

client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def fetch(url):
    with client.open(urllib.request.Request(url, headers={"Cache-Control": "no-cache"}), timeout=30) as response:
        return response.read()

bases = ["http://127.0.0.1:8080", "https://www.lynshen.org"]
results = []
for base in bases:
    parser = Scripts()
    parser.feed(fetch(base + "/dashboard/users").decode())
    assert parser.paths
    values = {}
    for path in parser.paths:
        url = urllib.parse.urljoin(base, path)
        assert urllib.parse.urlparse(url).netloc == urllib.parse.urlparse(base).netloc
        values[path] = hashlib.sha256(fetch(url)).hexdigest()
    results.append(values)
assert results[0] == results[1], "Public frontend differs from recovery candidate"
print("PUBLIC_CANDIDATE_MODULES_MATCH", json.dumps(results[0]))
root = pathlib.Path("/opt/migration-20260930")
reports = sorted((root / "final").glob("dashboard-selfcheck-*.json"), key=lambda p: p.stat().st_mtime)
report = json.loads(reports[-1].read_text())
print("MATRIX", reports[-1].name, report["at"], dict(collections.Counter(x.get("status", 0) for x in report["results"])))
assert len(report["results"]) == 50 and all(x.get("status") in (200, 403) for x in report["results"])
print("DIAGNOSTIC_SESSION_REMOVED", "DIAGNOSTIC_SESSION_REMOVED" in (root / "audit_dashboard_matrix.log").read_text())
PY

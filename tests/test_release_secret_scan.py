import contextlib
import io
import json
import pathlib
import re
import sys
import tempfile
import unittest
from unittest.mock import patch

from scripts import release_secret_scan


class ReleaseSecretScanTests(unittest.TestCase):
    def test_finding_reports_location_without_source_text(self):
        marker = "scan-fixture"
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp).resolve()
            source = root / "sample.txt"
            source.write_text(f"ordinary text\n{marker}\n", encoding="utf-8")
            report = root / "report.json"
            stdout = io.StringIO()
            stderr = io.StringIO()
            args = ["release_secret_scan.py", "--repo-root", str(root), "--report", str(report)]
            with (
                patch.object(release_secret_scan, "repo_files", return_value=[source]),
                patch.object(release_secret_scan, "PATTERNS", [("fixture", re.compile(marker))]),
                patch.object(sys, "argv", args),
                contextlib.redirect_stdout(stdout),
                contextlib.redirect_stderr(stderr),
            ):
                self.assertEqual(release_secret_scan.main(), 1)

            data = json.loads(report.read_text(encoding="utf-8"))
            self.assertEqual(data["total_findings"], 1)
            self.assertEqual(
                data["findings"],
                [{"file": "sample.txt", "line": 2, "pattern": "fixture", "snippet": "[redacted]"}],
            )
            self.assertIn("FAILED", stdout.getvalue())
            self.assertEqual(stderr.getvalue(), "- sample.txt:2 [fixture] [redacted]\n")

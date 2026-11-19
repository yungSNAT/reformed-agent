"""The gitnexus-explorer proxy serves only its dist folder, on loopback only.

Starts the skill's ``scripts/proxy.mjs``, and the copy its SKILL.md tells the
agent to write, with Node on a free port over a temporary dist folder that
holds links pointing out of it, then requests files through each.

Needs Node and a host that can create symbolic links: ``python3 -m unittest
tests/skills/test_gitnexus_proxy.py``.
"""

from __future__ import annotations

import http.client
import os
import re
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SKILL = ROOT / "optional-skills/research/gitnexus-explorer"
SECRET = "outside the dist folder"


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def _skill_md_proxy() -> str:
    """The JavaScript block of SKILL.md's 'Create the Proxy Script' step."""
    text = (SKILL / "SKILL.md").read_text(encoding="utf-8")
    step = text.split("### 4. Create the Proxy Script", 1)[1]
    return step.split("```javascript\n", 1)[1].split("```", 1)[0]


class ScriptProxyTests(unittest.TestCase):
    def proxy(self, base: Path) -> Path:
        return SKILL / "scripts/proxy.mjs"

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        base = Path(tmp.name)
        outside = base / "outside"
        outside.mkdir()
        (outside / "secret.txt").write_text(SECRET, encoding="utf-8")
        dist = base / "dist"
        dist.mkdir()
        (dist / "index.html").write_text("<p>index</p>", encoding="utf-8")
        (dist / "app.js").write_text("console.log('app');", encoding="utf-8")
        os.symlink(outside / "secret.txt", dist / "leak.txt")
        os.symlink(outside, dist / "linked")
        # A dist path given through a link still serves its own files.
        os.symlink(dist, base / "dist-link", target_is_directory=True)

        self.port = _free_port()
        self.proc = subprocess.Popen(
            ["node", str(self.proxy(base)), str(base / "dist-link"), str(self.port)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self.addCleanup(self._stop)
        self.banner = self.proc.stdout.readline()

    def _stop(self) -> None:
        self.proc.kill()
        self.proc.wait()
        self.proc.stdout.close()

    def _get(self, path: str) -> tuple[int, str]:
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        try:
            conn.request("GET", path)
            resp = conn.getresponse()
            return resp.status, resp.read().decode("utf-8", "replace")
        finally:
            conn.close()

    def test_listens_on_loopback_only(self) -> None:
        match = re.search(r"on http://(\S+):(\d+)", self.banner)
        self.assertIsNotNone(match, self.banner)
        self.assertEqual(match.group(1), "127.0.0.1", self.banner)
        self.assertEqual(int(match.group(2)), self.port, self.banner)

    def test_serves_files_inside_dist(self) -> None:
        self.assertEqual(self._get("/app.js"), (200, "console.log('app');"))
        self.assertEqual(self._get("/"), (200, "<p>index</p>"))

    def test_refuses_links_that_leave_dist(self) -> None:
        for path in ("/leak.txt", "/linked/secret.txt"):
            with self.subTest(path=path):
                status, body = self._get(path)
                self.assertEqual(status, 404, body)
                self.assertNotIn(SECRET, body)

    def test_refuses_dot_dot_paths(self) -> None:
        status, body = self._get("/../outside/secret.txt")
        self.assertEqual(status, 404, body)
        self.assertNotIn(SECRET, body)


class SkillMdProxyTests(ScriptProxyTests):
    def proxy(self, base: Path) -> Path:
        path = base / "proxy.mjs"
        path.write_text(_skill_md_proxy(), encoding="utf-8")
        return path


if __name__ == "__main__":
    unittest.main()

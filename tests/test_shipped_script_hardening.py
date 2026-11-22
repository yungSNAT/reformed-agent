"""Hardening in skill and plugin scripts that ship with the repository.

Each module is loaded from its file. The host modules a plugin imports at the
top (``hermes_cli``, ``hermes_constants``, the Honcho client) are not part of
this repository, so the tests put minimal stand-ins in ``sys.modules``.

Runs with the standard library alone: ``python3 -m unittest
tests/test_shipped_script_hardening.py``.
"""

from __future__ import annotations

import importlib.util
import ssl
import sys
import types
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]


def _load(relative: str, stubs: dict[str, dict[str, object]] | None = None) -> types.ModuleType:
    modules = {}
    for name, attributes in (stubs or {}).items():
        module = types.ModuleType(name)
        for key, value in attributes.items():
            setattr(module, key, value)
        modules[name] = module
    path = ROOT / relative
    spec = importlib.util.spec_from_file_location(f"hardening_{path.stem}", path)
    module = importlib.util.module_from_spec(spec)
    # Dataclasses look their module up in sys.modules while the class is built.
    modules[spec.name] = module
    with mock.patch.dict(sys.modules, modules):
        spec.loader.exec_module(module)
    return module


class DomainIntelTlsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = _load("optional-skills/research/domain-intel/scripts/domain_intel.py")

    def test_verified_context_refuses_protocols_older_than_tls_1_2(self) -> None:
        ctx = self.module.tls_context()
        self.assertEqual(ctx.minimum_version, ssl.TLSVersion.TLSv1_2)
        self.assertEqual(ctx.verify_mode, ssl.CERT_REQUIRED)
        self.assertTrue(ctx.check_hostname)

    def test_unverified_context_still_refuses_old_protocols(self) -> None:
        ctx = self.module.tls_context(verify=False)
        self.assertEqual(ctx.minimum_version, ssl.TLSVersion.TLSv1_2)
        self.assertEqual(ctx.verify_mode, ssl.CERT_NONE)
        self.assertFalse(ctx.check_hostname)


class OpenClawHostTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = _load("optional-skills/migration/openclaw-migration/scripts/openclaw_to_hermes.py")

    def test_host_comes_from_the_url_authority(self) -> None:
        url_host = self.module.url_host
        self.assertEqual(url_host("https://API.OpenAI.com/v1"), "api.openai.com")
        self.assertEqual(url_host("api.openai.com/v1"), "api.openai.com")
        self.assertEqual(url_host("https://openai.com.proxy.example/v1"), "openai.com.proxy.example")
        self.assertEqual(url_host("https://proxy.example/openai.com"), "proxy.example")
        self.assertEqual(url_host("http://[::1"), "")


class SpotifyIdTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = _load(
            "plugins/spotify/client.py",
            {
                "httpx": {},
                "hermes_cli": {},
                "hermes_cli.auth": {"AuthError": RuntimeError, "resolve_spotify_runtime_credentials": None},
            },
        )

    def test_open_spotify_links_give_their_id(self) -> None:
        normalize = self.module.normalize_spotify_id
        self.assertEqual(normalize("https://open.spotify.com/track/abc123?si=x"), "abc123")
        self.assertEqual(normalize("open.spotify.com/playlist/p1", "playlist"), "p1")
        with self.assertRaises(self.module.SpotifyError):
            normalize("https://open.spotify.com/album/a1", "track")

    def test_other_hosts_are_not_read_as_spotify_links(self) -> None:
        normalize = self.module.normalize_spotify_id
        url = "https://evil.example/open.spotify.com/track/abc123"
        self.assertEqual(normalize(url), url)


class AchievementsImportFailureTests(unittest.TestCase):
    def test_response_names_the_failure_and_the_log_keeps_the_reason(self) -> None:
        module = _load("plugins/hermes-achievements/dashboard/plugin_api.py")
        with mock.patch.dict(sys.modules, {"hermes_state": None}), self.assertLogs(level="WARNING") as logs:
            result = module.scan_sessions()
        self.assertEqual(result["error"], "Could not import SessionDB")
        self.assertEqual(result["scan_meta"]["mode"], "failed")
        self.assertIn("hermes_state", "\n".join(logs.output))


class HonchoKeyStateTests(unittest.TestCase):
    def test_key_state_shows_no_part_of_the_key(self) -> None:
        module = _load(
            "plugins/memory/honcho/cli.py",
            {
                "hermes_constants": {"get_hermes_home": lambda: Path(".")},
                "plugins": {},
                "plugins.memory": {},
                "plugins.memory.honcho": {},
                "plugins.memory.honcho.client": {
                    "resolve_active_host": None,
                    "resolve_config_path": None,
                    "HOST": "hermes",
                },
                "hermes_cli": {},
                "hermes_cli.config": {"cfg_get": None},
            },
        )
        self.assertEqual(module.key_state("hc-0123456789abcdef"), "set")
        self.assertEqual(module.key_state(""), "not set")


if __name__ == "__main__":
    unittest.main()

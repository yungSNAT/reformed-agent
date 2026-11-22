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


def _stub_modules(stubs: dict[str, dict[str, object]] | None) -> dict[str, types.ModuleType]:
    modules = {}
    for name, attributes in (stubs or {}).items():
        module = types.ModuleType(name)
        for key, value in attributes.items():
            setattr(module, key, value)
        modules[name] = module
    return modules


def _load(relative: str, stubs: dict[str, dict[str, object]] | None = None) -> types.ModuleType:
    modules = _stub_modules(stubs)
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

    def test_check_ssl_takes_both_contexts_from_the_helper(self) -> None:
        wrapped = mock.MagicMock()
        wrapped.__enter__.return_value.getpeercert.return_value = {}
        wrapped.__enter__.return_value.cipher.return_value = ("TLS_AES_128_GCM_SHA256", "TLSv1.3", 128)
        contexts = [mock.MagicMock(), mock.MagicMock()]
        contexts[0].wrap_socket.side_effect = ssl.SSLCertVerificationError("expired")
        contexts[1].wrap_socket.return_value = wrapped
        with mock.patch.object(self.module, "tls_context", side_effect=contexts) as helper, mock.patch.object(
            self.module.socket, "create_connection", return_value=mock.MagicMock()
        ):
            result = self.module.check_ssl("example.test")
        self.assertEqual(helper.call_args_list, [mock.call(), mock.call(verify=False)])
        self.assertIn("expired", result["verification_warning"])

    def test_check_available_takes_its_context_from_the_helper(self) -> None:
        context = mock.MagicMock()
        with (
            mock.patch.object(self.module, "tls_context", return_value=context) as helper,
            mock.patch.object(self.module.socket, "getaddrinfo", side_effect=OSError),
            mock.patch.object(self.module.socket, "create_connection", return_value=mock.MagicMock()),
            mock.patch.object(self.module.urllib.request, "urlopen", side_effect=OSError),
        ):
            result = self.module.check_available("example.invalid-tld")
        helper.assert_called_once_with(verify=False)
        context.wrap_socket.assert_called_once()
        self.assertTrue(result["signals"]["ssl_reachable"])


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
        self.assertEqual(url_host("//api.openai.com/v1"), "api.openai.com")
        self.assertEqual(url_host("https://user:pw@API.openai.com:8443/v1"), "api.openai.com")

    def _provider_env_vars(self, base_url: str) -> list[str]:
        import tempfile

        added: list[str] = []
        with tempfile.TemporaryDirectory() as source, tempfile.TemporaryDirectory() as target:
            migrator = self.module.Migrator(
                source_root=Path(source),
                target_root=Path(target),
                execute=False,
                workspace_target=None,
                overwrite=False,
                migrate_secrets=True,
                output_dir=None,
            )
            migrator.merge_env_values = lambda values, *_args, **_kwargs: added.extend(values)
            migrator.migrate_provider_keys(
                {"models": {"providers": {"custom": {"apiKey": "placeholder", "baseUrl": base_url}}}}
            )
        return added

    def test_provider_key_mapping_reads_the_host(self) -> None:
        self.assertEqual(self._provider_env_vars("https://api.openai.com/v1"), ["OPENAI_API_KEY"])
        self.assertEqual(self._provider_env_vars("//api.openai.com/v1"), ["OPENAI_API_KEY"])
        self.assertEqual(self._provider_env_vars("https://openai.com.proxy.example/v1"), [])
        self.assertEqual(self._provider_env_vars("https://proxy.example/openai.com/v1"), [])


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
        self.assertEqual(normalize("//open.spotify.com/track/abc123"), "abc123")
        self.assertEqual(normalize("https://OPEN.spotify.com:443/track/abc123"), "abc123")
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

    def test_scan_status_keeps_the_failure_text_in_the_log(self) -> None:
        module = _load("plugins/hermes-achievements/dashboard/plugin_api.py")
        failure = RuntimeError("cannot open /home/someone/.hermes/state.db")
        with mock.patch.object(module, "compute_all", side_effect=failure), self.assertLogs(level="ERROR") as logs:
            module._run_scan_and_update_cache(False)
        status = module._scan_status_payload()
        self.assertEqual(status["state"], "failed")
        self.assertNotIn("state.db", str(status["last_error"]))
        self.assertIn("state.db", "\n".join(logs.output))


KEY = "hc-0123456789abcdef-tail"


class HonchoKeyStateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = _load(
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

    def test_key_state_shows_no_part_of_the_key(self) -> None:
        self.assertEqual(self.module.key_state(KEY), "set")
        self.assertEqual(self.module.key_state(""), "not set")

    def test_status_shows_no_part_of_the_key(self) -> None:
        import contextlib
        import io

        config = mock.MagicMock(api_key=KEY, enabled=False, raw={}, host="hermes")
        client = types.ModuleType("plugins.memory.honcho.client")
        client.HonchoClientConfig = mock.MagicMock(**{"from_global_config.return_value": config})
        client.get_honcho_client = None
        out = io.StringIO()
        with (
            mock.patch.dict(sys.modules, {"honcho": types.ModuleType("honcho"), client.__name__: client}),
            mock.patch.multiple(
                self.module,
                _read_config=lambda: {"apiKey": KEY},
                _config_path=lambda: Path("honcho.json"),
                _local_config_path=lambda: Path("honcho.json"),
                _active_profile_name=lambda: "default",
                _host_key=lambda: "hermes",
            ),
            contextlib.redirect_stdout(out),
        ):
            self.module.cmd_status(types.SimpleNamespace(all=False))
        self.assertIn("API key:        set", out.getvalue())
        self.assertNotIn(KEY[-4:], out.getvalue())
        self.assertNotIn(KEY[:6], out.getvalue())

    def _output_until_first_prompt(self, command, **patches) -> str:
        import contextlib
        import io

        def prompt(label, *_args, **_kwargs):
            if label == "Cloud or local?":
                return "cloud"
            raise StopAtPrompt

        out = io.StringIO()
        with (
            mock.patch.multiple(self.module, _read_config=lambda: {"apiKey": KEY}, _prompt=prompt, **patches),
            contextlib.redirect_stdout(out),
            self.assertRaises(StopAtPrompt),
        ):
            command(types.SimpleNamespace())
        return out.getvalue()

    def test_setup_shows_no_part_of_the_key(self) -> None:
        output = self._output_until_first_prompt(
            self.module.cmd_setup,
            _ensure_sdk_installed=lambda: True,
            _config_path=lambda: Path("honcho.json"),
            _local_config_path=lambda: Path("honcho.json"),
            _host_key=lambda: "hermes",
        )
        self.assertIn("Current API key: set", output)
        self.assertNotIn(KEY[-4:], output)

    def test_migrate_shows_no_part_of_the_key(self) -> None:
        output = self._output_until_first_prompt(self.module.cmd_migrate, _resolve_api_key=lambda _cfg: KEY)
        self.assertIn("Honcho API key already configured.", output)
        self.assertNotIn(KEY[-4:], output)


class StopAtPrompt(Exception):
    pass


class HindsightSetupTests(unittest.TestCase):
    def test_key_prompt_shows_no_part_of_the_existing_key(self) -> None:
        import contextlib
        import io

        class Stdin(io.StringIO):
            def isatty(self) -> bool:
                return False

            def readline(self, *_args) -> str:
                raise StopAtPrompt

        stubs = {
            "agent": {},
            "agent.memory_provider": {"MemoryProvider": type("MemoryProvider", (), {})},
            "hermes_constants": {"get_hermes_home": lambda: Path(".")},
            "tools": {},
            "tools.registry": {"tool_error": None},
            "hermes_cli": {},
            "hermes_cli.config": {"cfg_get": None, "save_config": None},
            "hermes_cli.memory_setup": {"_curses_select": lambda *_args, **_kwargs: 0},
        }
        module = _load("plugins/memory/hindsight/__init__.py", stubs)
        provider = object.__new__(module.HindsightMemoryProvider)
        provider._config = {}
        out = io.StringIO()
        with (
            mock.patch.dict(sys.modules, _stub_modules(stubs)),
            mock.patch.dict("os.environ", {"HINDSIGHT_API_KEY": KEY}),
            mock.patch("shutil.which", return_value=None),
            mock.patch("sys.stdin", Stdin()),
            contextlib.redirect_stdout(out),
            self.assertRaises(StopAtPrompt),
        ):
            provider.post_setup(".", {})
        self.assertIn("current: set", out.getvalue())
        self.assertNotIn(KEY[-4:], out.getvalue())


if __name__ == "__main__":
    unittest.main()

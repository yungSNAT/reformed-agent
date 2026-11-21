import os
import plistlib
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"


@unittest.skipUnless(os.name == "posix", "launchd wrappers require Bash")
class WebhookLaunchdTests(unittest.TestCase):
    def test_generated_settings_are_valid_shell_assignments(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            repo = root / "repo with 'quotes' & spaces"
            wrappers = repo / "scripts"
            wrappers.mkdir(parents=True)
            for name in (
                "run-upstream-webhook-listener.sh",
                "run-upstream-webhook-worker.sh",
                "upstream_webhook_sync.py",
            ):
                (wrappers / name).touch()
            config = root / "generated-settings"
            install = [
                "env", f"PATH={os.defpath}", "UPSTREAM_SYNC_AUTO_GENERATE_SECRET=0",
                "bash", str(SCRIPTS / "install-upstream-webhook-launchd.sh"),
                "--repo-root", str(repo), "--agents-dir", str(root / "agents"),
                "--env-file", str(config), "--log-dir", str(root / "logs"), "--no-load",
            ]
            read_settings = [
                "bash", "-c",
                'set -eu; source "$1"; printf "%s\\0" '
                '"${UPSTREAM_SYNC_SQLITE_PATH:-}" "$UPSTREAM_SYNC_SUMMARY_CONTEXT_PROJECT" '
                '"$UPSTREAM_SYNC_SUMMARY_LOCAL_PATH" "$UPSTREAM_SYNC_ELITE_CMD"',
                "settings", str(config),
            ]
            expected = [
                str(repo / ".sync-queue" / "upstream-events.db"),
                repo.name,
                str(repo / ".sync-reports" / "upstream-sync-summary-fallback.log"),
                "python3 scripts/run-elite-sync-gate.py",
                "",
            ]

            def run(command):
                result = subprocess.run(command, capture_output=True, timeout=15)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                return result.stdout.decode().split("\0")

            run(install)
            self.assertEqual(run(read_settings), expected)

            config.write_text(
                "UPSTREAM_SYNC_ELITE_CMD=python3 scripts/run-elite-sync-gate.py\n"
                "UPSTREAM_SYNC_SUMMARY_LOCAL_PATH='custom report path'\n",
            )
            run(install)
            expected[0] = ""
            expected[2] = "custom report path"
            self.assertEqual(run(read_settings), expected)

            with config.open("a") as existing:
                existing.write("UPSTREAM_SYNC_ELITE_CMD=python3 scripts/run-elite-sync-gate.py\n")
                existing.write("UPSTREAM_SYNC_ELITE_CMD='custom gate command'\n")
            run(install)
            expected[3] = "custom gate command"
            self.assertEqual(run(read_settings), expected)

    def test_plists_preserve_configured_paths(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            fixture = root / "listener & worker"
            wrappers = fixture / "scripts"
            wrappers.mkdir(parents=True)
            for name in (
                "run-upstream-webhook-listener.sh",
                "run-upstream-webhook-worker.sh",
                "upstream_webhook_sync.py",
            ):
                (wrappers / name).touch()
            agents = root / "launch agents"
            config = root / "settings & state" / "webhook input\r\n"
            logs = root / "listener & worker logs <local>"
            command_bin = root / "commands"
            command_bin.mkdir()
            launchctl = command_bin / "launchctl"
            launchctl.write_text('#!/bin/sh\n: > "${0%/*}/launchctl.called"\nexit 97\n')
            launchctl.chmod(0o700)
            label = "test.hermes.webhook"

            subprocess.run(
                [
                    "env", f"PATH={command_bin}{os.pathsep}{os.defpath}",
                    "UPSTREAM_SYNC_AUTO_GENERATE_SECRET=0", "bash",
                    str(SCRIPTS / "install-upstream-webhook-launchd.sh"),
                    "--repo-root", str(fixture), "--agents-dir", str(agents),
                    "--env-file", str(config), "--log-dir", str(logs),
                    "--label-prefix", label, "--no-load",
                ],
                check=True,
                capture_output=True,
                timeout=15,
            )
            self.assertFalse((command_bin / "launchctl.called").exists())

            for role in ("listener", "worker"):
                with self.subTest(role=role):
                    service = f"{label}.upstream_webhook_{role}"
                    data = plistlib.loads((agents / f"{service}.plist").read_bytes())
                    self.assertEqual(data["Label"], service)
                    self.assertEqual(data["WorkingDirectory"], str(fixture))
                    self.assertEqual(
                        data["ProgramArguments"],
                        ["/usr/bin/env", "bash", str(wrappers / f"run-upstream-webhook-{role}.sh")],
                    )
                    self.assertEqual(data["EnvironmentVariables"]["REPO_ROOT"], str(fixture))
                    self.assertEqual(data["EnvironmentVariables"]["UPSTREAM_SYNC_ENV_FILE"], str(config))
                    self.assertEqual(data["StandardOutPath"], str(logs / f"upstream-webhook-{role}.out.log"))
                    self.assertEqual(data["StandardErrorPath"], str(logs / f"upstream-webhook-{role}.err.log"))
                    self.assertTrue(data["RunAtLoad"])
                    self.assertTrue(data["KeepAlive"])

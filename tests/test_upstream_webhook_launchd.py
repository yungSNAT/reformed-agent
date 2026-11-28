import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"


@unittest.skipUnless(os.name == "posix", "launchd wrappers require Bash")
class WebhookLaunchdTests(unittest.TestCase):
    def test_wrappers_refuse_retired_backends_before_starting_python(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            python = root / "python3"
            python.write_text('#!/bin/sh\n: > "${0%/*}/python.called"\n')
            python.chmod(0o700)
            env = {
                "PATH": f"{root}{os.pathsep}{os.defpath}",
                "REPO_ROOT": str(root),
                "UPSTREAM_SYNC_ENV_FILE": str(root / "unused"),
                "UPSTREAM_SYNC_DISABLE_DEV_GUARD": "1",
            }
            for role in ("listener", "worker"):
                wrapper = SCRIPTS / f"run-upstream-webhook-{role}.sh"
                for backend in ("sqs", "kafka"):
                    with self.subTest(role=role, backend=backend):
                        result = subprocess.run(
                            ["bash", str(wrapper)], env={**env, "UPSTREAM_SYNC_BACKEND": backend},
                            capture_output=True, text=True, timeout=15, check=False,
                        )
                        self.assertEqual(result.returncode, 2, result.stderr)
                        self.assertIn("expected sqlite", result.stderr)
                        self.assertFalse((root / "python.called").exists())

    def test_guided_setup_preserves_literal_overrides(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            wrappers = root / "scripts"
            wrappers.mkdir()
            for name in (
                "run-upstream-webhook-listener.sh",
                "run-upstream-webhook-worker.sh",
                "upstream_webhook_sync.py",
            ):
                (wrappers / name).touch()
            installer = wrappers / "install-upstream-webhook-launchd.sh"
            shutil.copyfile(SCRIPTS / installer.name, installer)
            installer.chmod(0o700)
            status = wrappers / "status-upstream-webhook-launchd.sh"
            status.write_text("#!/bin/sh\nexit 97\n")
            status.chmod(0o700)
            config = root / "generated-settings"
            key = "UPSTREAM_SYNC_SUMMARY_LOCAL_PATH"
            value = str(root / "Team's reports & notes")
            setup = [
                "env", f"PATH={os.defpath}", "UPSTREAM_SYNC_AUTO_GENERATE_SECRET=0",
                "bash", str(SCRIPTS / "setup-upstream-webhook-launchd.sh"),
                "--repo-root", str(root), "--agents-dir", str(root / "agents"),
                "--env-file", str(config), "--log-dir", str(root / "logs"),
                "--show-only", "--no-auto-secret",
            ]

            result = subprocess.run(
                [*setup, "--set", f"{key}={value}"], capture_output=True, text=True, timeout=15,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Show-only mode complete.", result.stdout)
            roundtrip = subprocess.run(
                [
                    "bash", "-c", 'set -eu; source "$1"; printf "%s" "$UPSTREAM_SYNC_SUMMARY_LOCAL_PATH"',
                    "settings", str(config),
                ],
                text=True,
                capture_output=True,
                check=True,
                timeout=15,
            )
            self.assertEqual(roundtrip.stdout, value)

            before = config.read_bytes()
            bad_key = subprocess.run(
                [*setup, "--set", f"{key}=unused", "--set", "invalid-key=value"],
                capture_output=True, text=True, timeout=15,
            )
            self.assertEqual(bad_key.returncode, 2)
            self.assertIn("Invalid --set key", bad_key.stderr)
            self.assertEqual(config.read_bytes(), before)

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
            default_elite_cmd = (
                "cargo run --locked -q -p hermes-parity-tools -- run-elite-sync-gate"
            )
            expected = [
                str(repo / ".sync-queue" / "upstream-events.db"),
                repo.name,
                str(repo / ".sync-reports" / "upstream-sync-summary-fallback.log"),
                default_elite_cmd,
                "",
            ]

            def run(command):
                result = subprocess.run(command, capture_output=True, timeout=15)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                return result.stdout.decode().split("\0")

            run(install)
            self.assertEqual(run(read_settings), expected)

            expected[0] = ""
            expected[2] = "custom report path"
            legacy = "python3 scripts/run-elite-sync-gate.py"
            for assignment in (legacy, legacy.replace(" ", "\\ "), f"'{legacy}'", f'"{legacy}"'):
                with self.subTest(assignment=assignment):
                    config.write_text(
                        f"UPSTREAM_SYNC_ELITE_CMD={assignment}\n"
                        "UPSTREAM_SYNC_SUMMARY_LOCAL_PATH='custom report path'\n",
                    )
                    run(install)
                    self.assertEqual(run(read_settings), expected)

            config.write_text(
                f"UPSTREAM_SYNC_ELITE_CMD={default_elite_cmd}\n"
                "UPSTREAM_SYNC_SUMMARY_LOCAL_PATH='custom report path'\n",
            )
            run(install)
            self.assertEqual(run(read_settings), expected)

            with config.open("a") as existing:
                existing.write(f"UPSTREAM_SYNC_ELITE_CMD={default_elite_cmd}\n")
                existing.write("UPSTREAM_SYNC_ELITE_CMD='custom gate command'\n")
            run(install)
            expected[3] = "custom gate command"
            self.assertEqual(run(read_settings), expected)

    def test_installer_refuses_retired_backends_before_restarting_agents(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            stub = root / "launchctl"
            calls = root / "launchctl-calls"
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$LAUNCHCTL_CALLS"\n')
            stub.chmod(0o700)
            wrappers = root / "scripts"
            wrappers.mkdir()
            for name in (
                "run-upstream-webhook-listener.sh",
                "run-upstream-webhook-worker.sh",
                "upstream_webhook_sync.py",
            ):
                (wrappers / name).touch()
            config = root / "settings"
            install = [
                "bash", str(SCRIPTS / "install-upstream-webhook-launchd.sh"),
                "--repo-root", str(root), "--agents-dir", str(root / "agents"),
                "--env-file", str(config), "--log-dir", str(root / "logs"),
            ]
            env = {
                **os.environ, "PATH": f"{root}{os.pathsep}{os.defpath}",
                "LAUNCHCTL_CALLS": str(calls),
            }
            for backend in ("sqs", "kafka"):
                with self.subTest(backend=backend):
                    config.write_text(f"UPSTREAM_SYNC_BACKEND={backend}\n")
                    result = subprocess.run(
                        install, env=env, capture_output=True, text=True, timeout=15, check=False,
                    )
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("UPSTREAM_SYNC_BACKEND must be sqlite", result.stderr)
                    self.assertFalse(calls.exists())

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


@unittest.skipUnless(
    sys.platform == "darwin" and os.environ.get("HERMES_TEST_LAUNCHD_LIFECYCLE") == "1",
    "native launchd fixture requires an explicitly enabled macOS login session",
)
class NativeWebhookLaunchdTests(unittest.TestCase):
    def test_installer_starts_restarts_and_stops_both_agents(self):
        with tempfile.TemporaryDirectory(prefix="hermes-native-launchd-") as scratch:
            root = Path(scratch).resolve()
            repo = root / "repo with 'quotes' & spaces"
            wrappers = repo / "scripts"
            wrappers.mkdir(parents=True)
            for role in ("listener", "worker"):
                (wrappers / f"run-upstream-webhook-{role}.sh").write_text(
                    '#!/bin/bash\n'
                    'set -eu\n'
                    'source "$UPSTREAM_SYNC_ENV_FILE"\n'
                    'printf "%s\\n" "$$" "$PWD" "$UPSTREAM_SYNC_SUMMARY_LOCAL_PATH" > "$0.ready.$$"\n'
                    'mv -f "$0.ready.$$" "$0.ready"\n'
                    'trap "exit 0" TERM INT\n'
                    'while :; do sleep 1; done\n',
                    encoding="utf-8",
                )
            (wrappers / "upstream_webhook_sync.py").touch()
            label = f"test.hermes.webhook.{uuid.uuid4().hex}"
            domain = f"gui/{os.getuid()}"
            targets = {
                role: f"{domain}/{label}.upstream_webhook_{role}"
                for role in ("listener", "worker")
            }
            env = {
                "HOME": str(root),
                "PATH": os.defpath,
                "UPSTREAM_SYNC_AUTO_GENERATE_SECRET": "0",
            }

            def command(*args, check=True):
                result = subprocess.run(
                    args, env=env, capture_output=True, text=True, timeout=20,
                )
                if check:
                    self.assertEqual(result.returncode, 0, result.stderr)
                return result

            def wait_for_marker(role, prior_pid):
                marker = wrappers / f"run-upstream-webhook-{role}.sh.ready"
                deadline = time.monotonic() + 25
                while time.monotonic() < deadline:
                    if marker.exists():
                        pid, cwd, summary = marker.read_text(encoding="utf-8").splitlines()
                        status = command("/bin/launchctl", "print", targets[role], check=False)
                        current = status.stdout.splitlines()
                        if pid != prior_pid and f"pid = {pid}" in map(str.strip, current):
                            self.assertEqual(cwd, str(repo))
                            self.assertEqual(
                                summary, str(repo / ".sync-reports/upstream-sync-summary-fallback.log")
                            )
                            return pid
                    time.sleep(0.2)
                self.fail(f"{role} did not start a new process before the deadline")

            def wait_for_stop(target, pid):
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    status = command("/bin/launchctl", "print", target, check=False)
                    if status.returncode != 0:
                        try:
                            os.kill(int(pid), 0)
                        except ProcessLookupError:
                            return
                    time.sleep(0.2)
                self.fail(f"{target} was still registered or running after bootout")

            command("/bin/launchctl", "print", domain)
            install = [
                "/bin/bash", str(SCRIPTS / "install-upstream-webhook-launchd.sh"),
                "--repo-root", str(repo), "--label-prefix", label,
                "--agents-dir", str(root / "launch agents"),
                "--env-file", str(root / "webhook & settings"),
                "--log-dir", str(root / "webhook & logs"),
            ]
            try:
                command(*install)
                initial = {role: wait_for_marker(role, None) for role in targets}
                command(*install)
                restarted = {role: wait_for_marker(role, initial[role]) for role in targets}
                for role, target in targets.items():
                    command("/bin/launchctl", "bootout", target)
                    wait_for_stop(target, restarted[role])
            finally:
                for target in targets.values():
                    command("/bin/launchctl", "bootout", target, check=False)

"""Skill setup scripts install pinned, hash-checked packages.

Each test runs a setup script with a PATH that holds only the system tools it
needs and stand-ins for the installers (``pip``, ``pipx``, ``uvx``, ``npm``,
``curl``), which record how they were called. The lock files beside the
scripts are checked too: every package pinned to one version with a hash.

Runs with the standard library alone on Linux or macOS: ``python3 -m unittest
tests/skills/test_pinned_skill_installs.py``.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
COMFY_SCRIPTS = ROOT / "skills/creative/comfyui/scripts"
COMFY_SETUP = COMFY_SCRIPTS / "comfyui_setup.sh"
COMFY_REQUIREMENTS = COMFY_SCRIPTS / "comfy-cli-requirements.txt"
HYPERFRAMES_SCRIPTS = ROOT / "optional-skills/creative/hyperframes/scripts"
GH_ENV = ROOT / "skills/github/github-auth/scripts/gh-env.sh"

SYSTEM_TOOLS = (
    "awk", "basename", "bash", "cat", "chmod", "cp", "cut", "dirname", "env", "grep",
    "head", "ln", "mkdir", "python3", "rm", "sed", "sort", "tail", "tee", "tr",
    "uname",
)

# A recording stand-in: appends its arguments, one per line, then a blank line.
RECORDER = """#!/usr/bin/env bash
printf '%s\\n' "$@" >> "$RECORD_DIR/{name}.calls"
printf '\\n' >> "$RECORD_DIR/{name}.calls"
"""


def _comfy_version() -> str:
    match = re.search(r'^COMFY_CLI_VERSION="([^"]+)"$', COMFY_SETUP.read_text(), re.M)
    assert match, "COMFY_CLI_VERSION not found in comfyui_setup.sh"
    return match.group(1)


@unittest.skipIf(os.name == "nt", "the setup scripts run under bash on Linux and macOS")
class Sandbox(unittest.TestCase):
    """A temporary HOME and a PATH of system tools plus recording stand-ins."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.home = self.tmp / "home"
        self.bin = self.tmp / "bin"
        self.records = self.tmp / "records"
        for folder in (self.home, self.bin, self.records):
            folder.mkdir()
        for tool in SYSTEM_TOOLS:
            found = shutil.which(tool)
            if found is None:
                self.skipTest(f"{tool} is not installed")
            (self.bin / tool).symlink_to(found)

    def stub(self, name: str, extra: str = "") -> None:
        path = self.bin / name
        path.write_text(RECORDER.format(name=name) + extra)
        path.chmod(0o755)

    def run_script(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
        full_env = {
            "HOME": str(self.home),
            "PATH": str(self.bin),
            "RECORD_DIR": str(self.records),
            "TMPDIR": str(self.tmp),
            **(env or {}),
        }
        return subprocess.run(
            ["bash", *args], env=full_env, capture_output=True, text=True, timeout=60,
        )

    def calls(self, name: str) -> list[list[str]]:
        path = self.records / f"{name}.calls"
        if not path.exists():
            return []
        return [block.split("\n") for block in path.read_text().split("\n\n") if block]


class ComfyCliInstallTests(Sandbox):
    # The installer stand-ins leave a `comfy` behind, as the real ones would.
    MAKE_COMFY = """mkdir -p "$HOME/.local/bin"
printf '#!/usr/bin/env bash\\nexit 0\\n' > "$HOME/.local/bin/comfy"
chmod 755 "$HOME/.local/bin/comfy"
"""

    def run_setup(self) -> subprocess.CompletedProcess:
        result = self.run_script(str(COMFY_SETUP), "--cpu", "--skip-launch")
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_pip_fallback_installs_from_the_hashed_requirements(self) -> None:
        self.stub("pip", self.MAKE_COMFY)
        self.run_setup()
        self.assertEqual(
            self.calls("pip"),
            [["install", "--user", "--require-hashes", "-r", str(COMFY_REQUIREMENTS)]],
        )

    def test_pip_retry_for_managed_python_keeps_the_hashes(self) -> None:
        # The first call fails, as PEP 668 makes it on a managed Python.
        self.stub("pip", '[ "$3" = --break-system-packages ] || exit 1\n' + self.MAKE_COMFY)
        self.run_setup()
        self.assertEqual(self.calls("pip")[1], [
            "install", "--user", "--break-system-packages", "--require-hashes", "-r",
            str(COMFY_REQUIREMENTS),
        ])

    def test_pipx_installs_the_pinned_version(self) -> None:
        self.stub("pipx", self.MAKE_COMFY)
        self.run_setup()
        self.assertEqual(self.calls("pipx"), [["install", f"comfy-cli=={_comfy_version()}"]])

    def test_uvx_runs_the_pinned_version(self) -> None:
        self.stub("uvx")
        self.run_setup()
        uvx_calls = self.calls("uvx")
        self.assertTrue(uvx_calls)
        for call in uvx_calls:
            self.assertEqual(call[:3], ["--from", f"comfy-cli=={_comfy_version()}", "comfy"])

    def test_server_stats_are_read_before_python_sees_them(self) -> None:
        stats = json.dumps({"system": {"os": "posix"}})
        self.stub("comfy")
        self.stub("curl", f"printf '%s' '{stats}'\n")
        result = self.run_script(str(COMFY_SETUP), "--cpu")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"os": "posix"', result.stdout)


class ComfyCliRequirementsTests(unittest.TestCase):
    def requirements(self) -> list[str]:
        text = COMFY_REQUIREMENTS.read_text().replace("\\\n", " ")
        return [line.strip() for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")]

    def test_every_requirement_has_one_version_and_a_hash(self) -> None:
        entries = self.requirements()
        self.assertTrue(entries)
        for entry in entries:
            self.assertRegex(entry, r"^[A-Za-z0-9_.\-]+==[^ ;]+ ", entry)
            self.assertRegex(entry, r"--hash=sha256:[0-9a-f]{64}", entry)

    def test_requirements_pin_the_version_the_script_names(self) -> None:
        names = [entry.split(" ", 1)[0] for entry in self.requirements()]
        self.assertIn(f"comfy-cli=={_comfy_version()}", names)


class HyperframesInstallTests(Sandbox):
    def setUp(self) -> None:
        super().setUp()
        self.prefix = self.tmp / "prefix"
        self.global_prefix = self.tmp / "global"
        version = json.loads((HYPERFRAMES_SCRIPTS / "package.json").read_text())["dependencies"]["hyperframes"]
        self.version = version
        node = self.bin / "node"
        node.write_text(
            "#!/usr/bin/env python3\n"
            "import json, sys\n"
            "if sys.argv[1] == '--version':\n"
            "    print('v22.11.0')\n"
            "else:\n"
            "    print(json.load(open(sys.argv[3]))['dependencies']['hyperframes'])\n"
        )
        node.chmod(0o755)
        self.stub("ffmpeg", "echo 'ffmpeg version 7.0'\n")
        self.stub("npx")
        # `npm ci` leaves a CLI that reports the pinned version.
        self.stub("npm", f"""case "$1" in
  ci)
    mkdir -p "$3/node_modules/.bin"
    printf '#!/usr/bin/env bash\\n[ "$1" = --version ] && echo {version}\\nexit 0\\n' > "$3/node_modules/.bin/hyperframes"
    chmod 755 "$3/node_modules/.bin/hyperframes" ;;
  prefix) echo "{self.global_prefix}" ;;
esac
""")

    def test_cli_is_installed_with_npm_ci_and_linked_globally(self) -> None:
        result = self.run_script(
            str(HYPERFRAMES_SCRIPTS / "setup.sh"), env={"HYPERFRAMES_PREFIX": str(self.prefix)},
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        npm_calls = self.calls("npm")
        self.assertIn(["ci", "--prefix", str(self.prefix), "--no-audit", "--no-fund"], npm_calls)
        self.assertFalse([call for call in npm_calls if call[0] in ("install", "i", "update")])
        for name in ("package.json", "package-lock.json"):
            self.assertEqual(
                (self.prefix / name).read_bytes(), (HYPERFRAMES_SCRIPTS / name).read_bytes(),
            )
        link = self.global_prefix / "bin/hyperframes"
        self.assertEqual(Path(os.readlink(link)), self.prefix / "node_modules/.bin/hyperframes")

    def test_lockfile_pins_every_package_with_an_integrity_hash(self) -> None:
        package = json.loads((HYPERFRAMES_SCRIPTS / "package.json").read_text())
        lock = json.loads((HYPERFRAMES_SCRIPTS / "package-lock.json").read_text())
        self.assertRegex(self.version, r"^\d+\.\d+\.\d+$")
        self.assertEqual(lock["packages"][""]["dependencies"], package["dependencies"])
        self.assertEqual(lock["packages"]["node_modules/hyperframes"]["version"], self.version)
        for path, entry in lock["packages"].items():
            if not path:
                continue
            self.assertTrue(entry["resolved"].startswith("https://registry.npmjs.org/"), path)
            self.assertTrue(entry["integrity"].startswith("sha512-"), path)


class GhEnvTests(Sandbox):
    def test_user_is_read_from_the_api_reply(self) -> None:
        self.stub("curl", """printf '{"login": "octocat"}'\n""")
        result = self.run_script(
            "-c", 'source "$1" >/dev/null; printf "USER=%s\\n" "$GH_USER"', "_", str(GH_ENV),
            env={"GITHUB_TOKEN": "test-token"},
        )
        self.assertIn("USER=octocat", result.stdout)
        self.assertEqual(self.calls("curl")[0][-1], "https://api.github.com/user")


class NoDownloadPipedIntoAnInterpreterTests(unittest.TestCase):
    """What a download returns is never piped straight into a shell or interpreter."""

    PIPE = re.compile(
        r"\b(curl|wget)\b[^|;&]*\|\s*(sudo\s+)?(sh|bash|zsh|python[0-9.]*|node|perl|ruby)\b"
    )

    def test_shipped_shell_scripts(self) -> None:
        offenders = []
        for top in ("skills", "optional-skills", "plugins"):
            for script in sorted((ROOT / top).rglob("*.sh")):
                # Join continued lines so a pipe on the next line is seen.
                text = script.read_text(encoding="utf-8", errors="replace").replace("\\\n", " ")
                for line in text.splitlines():
                    if not line.lstrip().startswith("#") and self.PIPE.search(line):
                        offenders.append(f"{script.relative_to(ROOT)}: {line.strip()}")
        self.assertEqual(offenders, [])


if __name__ == "__main__":
    unittest.main()

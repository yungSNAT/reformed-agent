"""Skill scripts never start a program from the working folder.

Skill scripts run inside whatever folder the agent is working in, which can be
an untrusted checkout. A relative PATH entry (an empty one included) points
into that folder, and on Windows ``shutil.which`` and a bare-name
``subprocess`` call search it first. Each test plants a program in the working
folder and puts a trusted one in an absolute PATH entry that comes after the
relative entries: the trusted one must be found and the planted one never run.

Runs with the standard library alone: ``python3 -m unittest
tests/skills/test_skill_program_lookup.py``.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
GOOGLE = ROOT / "skills/productivity/google-workspace/scripts"
COMFYUI = ROOT / "skills/creative/comfyui/scripts"
PIXEL_ART = ROOT / "skills/creative/pixel-art/scripts"
KANBAN = ROOT / "optional-skills/creative/kanban-video-orchestrator/scripts"
EXCEL = ROOT / "optional-skills/finance/excel-author/scripts"
SCRIPT_DIRS = (GOOGLE, COMFYUI, PIXEL_ART, KANBAN, EXCEL)

TRUSTED = {"ran": "trusted"}


def _write_program(folder: Path, name: str, label: str, log: Path) -> None:
    """Write a program that records `label` in `log` and prints it as JSON."""
    if os.name == "nt":
        path = folder / f"{name}.BAT"
        path.write_text(f'@echo {label}>>"{log}"\r\n@echo {{"ran": "{label}"}}\r\n')
    else:
        path = folder / name
        path.write_text(f'#!/bin/sh\necho {label} >> "{log}"\necho \'{{"ran": "{label}"}}\'\n')
        path.chmod(0o755)


def _load(script: Path):
    """Import a skill script by path, its own folder first on sys.path."""
    folder = str(script.parent)
    if folder not in sys.path:
        sys.path.insert(0, folder)
    sys.modules.pop("_program_path", None)
    name = f"skill_lookup_{script.parent.parent.name}_{script.stem}".replace("-", "_")
    spec = importlib.util.spec_from_file_location(name, script)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PlantedProgramCase(unittest.TestCase):
    """A working folder with planted programs and a trusted absolute PATH entry."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        base = Path(temp.name).resolve()
        self.work = base / "checkout"
        self.trusted = base / "trusted-bin"
        self.work.mkdir()
        self.trusted.mkdir()
        self.log = base / "ran.log"
        self.home = base / "home"
        self.home.mkdir()

        previous = os.getcwd()
        os.chdir(self.work)
        self.addCleanup(os.chdir, previous)

        # Relative entries first: an empty entry and "." both mean the
        # working folder, and "checkout-bin" a folder inside it.
        # The trusted folder comes last and is the only absolute entry, so a
        # program installed on the test machine cannot satisfy a lookup.
        path = os.pathsep.join(["", ".", "checkout-bin", str(self.trusted)])
        env = mock.patch.dict(
            os.environ, {"PATH": path, "HERMES_HOME": str(self.home)}, clear=False
        )
        env.start()
        self.addCleanup(env.stop)
        os.environ.pop("HERMES_GWS_BIN", None)

    def plant(self, *names: str) -> None:
        for name in names:
            _write_program(self.work, name, "planted", self.log)

    def install(self, *names: str) -> None:
        for name in names:
            _write_program(self.trusted, name, "trusted", self.log)

    def trusted_path(self, name: str) -> str:
        suffix = ".BAT" if os.name == "nt" else ""
        return str(self.trusted / f"{name}{suffix}")

    def ran(self) -> list[str]:
        return self.log.read_text().split() if self.log.exists() else []


class FindProgramTests(PlantedProgramCase):
    def test_copies_are_identical(self):
        copies = {folder: (folder / "_program_path.py").read_bytes() for folder in SCRIPT_DIRS}
        self.assertEqual(len(set(copies.values())), 1, sorted(str(f) for f in copies))

    def test_skips_the_working_folder_and_relative_entries(self):
        module = _load(GOOGLE / "_program_path.py")
        self.plant("tool")
        (self.work / "checkout-bin").mkdir()
        _write_program(self.work / "checkout-bin", "tool", "planted", self.log)
        self.assertIsNone(module.find_program("tool"))
        self.install("tool")
        self.assertEqual(module.find_program("tool"), self.trusted_path("tool"))


class SkillScriptTests(PlantedProgramCase):
    def test_gws_bridge_runs_trusted_gws_only(self):
        bridge = _load(GOOGLE / "gws_bridge.py")
        self.plant("gws")
        with mock.patch.object(bridge, "get_valid_token", return_value="token"), \
                mock.patch.object(sys, "argv", ["gws_bridge.py", "gmail", "+triage"]):
            with self.assertRaises(SystemExit) as missing:
                bridge.main()
            self.assertNotEqual(missing.exception.code, 0)
            self.assertEqual(self.ran(), [])

            self.install("gws")
            with self.assertRaises(SystemExit) as done:
                bridge.main()
        self.assertEqual(done.exception.code, 0)
        self.assertEqual(self.ran(), ["trusted"])

    def test_google_api_finds_trusted_gws_only(self):
        api = _load(GOOGLE / "google_api.py")
        self.plant("gws")
        self.assertIsNone(api._gws_binary())
        self.install("gws")
        self.assertEqual(api._gws_binary(), self.trusted_path("gws"))

    def test_google_setup_falls_back_to_trusted_uv_only(self):
        setup = _load(GOOGLE / "setup.py")
        calls = []

        def check_call(cmd, **_kwargs):
            calls.append(cmd)
            if cmd[0] == sys.executable:
                raise subprocess.CalledProcessError(1, cmd)
            return 0

        missing = dict.fromkeys(("googleapiclient", "google_auth_oauthlib", "google_auth_httplib2"))
        self.plant("uv")
        with mock.patch.dict(sys.modules, missing), \
                mock.patch.object(setup.subprocess, "check_call", side_effect=check_call):
            self.assertFalse(setup.install_deps())
            self.assertEqual([c[0] for c in calls], [sys.executable])
            self.install("uv")
            self.assertTrue(setup.install_deps())
        self.assertEqual(calls[-1][0], self.trusted_path("uv"))

    def test_hardware_check_runs_trusted_probes_only(self):
        hardware = _load(COMFYUI / "hardware_check.py")
        self.plant("nvidia-smi", "wmic")
        self.assertIsNone(hardware.detect_nvidia())
        self.assertEqual(hardware._run(["wmic", "ComputerSystem"]), "")
        self.assertEqual(self.ran(), [])
        self.install("wmic")
        self.assertEqual(json.loads(hardware._run(["wmic", "ComputerSystem"])), TRUSTED)
        self.assertEqual(self.ran(), ["trusted"])

    def test_comfyui_auto_fix_uses_trusted_cli_only(self):
        auto_fix = _load(COMFYUI / "auto_fix_deps.py")
        self.plant("comfy", "uvx")
        self.assertIsNone(auto_fix.comfy_cli_available())
        self.install("uvx")
        uvx = auto_fix.comfy_cli_available()
        self.assertEqual(uvx[0], self.trusted_path("uvx"))
        self.install("comfy")
        comfy = auto_fix.comfy_cli_available()
        self.assertEqual(comfy, [self.trusted_path("comfy")])
        self.assertTrue(auto_fix.install_node("example-node", comfy_cmd=comfy))
        self.assertEqual(self.ran(), ["trusted"])

    def test_comfyui_health_check_reports_trusted_cli_only(self):
        health = _load(COMFYUI / "health_check.py")
        self.plant("comfy", "uvx")
        self.assertFalse(health.comfy_cli_status()["available"])
        self.install("comfy")
        self.assertEqual(health.comfy_cli_status()["path"], self.trusted_path("comfy"))

    def test_pixel_art_video_finds_trusted_ffmpeg_only(self):
        stub = mock.MagicMock()
        with mock.patch.dict(sys.modules, {"PIL": stub, "PIL.Image": stub, "PIL.ImageDraw": stub}):
            video = _load(PIXEL_ART / "pixel_art_video.py")
        self.plant("ffmpeg")
        with self.assertRaises(RuntimeError):
            video._ensure_ffmpeg()
        self.install("ffmpeg")
        self.assertEqual(video._ensure_ffmpeg(), self.trusted_path("ffmpeg"))

    @unittest.skipIf(importlib.util.find_spec("PIL") is None, "needs Pillow")
    def test_pixel_art_video_runs_trusted_ffmpeg_only(self):
        from PIL import Image

        video = _load(PIXEL_ART / "pixel_art_video.py")
        base = self.work / "base.png"
        Image.new("RGB", (64, 64)).save(base)
        self.plant("ffmpeg")
        self.install("ffmpeg")
        video.pixel_art_video(str(base), str(self.work / "out.mp4"), duration=1, fps=1, export_gif=True)
        self.assertEqual(self.ran(), ["trusted", "trusted"])

    def test_kanban_monitor_runs_trusted_hermes_only(self):
        monitor = _load(KANBAN / "monitor.py")
        self.plant("hermes")
        self.assertFalse(monitor.hermes_available())
        with self.assertRaises(FileNotFoundError):
            monitor.kanban_show("t_1")
        self.assertEqual(self.ran(), [])
        self.install("hermes")
        self.assertTrue(monitor.hermes_available())
        self.assertEqual(monitor.kanban_show("t_1"), TRUSTED)
        self.assertEqual(self.ran(), ["trusted"])

    def test_excel_recalc_finds_trusted_libreoffice_only(self):
        recalc = _load(EXCEL / "recalc.py")
        self.plant("libreoffice", "soffice")
        self.assertIsNone(recalc.find_libreoffice())
        self.install("soffice")
        self.assertEqual(recalc.find_libreoffice(), self.trusted_path("soffice"))


if __name__ == "__main__":
    unittest.main()

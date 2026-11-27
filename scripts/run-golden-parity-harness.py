#!/usr/bin/env python3
"""Golden parity harness compatibility shim (delegates to hermes-parity-tools)."""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]

REQUIRED_TUI_TESTS = [
    "test_completion_popup_hidden_when_slash_deleted",
    "test_completion_popup_hidden_when_modal_or_processing_active",
    "test_transcript_hides_system_messages",
    "test_transcript_wrap_width_caps_at_80",
    "test_find_anchor_line_index_prefers_near_expected_window",
    "test_submit_shortcuts_are_detected",
    "test_stream_handle",
    "test_is_ctrl_c_detection",
]


def _read_rust_surface(path: pathlib.Path) -> str:
    if path.is_dir():
        return "\n".join(
            p.read_text(encoding="utf-8") for p in sorted(path.rglob("*.rs")) if p.is_file()
        )
    return path.read_text(encoding="utf-8")


def parse_tui_tests(tui_rs: pathlib.Path) -> set[str]:
    raw = _read_rust_surface(tui_rs)
    tui_dir = tui_rs.with_suffix("")
    if tui_dir.is_dir():
        raw += "\n" + _read_rust_surface(tui_dir)
    return set(re.findall(r"fn\s+([a-zA-Z0-9_]+)\s*\(", raw))


def main() -> int:
    proc = subprocess.run(
        [
            "cargo",
            "run",
            "--locked",
            "--manifest-path",
            str(REPO_ROOT / "Cargo.toml"),
            "-q",
            "-p",
            "hermes-parity-tools",
            "--",
            "run-golden-parity-harness",
            *sys.argv[1:],
        ],
        check=False,
    )
    return int(proc.returncode)


if __name__ == "__main__":
    raise SystemExit(main())

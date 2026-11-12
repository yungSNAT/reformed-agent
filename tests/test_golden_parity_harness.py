from __future__ import annotations

import importlib.util
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def _load_harness_module():
    script_path = REPO_ROOT / "scripts" / "run-golden-parity-harness.py"
    spec = importlib.util.spec_from_file_location("golden_parity_harness", script_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def test_required_tui_tests_are_found_in_the_tui_surface() -> None:
    # The tests live in tui/tests.rs, which tui.rs includes.
    harness = _load_harness_module()
    found = harness.parse_tui_tests(REPO_ROOT / "crates" / "hermes-cli" / "src" / "tui.rs")
    assert set(harness.REQUIRED_TUI_TESTS) <= found

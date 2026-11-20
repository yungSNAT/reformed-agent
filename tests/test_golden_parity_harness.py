from __future__ import annotations

import runpy
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def test_required_tui_tests_are_found_in_the_tui_surface() -> None:
    harness = runpy.run_path(str(REPO_ROOT / "scripts" / "run-golden-parity-harness.py"))
    found = harness["parse_tui_tests"](REPO_ROOT / "crates" / "hermes-cli" / "src" / "tui.rs")
    assert set(harness["REQUIRED_TUI_TESTS"]) <= found


def test_tui_test_declarations_can_live_in_a_sidecar(tmp_path: Path) -> None:
    harness = runpy.run_path(str(REPO_ROOT / "scripts" / "run-golden-parity-harness.py"))
    facade = tmp_path / "tui.rs"
    facade.write_text('include!("tui/tests.rs");\n', encoding="utf-8")
    tui_tests = tmp_path / "tui" / "tests.rs"
    tui_tests.parent.mkdir()
    tui_tests.write_text("#[test]\nfn split_example() {}\n", encoding="utf-8")
    assert harness["parse_tui_tests"](facade) == {"split_example"}

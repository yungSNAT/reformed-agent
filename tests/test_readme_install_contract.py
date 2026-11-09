"""README contracts for install and Rust-primary positioning."""

from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
README = REPO_ROOT / "README.md"


def test_readme_installs_from_this_fork() -> None:
    text = README.read_text()

    assert "cargo install --git https://github.com/yungSNAT/reformed-agent" in text
    assert "git clone https://github.com/yungSNAT/reformed-agent.git" in text
    assert "bash scripts/install.sh" in text
    assert "--bin hermes-agent-ultra --bin hermes-ultra" in text
    assert "--bin hermes\n" not in text
    # Ultra's installer and source URL would install Ultra, not this fork.
    assert "sheawinkler/hermes-agent-ultra/main/scripts/install.sh" not in text
    assert "cargo install --git https://github.com/sheawinkler" not in text


def test_readme_documents_rust_primary_runtime() -> None:
    text = README.read_text()

    assert "Rust-first autonomous agent runtime" in text
    assert "Fully Rust-native core runtime" in text
    assert "Rust-only implementation strategy" in text


def test_readme_quick_start_uses_ultra_commands() -> None:
    text = README.read_text()

    for command in (
        "hermes-ultra setup",
        "hermes-ultra",
        'hermes-ultra chat --query "summarize this repository"',
        "hermes-ultra gateway --live",
        "hermes-ultra doctor --deep --snapshot --bundle",
    ):
        assert command in text

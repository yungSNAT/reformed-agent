"""Contracts for install-facing documentation: it installs this fork, under Ultra's command names."""

from __future__ import annotations

import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
DOC_ROOTS = [
    REPO_ROOT / "website" / "docs",
    REPO_ROOT / "website" / "scripts",
    REPO_ROOT / "skills",
    REPO_ROOT / "plugins" / "hermes-achievements" / "README.md",
]
PYTHON_LIBRARY_GUIDE = REPO_ROOT / "website" / "docs" / "guides" / "python-library.md"

# Installers and clones of the projects this fork descends from: they install
# a different program under the same command names.
UPSTREAM_INSTALL_PATTERNS = (
    "raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.sh",
    "raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.ps1",
    "raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/",
    "github.com/sheawinkler/hermes-agent-ultra hermes-cli",
    "github.com/sheawinkler/hermes-agent-ultra.git",
)
# This fork has published no release, so its installer works only from a
# checkout, where it builds from source; piped through curl it fails.
FORK_CARGO_INSTALL = "cargo install --git https://github.com/yungSNAT/reformed-agent hermes-cli"
FORK_CLONE = "git clone https://github.com/yungSNAT/reformed-agent.git"
FORK_PIPED_INSTALL_RE = re.compile(
    r"raw\.githubusercontent\.com/yungSNAT/reformed-agent/\S*install\.sh[^\n]*\|\s*(?:sudo\s+)?(?:ba)?sh\b"
)

PIP_APP_INSTALL_RE = re.compile(
    r"\bpip\s+install\s+['\"]?hermes-agent(?:\[[^\]]+\])?\b"
)
UV_APP_INSTALL_RE = re.compile(
    r"\buv\s+pip\s+install\b[^\n]*\bhermes-agent(?:\[[^\]]+\])?\b"
)


def iter_install_contract_files() -> list[Path]:
    files: list[Path] = []
    for root in DOC_ROOTS:
        if root.is_file():
            files.append(root)
            continue
        files.extend(
            p
            for p in root.rglob("*")
            if p.suffix in {".md", ".py"} and p != PYTHON_LIBRARY_GUIDE
        )
    return sorted(set(files))


def test_docs_do_not_point_installers_at_upstream_repo() -> None:
    offenders: list[str] = []
    for path in iter_install_contract_files():
        text = path.read_text()
        for pattern in UPSTREAM_INSTALL_PATTERNS:
            if pattern in text:
                offenders.append(f"{path.relative_to(REPO_ROOT)}: {pattern}")

    assert offenders == []


def test_docs_do_not_recommend_pip_installing_the_ultra_app() -> None:
    offenders: list[str] = []
    for path in iter_install_contract_files():
        text = path.read_text()
        if PIP_APP_INSTALL_RE.search(text) or UV_APP_INSTALL_RE.search(text):
            offenders.append(str(path.relative_to(REPO_ROOT)))

    assert offenders == []


def test_install_docs_use_ultra_commands_and_coexistence_contract() -> None:
    install = (REPO_ROOT / "website" / "docs" / "getting-started" / "installation.md").read_text()
    quickstart = (REPO_ROOT / "website" / "docs" / "getting-started" / "quickstart.md").read_text()
    llms_generator = (REPO_ROOT / "website" / "scripts" / "generate-llms-txt.py").read_text()

    assert FORK_CARGO_INSTALL in install
    assert FORK_CLONE in install
    assert "bash scripts/install.sh" in install
    assert FORK_CARGO_INSTALL in quickstart or FORK_CLONE in quickstart
    assert "hermes-ultra setup" in install
    assert "hermes-ultra model" in quickstart
    assert "INSTALL_LEGACY_ALIAS=true" in install
    assert "yungSNAT/reformed-agent" in llms_generator


def test_docs_do_not_pipe_the_fork_installer() -> None:
    offenders = [
        str(path.relative_to(REPO_ROOT))
        for path in iter_install_contract_files()
        if FORK_PIPED_INSTALL_RE.search(path.read_text())
    ]

    assert offenders == []

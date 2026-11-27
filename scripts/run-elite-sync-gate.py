#!/usr/bin/env python3
"""Run consolidated ELITE sync gates (delegates to hermes-parity-tools)."""

from __future__ import annotations

import pathlib
import subprocess
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]


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
            "run-elite-sync-gate",
            *sys.argv[1:],
        ],
        check=False,
    )
    return int(proc.returncode)


if __name__ == "__main__":
    raise SystemExit(main())

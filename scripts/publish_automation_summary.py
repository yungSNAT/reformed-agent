#!/usr/bin/env python3
"""Publish automation summaries (delegates to hermes-parity-tools)."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
from typing import Any

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
SUPPORTED_SINKS = {"contextlattice", "github", "local", "stdout"}


def parse_sink_order(raw: str) -> list[str]:
    order: list[str] = []
    for token in str(raw or "").split(","):
        sink = token.strip().lower()
        if sink and sink in SUPPORTED_SINKS and sink not in order:
            order.append(sink)
    return order or ["local"]


def build_summary_markdown(
    *,
    title: str,
    summary_kind: str,
    status: str,
    metadata: dict[str, Any],
    body: str,
) -> str:
    heading = title.strip() or f"{summary_kind.strip() or 'automation'} summary"
    lines = [f"## {heading}", ""]
    keys = ["timestamp_utc", "status", "summary_kind", "repo", "head_sha", "delivery_id"]
    for key in keys:
        value = metadata.get(key)
        if value is not None and value != "":
            lines.append(f"- {key}: `{value}`")
    extra = {k: v for k, v in metadata.items() if k not in keys and v is not None and v != ""}
    if extra:
        lines.append(f"- metadata: `{json.dumps(extra, sort_keys=True)}`")
    lines.extend(["", body.strip() if body.strip() else "_No summary body provided._", ""])
    return "\n".join(lines)


def publish_summary(
    *,
    sink_order: list[str],
    summary_markdown: str,
    repo_root: pathlib.Path,
    contextlattice_url: str,
    context_timeout_secs: float,
    context_api_key: str,
    context_project: str,
    context_file_name: str,
    context_topic_path: str,
    github_issue: int,
    local_path: pathlib.Path,
) -> dict[str, Any]:
    cmd = [
        "cargo", "run", "--locked", "--manifest-path", str(REPO_ROOT / "Cargo.toml"),
        "-q", "-p", "hermes-parity-tools", "--", "publish-automation-summary",
        "--repo-root", str(repo_root),
        "--sink-order", ",".join(sink_order),
        "--preformatted-markdown", "-",
        "--contextlattice-url", str(contextlattice_url),
        "--context-timeout-secs", str(context_timeout_secs),
        "--context-api-key", str(context_api_key),
        "--context-project", str(context_project),
        "--context-file-name", str(context_file_name),
        "--context-topic-path", str(context_topic_path),
        "--github-issue", str(github_issue),
        "--local-path", str(local_path),
        "--json",
    ]
    proc = subprocess.run(cmd, input=summary_markdown, capture_output=True, text=True, check=False)
    try:
        parsed = json.loads(proc.stdout or "{}")
    except json.JSONDecodeError:
        parsed = {}
    return {
        "ok": bool(parsed.get("ok")),
        "primary_sink": str(parsed.get("primary_sink", "")),
        "attempts": list(parsed.get("attempts", [])),
    }


def main() -> int:
    proc = subprocess.run(
        [
            "cargo", "run", "--locked", "--manifest-path", str(REPO_ROOT / "Cargo.toml"),
            "-q", "-p", "hermes-parity-tools", "--", "publish-automation-summary",
            *sys.argv[1:],
        ],
        check=False,
    )
    return int(proc.returncode)


if __name__ == "__main__":
    raise SystemExit(main())

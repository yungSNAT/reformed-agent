#!/usr/bin/env python3
"""Publish automation summaries (delegates to hermes-parity-tools)."""

from __future__ import annotations

import json
import os
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
    # Each sink runs on its own, in the order given, as the script's loop did:
    # unsupported names are recorded, repeats are retried and an empty order
    # tries nothing. `--opt=value` keeps values that start with "-" intact.
    base_cmd = [
        "cargo", "run", "--locked", "--manifest-path", str(REPO_ROOT / "Cargo.toml"),
        "-q", "-p", "hermes-parity-tools", "--", "publish-automation-summary",
        f"--repo-root={repo_root}",
        "--preformatted-markdown=-",
        f"--contextlattice-url={contextlattice_url}",
        f"--context-timeout-secs={context_timeout_secs}",
        f"--context-project={context_project}",
        f"--context-file-name={context_file_name}",
        f"--context-topic-path={context_topic_path}",
        f"--github-issue={github_issue}",
        f"--local-path={local_path}",
        "--json",
    ]
    env = dict(os.environ)
    env["AUTOMATION_SUMMARY_CONTEXT_API_KEY"] = str(context_api_key)
    if not str(context_api_key):
        env.pop("CONTEXTLATTICE_API_KEY", None)
    attempts: list[dict[str, str]] = []
    for sink in sink_order:
        if sink not in SUPPORTED_SINKS:
            attempts.append({"sink": sink, "ok": "false", "detail": "unsupported_sink"})
            continue
        proc = subprocess.run(
            [*base_cmd, f"--sink-order={sink}"],
            input=summary_markdown, capture_output=True, text=True, env=env, check=False,
        )
        try:
            parsed = json.loads(proc.stdout or "{}")
        except json.JSONDecodeError as exc:
            err_tail = (proc.stderr or "").strip()[-400:]
            detail = f"exit={proc.returncode} json_err={exc}" + (f" stderr={err_tail}" if err_tail else "")
            attempts.append({"sink": "cli", "ok": "false", "detail": detail})
            return {"ok": False, "primary_sink": "", "attempts": attempts}
        returned = parsed.get("attempts") or []
        if not returned:
            detail = (proc.stderr or "").strip()[-400:] or f"exit={proc.returncode}"
            attempts.append({"sink": "cli", "ok": "false", "detail": detail})
            return {"ok": False, "primary_sink": "", "attempts": attempts}
        # The CLI prints sorted keys; keep the helper's sink, ok, detail order.
        attempts.extend(
            {"sink": a.get("sink", ""), "ok": a.get("ok", ""), "detail": a.get("detail", "")}
            for a in returned
        )
        if parsed.get("ok"):
            if sink == "stdout":
                print(summary_markdown)
            return {"ok": True, "primary_sink": sink, "attempts": attempts}
    return {"ok": False, "primary_sink": "", "attempts": attempts}


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

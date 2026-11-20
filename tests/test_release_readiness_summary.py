import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "generate-release-readiness-summary.py"
PARITY_AUDIT = REPO_ROOT / ".github" / "workflows" / "parity-audit.yml"
RUN_REPO_CI = REPO_ROOT / "scripts" / "run-repo-ci.sh"


@pytest.mark.parametrize("pending", [0, 1])
def test_release_readiness_summary_reports_backlog(tmp_path: Path, pending: int) -> None:
    parity = tmp_path / "docs" / "parity"
    parity.mkdir(parents=True)
    for name, payload in {
        "global-parity-proof": {"release_gate": {"pass": True}, "ci_gate": {"pass": True}},
        "upstream-missing-queue": {"summary": {"by_disposition": {"pending": pending}}},
        "shared-diff-backlog": {"summary": {"pending_classification": 0, "pending_review": 0}},
        "test-coverage-audit": {"audit_gate": {"critical_gaps": 0}, "summary": {"missing_rust_test_refs": 0}},
        "sota-harness-matrix": {"gate": {"critical_gaps": 0, "missing_rust_test_refs": 0}},
        "global-parity-thresholds": {},
    }.items():
        (parity / f"{name}.json").write_text(json.dumps(payload), encoding="utf-8")
    out_json = tmp_path / "summary.json"
    out_md = tmp_path / "summary.md"
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--repo-root",
            str(tmp_path),
            "--output-json",
            str(out_json),
            "--output-md",
            str(out_md),
            "--check",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == int(pending > 0), result.stdout + result.stderr
    payload = json.loads(out_json.read_text())
    assert payload["ok"] is (pending == 0)
    assert payload["summary"]["queue_pending"] == pending
    assert payload["summary"]["shared_pending_classification"] == 0
    assert payload["summary"]["shared_pending_review"] == 0
    assert payload["summary"]["coverage_critical_gaps"] == 0
    assert payload["summary"]["sota_critical_gaps"] == 0
    assert payload["summary"]["upstream_delta_watch_pass"] is (pending == 0)
    assert payload["upstream_delta_watch"]["queue_pending"] == pending
    status = "PASS" if pending == 0 else "FAIL"
    assert payload["upstream_delta_watch"]["status"] == status
    text = out_md.read_text()
    assert f"Status: **{status}**" in text
    assert "Upstream queue pending" in text
    assert "Upstream Delta Watch" in text


def test_release_readiness_summary_uses_public_artifacts_only() -> None:
    text = SCRIPT.read_text()
    assert "docs/parity/global-parity-proof.json" in text
    assert "docs/parity/upstream-missing-queue.json" in text
    assert "docs/parity/shared-diff-backlog.json" in text
    assert "docs/parity/test-coverage-audit.json" in text
    assert "docs/parity/sota-harness-matrix.json" in text
    assert "docs/parity/global-parity-thresholds.json" in text
    assert "contextlattice" not in text.lower()


def test_repo_ci_runs_readiness_summary_after_global_parity_proof() -> None:
    text = RUN_REPO_CI.read_text()
    assert "python3 scripts/generate-global-parity-proof.py" in text
    assert "python3 scripts/generate-release-readiness-summary.py --check" in text
    assert text.index("python3 scripts/generate-global-parity-proof.py") < text.index(
        "python3 scripts/generate-release-readiness-summary.py --check"
    )


def test_parity_audit_reports_live_drift_without_gating_on_it() -> None:
    # The scheduled audit measures upstream's live main; PR CI gates on the pin.
    text = PARITY_AUDIT.read_text()
    assert "python3 scripts/generate-global-parity-proof.py\n" in text
    assert "python3 scripts/generate-release-readiness-summary.py --repo-root .\n" in text
    assert text.index("python3 scripts/generate-global-parity-proof.py") < text.index(
        "python3 scripts/generate-release-readiness-summary.py"
    )
    assert 'cat .sync-reports/release-readiness-summary.md >> "$GITHUB_STEP_SUMMARY"' in text
    assert "--check-ci" not in text
    assert "gh issue" not in text
    # Local proof metrics still gate; scheduled upstream drift is reported.
    assert 'select(.status == "fail" or .status == "missing")] == []' in text
    selection = re.search(r"\.metric \| IN\(([^)]+)\) \| not", text)
    assert selection
    assert set(re.findall(r'"([^"]+)"', selection[1])) == {
        "max_commits_behind",
        "max_upstream_patch_missing",
        "max_files_only_upstream",
        "max_queue_pending_commits",
        "min_test_coverage_tracked_behavior_ratio",
    }
    assert "persist-credentials: false" in text


def test_parity_audit_runs_golden_harness_with_upstream_dependencies() -> None:
    # The harness imports upstream's hermes_cli.commands, so it needs upstream's core dependencies.
    text = PARITY_AUDIT.read_text()
    install = "uv sync --frozen --no-dev --no-install-project --no-build --project ../hermes-agent"
    harness = "../hermes-agent/.venv/bin/python scripts/run-golden-parity-harness.py"
    assert install in text
    assert harness in text
    assert text.index(install) < text.index(harness)

"""A workflow file that is not valid JSON gets the scripts' invalid-JSON message."""

from __future__ import annotations

import check_deps
import extract_schema
import run_workflow


def _broken(tmp_path):
    path = tmp_path / "workflow.json"
    path.write_text("{not json")
    return path


def test_check_deps_reports_invalid_json(tmp_path, capsys):
    assert check_deps.main([str(_broken(tmp_path))]) == 1
    assert "Invalid JSON: " in capsys.readouterr().out


def test_extract_schema_reports_invalid_json(tmp_path, capsys):
    assert extract_schema.main([str(_broken(tmp_path))]) == 1
    assert "invalid JSON" in capsys.readouterr().err


def test_run_workflow_reports_invalid_json(tmp_path, capsys):
    assert run_workflow.main(["--workflow", str(_broken(tmp_path))]) == 1
    assert "Invalid JSON in workflow file: " in capsys.readouterr().out

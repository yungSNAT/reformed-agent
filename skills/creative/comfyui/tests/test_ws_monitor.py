"""Tests for ws_monitor.py's connection logging."""

from __future__ import annotations

import sys
import types

import pytest

import ws_monitor


class _Stop(Exception):
    pass


def test_connect_log_leaves_out_the_cloud_api_key(monkeypatch, capsys):
    seen = {}

    def create_connection(url, timeout):
        seen["url"] = url
        raise _Stop

    monkeypatch.setitem(
        sys.modules, "websocket", types.SimpleNamespace(create_connection=create_connection)
    )
    key = "test-key-not-a-real-secret-123"

    with pytest.raises(_Stop):
        ws_monitor.main(["--host", "https://cloud.comfy.org", "--api-key", key, "--client-id", "c1"])

    assert f"token={key}" in seen["url"]
    err = capsys.readouterr().err
    assert "Connecting to wss://cloud.comfy.org/ws" in err
    assert key not in err

from __future__ import annotations

import importlib.util
import json
from argparse import Namespace
from pathlib import Path


def test_token_cli_accepts_null_largest_accounts(monkeypatch, capsys):
    script = (
        Path(__file__).resolve().parents[2]
        / "optional-skills/blockchain/solana/scripts/solana_client.py"
    )
    spec = importlib.util.spec_from_file_location("solana_client", script)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    replies = {
        "getAccountInfo": {
            "value": {"data": {"parsed": {"info": {"supply": "123", "decimals": 0}}}}
        },
        "getTokenLargestAccounts": None,
    }
    monkeypatch.setattr(module, "rpc", lambda method, params: replies[method])
    monkeypatch.setattr(module, "resolve_token_name", lambda mint: {})
    monkeypatch.setattr(module, "fetch_prices", lambda mints: {})

    module.cmd_token(Namespace(mint="fixture-mint"))

    payload = json.loads(capsys.readouterr().out)
    assert payload["mint"] == "fixture-mint"
    assert payload["supply"] == 123
    assert payload["top_5_holders"] == []

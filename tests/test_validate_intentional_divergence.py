import importlib.util
from pathlib import Path


def load_module():
    repo_root = Path(__file__).resolve().parents[1]
    script = repo_root / "scripts" / "validate-intentional-divergence.py"
    spec = importlib.util.spec_from_file_location("validate_intentional_divergence", script)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def test_registry_path_is_reported_relative_to_the_repository(tmp_path):
    mod = load_module()
    registry = tmp_path / "docs" / "parity" / "intentional-divergence.json"

    # The committed report must not carry the location of someone's checkout.
    assert mod.repo_relative(registry, tmp_path) == "docs/parity/intentional-divergence.json"


def test_registry_path_outside_the_repository_is_reported_in_full(tmp_path):
    mod = load_module()
    outside = tmp_path / "elsewhere" / "registry.json"

    assert mod.repo_relative(outside, tmp_path / "repo") == str(outside)

#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
export PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 PYTEST_ADDOPTS=
exec "${PARITY_PYTHON:-python3}" -m pytest --noconftest -p no:cacheprovider -q \
  tests/test_generate_upstream_patch_queue.py \
  tests/test_golden_parity_harness.py \
  tests/test_release_readiness_summary.py \
  tests/test_upstream_webhook_sync_rust_only_guard.py

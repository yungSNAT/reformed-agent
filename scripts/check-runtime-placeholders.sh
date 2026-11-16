#!/usr/bin/env bash
# Fails when runtime code under crates/ or scripts/ carries a placeholder marker.
# The check is the Rust test runtime_has_no_placeholder_markers.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
exec cargo test -q -p hermes-source-parity-tests --test runtime_source_guards \
  runtime_has_no_placeholder_markers -- --exact

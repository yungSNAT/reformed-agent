#!/usr/bin/env bash
# Fails when the Rust runtime spawns Python or defaults code execution to it.
# The check is the Rust test rust_runtime_spawns_no_python.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
exec cargo test -q -p hermes-source-parity-tests --test runtime_source_guards \
  rust_runtime_spawns_no_python -- --exact

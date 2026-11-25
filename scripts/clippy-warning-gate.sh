#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec cargo run --locked --manifest-path "${ROOT_DIR}/Cargo.toml" -q -p hermes-parity-tools -- clippy-warning-gate --repo-root "${ROOT_DIR}" "$@"

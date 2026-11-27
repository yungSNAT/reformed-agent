#!/usr/bin/env bash
# Runs `hermes-parity-tools sync-upstream` (see its --help); options pass through.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# launchd and cron PATHs often lack rustup's cargo.
export PATH="${PATH:-/usr/bin:/bin}${HOME:+:$HOME/.cargo/bin}"
exec cargo run --locked -q --manifest-path "$root/Cargo.toml" -p hermes-parity-tools -- sync-upstream --repo-root "$root" "$@"

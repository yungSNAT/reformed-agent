#!/usr/bin/env bash
# Installed crontab lines run this path; `hermes-parity-tools run-upstream-sync-cron` does the work.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export REPO_ROOT="${REPO_ROOT:-$root}" PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${HOME:+$HOME/.cargo/bin:}${PATH:-}"
exec cargo run --locked -q --manifest-path "$root/Cargo.toml" -p hermes-parity-tools -- run-upstream-sync-cron

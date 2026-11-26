#!/usr/bin/env sh
set -eu

bin="/usr/local/bin/hermes-entrypoint"
[ -x "$bin" ] || bin="hermes-entrypoint"
exec "$bin" "$@"

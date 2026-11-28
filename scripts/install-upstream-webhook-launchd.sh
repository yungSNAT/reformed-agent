#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/install-upstream-webhook-launchd.sh [options]

Install/update launchd user agents for webhook listener + queue worker.

Options:
  --repo-root <path>      Repository root (default: script parent)
  --label-prefix <prefix> launchd label prefix (default: com.hermes_agent_ultra)
  --agents-dir <path>     LaunchAgents directory (default: ~/Library/LaunchAgents)
  --env-file <path>       Env file read by wrappers (default: ~/.hermes-agent-ultra/upstream-webhook-sync.env)
  --log-dir <path>        Log directory (default: ~/.hermes-agent-ultra/logs)
  --no-load               Only write plist files, do not load with launchctl
  -h, --help              Show help
USAGE
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
LABEL_PREFIX="com.hermes_agent_ultra"
AGENTS_DIR="${HOME}/Library/LaunchAgents"
ENV_FILE="${HOME}/.hermes-agent-ultra/upstream-webhook-sync.env"
LOG_DIR="${HOME}/.hermes-agent-ultra/logs"
LOAD_SERVICES="1"
DEFAULT_HOSTNAME="$(hostname -s 2>/dev/null || hostname)"
DEFAULT_ELITE_CMD="python3 scripts/run-elite-sync-gate.py"
AUTO_GENERATE_SECRET="${UPSTREAM_SYNC_AUTO_GENERATE_SECRET:-1}"

ensure_env_key() {
  local file="$1"
  local key="$2"
  local value="$3"
  local tmp
  if grep -qE "^${key}=" "${file}"; then
    return 0
  fi
  tmp="$(mktemp)"
  cat "${file}" > "${tmp}"
  printf '\n%s=%q\n' "${key}" "${value}" >> "${tmp}"
  mv "${tmp}" "${file}"
}

replace_env_value() {
  local file="$1"
  local key="$2"
  local previous="$3"
  local value="$4"
  local tmp
  tmp="$(mktemp)"
  WEBHOOK_SETTING_VALUE="$(printf '%q' "${value}")" awk -v k="${key}" -v old="${key}=${previous}" '
    $0 == old { print k "=" ENVIRON["WEBHOOK_SETTING_VALUE"]; next }
    { print }
  ' "${file}" > "${tmp}"
  mv "${tmp}" "${file}"
}

is_true() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

generate_secret() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
    return
  fi
  python3 - <<'PY'
import secrets
print(secrets.token_hex(32))
PY
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-root)
      REPO_ROOT="${2:?missing value for --repo-root}"
      shift 2
      ;;
    --label-prefix)
      LABEL_PREFIX="${2:?missing value for --label-prefix}"
      shift 2
      ;;
    --agents-dir)
      AGENTS_DIR="${2:?missing value for --agents-dir}"
      shift 2
      ;;
    --env-file)
      ENV_FILE="${2:?missing value for --env-file}"
      shift 2
      ;;
    --log-dir)
      LOG_DIR="${2:?missing value for --log-dir}"
      shift 2
      ;;
    --no-load)
      LOAD_SERVICES="0"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "${LOAD_SERVICES}" == "1" && -f "${ENV_FILE}" ]]; then
  (
    # shellcheck disable=SC1090
    source "${ENV_FILE}"
    if [[ "${UPSTREAM_SYNC_BACKEND:-sqlite}" != "sqlite" ]]; then
      echo "UPSTREAM_SYNC_BACKEND must be sqlite before loading the agents." >&2
      exit 2
    fi
  )
fi

LISTENER_LABEL="${LABEL_PREFIX}.upstream_webhook_listener"
WORKER_LABEL="${LABEL_PREFIX}.upstream_webhook_worker"
LISTENER_PLIST="${AGENTS_DIR}/${LISTENER_LABEL}.plist"
WORKER_PLIST="${AGENTS_DIR}/${WORKER_LABEL}.plist"

LISTENER_WRAPPER="${REPO_ROOT}/scripts/run-upstream-webhook-listener.sh"
WORKER_WRAPPER="${REPO_ROOT}/scripts/run-upstream-webhook-worker.sh"

mkdir -p "${AGENTS_DIR}" "$(dirname "${ENV_FILE}")" "${LOG_DIR}"
chmod +x "${LISTENER_WRAPPER}" "${WORKER_WRAPPER}" "${REPO_ROOT}/scripts/upstream_webhook_sync.py"

if [[ ! -f "${ENV_FILE}" ]]; then
  cat > "${ENV_FILE}" <<EOF
# Upstream webhook sync runtime config.
# Keep this file private because it may contain webhook secrets.
# Guardrail: this stack is for DEV/INTEGRATION hosts only.

GITHUB_WEBHOOK_SECRET=
UPSTREAM_SYNC_BACKEND=sqlite
UPSTREAM_SYNC_SQLITE_PATH=$(printf '%q' "${REPO_ROOT}/.sync-queue/upstream-events.db")
UPSTREAM_SYNC_RUNTIME_ROLE=dev
UPSTREAM_SYNC_ALLOWED_HOSTNAME=$(printf '%q' "${DEFAULT_HOSTNAME}")
UPSTREAM_SYNC_DISABLE_DEV_GUARD=0

UPSTREAM_SYNC_EXPECTED_REPO=NousResearch/hermes-agent
UPSTREAM_SYNC_EXPECTED_REF=refs/heads/main
UPSTREAM_SYNC_HOST=127.0.0.1
UPSTREAM_SYNC_PORT=8099
UPSTREAM_SYNC_PATH=/github/upstream-sync

UPSTREAM_SYNC_STRATEGY=merge
UPSTREAM_SYNC_STRICT_RISK_GATE=1
UPSTREAM_SYNC_ELITE_GATE=0
UPSTREAM_SYNC_ELITE_CMD=$(printf '%q' "${DEFAULT_ELITE_CMD}")
UPSTREAM_SYNC_ELITE_ROLLBACK_CMD=
UPSTREAM_SYNC_ALLOW_RISK_PATHS=0
UPSTREAM_SYNC_NO_TESTS=0
UPSTREAM_SYNC_NO_PR=0
UPSTREAM_SYNC_MAX_ATTEMPTS=3
UPSTREAM_SYNC_TIMEOUT_SEC=1800
UPSTREAM_SYNC_POLL_INTERVAL_SEC=10
UPSTREAM_SYNC_CONFLICT_LABEL=upstream-sync-conflict
UPSTREAM_SYNC_DISABLE_PARITY_DRIFT_CHECK=0
UPSTREAM_SYNC_PARITY_UPSTREAM_REF=upstream/main
UPSTREAM_SYNC_PARITY_PARENT_ISSUE=13
UPSTREAM_SYNC_PARITY_LABELS=parity,parity-upkeep
UPSTREAM_SYNC_PARITY_OPEN_ISSUES=1
UPSTREAM_SYNC_DISABLE_GLOBAL_PARITY_CHECK=0
UPSTREAM_SYNC_GLOBAL_PARITY_PARENT_ISSUE=19
UPSTREAM_SYNC_GLOBAL_PARITY_LABELS=parity,parity-upkeep
UPSTREAM_SYNC_GLOBAL_PARITY_OPEN_ISSUES=1
UPSTREAM_SYNC_GLOBAL_PARITY_MAX_QUEUE_COMMITS=0

# Automation summary sink chain (ContextLattice first; fallback to github/local).
UPSTREAM_SYNC_SUMMARY_SINK_ORDER=contextlattice,github,local
UPSTREAM_SYNC_SUMMARY_CONTEXT_PROJECT=$(printf '%q' "${REPO_ROOT##*/}")
UPSTREAM_SYNC_SUMMARY_CONTEXT_TOPIC_PATH=ops/upstream-sync
UPSTREAM_SYNC_SUMMARY_CONTEXT_FILE_NAME=ops/upstream-sync.md
UPSTREAM_SYNC_SUMMARY_CONTEXT_TIMEOUT_SECS=8
UPSTREAM_SYNC_SUMMARY_GITHUB_ISSUE=13
UPSTREAM_SYNC_SUMMARY_LOCAL_PATH=$(printf '%q' "${REPO_ROOT}/.sync-reports/upstream-sync-summary-fallback.log")

# Optional Nous/Codex assist command invoked for risk_blocked/conflict/dead.
UPSTREAM_SYNC_ASSIST_CMD=

EOF
fi
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_RUNTIME_ROLE" "dev"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_ALLOWED_HOSTNAME" "${DEFAULT_HOSTNAME}"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_DISABLE_DEV_GUARD" "0"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_EXPECTED_REPO" "NousResearch/hermes-agent"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_DISABLE_PARITY_DRIFT_CHECK" "0"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_PARITY_UPSTREAM_REF" "upstream/main"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_PARITY_PARENT_ISSUE" "13"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_PARITY_LABELS" "parity,parity-upkeep"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_PARITY_OPEN_ISSUES" "1"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_DISABLE_GLOBAL_PARITY_CHECK" "0"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_GLOBAL_PARITY_PARENT_ISSUE" "19"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_GLOBAL_PARITY_LABELS" "parity,parity-upkeep"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_GLOBAL_PARITY_OPEN_ISSUES" "1"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_GLOBAL_PARITY_MAX_QUEUE_COMMITS" "0"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_SUMMARY_SINK_ORDER" "contextlattice,github,local"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_SUMMARY_CONTEXT_PROJECT" "${REPO_ROOT##*/}"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_SUMMARY_CONTEXT_TOPIC_PATH" "ops/upstream-sync"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_SUMMARY_CONTEXT_FILE_NAME" "ops/upstream-sync.md"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_SUMMARY_CONTEXT_TIMEOUT_SECS" "8"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_SUMMARY_GITHUB_ISSUE" "13"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_SUMMARY_LOCAL_PATH" "${REPO_ROOT}/.sync-reports/upstream-sync-summary-fallback.log"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_ELITE_GATE" "0"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_ELITE_CMD" "${DEFAULT_ELITE_CMD}"
ensure_env_key "${ENV_FILE}" "UPSTREAM_SYNC_ELITE_ROLLBACK_CMD" ""
if grep -Fxq "UPSTREAM_SYNC_ELITE_CMD=${DEFAULT_ELITE_CMD}" "${ENV_FILE}"; then
  replace_env_value "${ENV_FILE}" "UPSTREAM_SYNC_ELITE_CMD" "${DEFAULT_ELITE_CMD}" "${DEFAULT_ELITE_CMD}"
fi
if grep -qE '^UPSTREAM_SYNC_EXPECTED_REPO=Lumio-Research/hermes-agent-rs$' "${ENV_FILE}"; then
  replace_env_value "${ENV_FILE}" "UPSTREAM_SYNC_EXPECTED_REPO" \
    "Lumio-Research/hermes-agent-rs" "NousResearch/hermes-agent"
fi
if is_true "${AUTO_GENERATE_SECRET}" && grep -qE '^GITHUB_WEBHOOK_SECRET=$' "${ENV_FILE}"; then
  replace_env_value "${ENV_FILE}" "GITHUB_WEBHOOK_SECRET" "" "$(generate_secret)"
fi
chmod 600 "${ENV_FILE}"

escape_plist_text() {
  printf '%s\n' "$1" | awk '
    NR > 1 { printf "&#10;" }
    {
      gsub(/&/, "\\&amp;")
      gsub(/</, "\\&lt;")
      gsub(/>/, "\\&gt;")
      gsub(/\r/, "\\&#13;")
      printf "%s", $0
    }
  '
}

write_service_plist() {
  local destination="$1" label="$2" wrapper="$3" service="$4"
  local escaped_label escaped_wrapper
  escaped_label="$(escape_plist_text "${label}")"
  escaped_wrapper="$(escape_plist_text "${wrapper}")"
  cat > "${destination}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${escaped_label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/env</string>
    <string>bash</string>
    <string>${escaped_wrapper}</string>
  </array>
  <key>WorkingDirectory</key>
  <string>${escaped_repo_root}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>REPO_ROOT</key>
    <string>${escaped_repo_root}</string>
    <key>UPSTREAM_SYNC_ENV_FILE</key>
    <string>${escaped_env_file}</string>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>${escaped_log_dir}/upstream-webhook-${service}.out.log</string>
  <key>StandardErrorPath</key>
  <string>${escaped_log_dir}/upstream-webhook-${service}.err.log</string>
</dict>
</plist>
EOF
}

escaped_repo_root="$(escape_plist_text "${REPO_ROOT}")"
escaped_env_file="$(escape_plist_text "${ENV_FILE}")"
escaped_log_dir="$(escape_plist_text "${LOG_DIR}")"
write_service_plist "${LISTENER_PLIST}" "${LISTENER_LABEL}" "${LISTENER_WRAPPER}" listener
write_service_plist "${WORKER_PLIST}" "${WORKER_LABEL}" "${WORKER_WRAPPER}" worker

if [[ "${LOAD_SERVICES}" == "1" ]]; then
  launchctl bootout "gui/${UID}" "${LISTENER_PLIST}" >/dev/null 2>&1 || true
  launchctl bootout "gui/${UID}" "${WORKER_PLIST}" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/${UID}" "${LISTENER_PLIST}"
  launchctl bootstrap "gui/${UID}" "${WORKER_PLIST}"
  launchctl kickstart -k "gui/${UID}/${LISTENER_LABEL}" >/dev/null 2>&1 || true
  launchctl kickstart -k "gui/${UID}/${WORKER_LABEL}" >/dev/null 2>&1 || true
fi

echo "Installed launchd agents:"
echo "  ${LISTENER_PLIST}"
echo "  ${WORKER_PLIST}"
echo "Env file: ${ENV_FILE}"
echo "Logs:"
echo "  ${LOG_DIR}/upstream-webhook-listener.out.log"
echo "  ${LOG_DIR}/upstream-webhook-listener.err.log"
echo "  ${LOG_DIR}/upstream-webhook-worker.out.log"
echo "  ${LOG_DIR}/upstream-webhook-worker.err.log"
echo
echo "Check status with:"
echo "  bash ${REPO_ROOT}/scripts/status-upstream-webhook-launchd.sh --label-prefix ${LABEL_PREFIX}"

#!/usr/bin/env bash
set -euo pipefail

is_termux() {
  [[ -n "${TERMUX_VERSION:-}" ]] || [[ "${PREFIX:-}" == *"com.termux/files/usr"* ]]
}

default_install_dir() {
  if is_termux && [[ -n "${PREFIX:-}" ]]; then
    echo "${PREFIX}/bin"
  else
    echo "${HOME}/.local/bin"
  fi
}

resolve_install_dir() {
  if [[ "${INSTALL_DIR_EXPLICIT}" == "true" ]]; then
    return
  fi

  if is_termux && [[ -n "${PREFIX:-}" ]]; then
    INSTALL_DIR="${PREFIX}/bin"
    return
  fi

  if [[ "$(uname -s)" == "Linux" ]] && [[ "$(id -u)" -eq 0 ]]; then
    INSTALL_DIR="/usr/local/bin"
    ROOT_FHS_LAYOUT=true
    return
  fi

  INSTALL_DIR="$(default_install_dir)"
}

REPO="${REPO:-yungSNAT/reformed-agent}"
VERSION="${VERSION:-latest}"
if [[ -n "${HERMES_INSTALL_DIR:-}" ]]; then
  INSTALL_DIR="${HERMES_INSTALL_DIR}"
  INSTALL_DIR_EXPLICIT=true
elif [[ -n "${INSTALL_DIR:-}" ]]; then
  INSTALL_DIR="${INSTALL_DIR}"
  INSTALL_DIR_EXPLICIT=true
else
  INSTALL_DIR=""
  INSTALL_DIR_EXPLICIT=false
fi
# The agent's own home; upstream Hermes keeps ~/.hermes.
HERMES_HOME="${HERMES_HOME:-${HERMES_AGENT_ULTRA_HOME:-$HOME/.hermes-agent-ultra}}"
HERMES_RELEASE_BASE_URL="${HERMES_RELEASE_BASE_URL:-}"
HERMES_REQUIRE_SIGNATURE="${HERMES_REQUIRE_SIGNATURE:-false}"
CANONICAL_BIN_NAME="${CANONICAL_BIN_NAME:-hermes-agent-ultra}"
PRIMARY_BIN_NAME="${PRIMARY_BIN_NAME:-hermes-ultra}"
LEGACY_BIN_NAME="${LEGACY_BIN_NAME:-hermes}"
INSTALL_LEGACY_ALIAS="${INSTALL_LEGACY_ALIAS:-false}"
RELEASE_BIN_BASENAME="${RELEASE_BIN_BASENAME:-hermes}"
RUN_SETUP_MODE="${RUN_SETUP_MODE:-auto}" # auto|always|never
ROOT_FHS_LAYOUT=false
PATH_GUARD_APPLIED=false
POSITIONAL_VERSION=""
DEFAULT_SOUL_MD="You are Hermes Agent, an intelligent AI assistant created by Nous Research. You are helpful, knowledgeable, and direct. You assist users with a wide range of tasks including answering questions, writing and editing code, analyzing information, creative work, and executing actions via your tools. You communicate clearly, admit uncertainty when appropriate, and prioritize being genuinely useful over being verbose unless otherwise directed below. Be targeted and efficient in your exploration and investigations. When the user asks for an actionable task, execute immediately: run the first concrete step with available tools, then continue until completion. Do not stop at intent-only narration such as 'I'll proceed' without performing work."
if [[ -t 0 ]]; then
  IS_INTERACTIVE=true
else
  IS_INTERACTIVE=false
fi

show_help() {
  cat <<'EOF'
Usage: scripts/install.sh [version] [options]

Install Reformed Agent (hermes-agent-ultra) from GitHub Releases, or build it
from source when run from a checkout that has no release asset for this target.

Arguments:
  version                Release tag to install (default: latest)

Options:
  --version TAG          Release tag to install (same as positional version)
  --setup                Run post-install setup flow without prompting
  --skip-setup           Skip post-install setup flow
  --dir PATH             Install directory for binaries/symlink (explicit override)
  --hermes-home PATH     Hermes home directory for SOUL.md bootstrap
  -h, --help             Show this help

Environment variables:
  REPO                   GitHub repo slug (default: yungSNAT/reformed-agent)
  HERMES_INSTALL_DIR     Destination bin directory (explicit override)
  INSTALL_DIR            Destination bin directory (legacy alias, overridden by HERMES_INSTALL_DIR)
  HERMES_HOME            Hermes config dir for SOUL.md bootstrap (default: $HERMES_AGENT_ULTRA_HOME,
                         else $HOME/.hermes-agent-ultra)
  CANONICAL_BIN_NAME     Installed binary name (default: hermes-agent-ultra)
  PRIMARY_BIN_NAME       Primary user-facing command symlink (default: hermes-ultra)
  INSTALL_LEGACY_ALIAS   Also install the legacy hermes alias (default: false)
  LEGACY_BIN_NAME        Compatibility alias symlink name when enabled (default: hermes)
  RELEASE_BIN_BASENAME   Tarball executable basename (default: hermes)
  RUN_SETUP_MODE         auto|always|never for setup flow (default: auto)
  HERMES_INSTALL_PROBE_TIMEOUT_SECONDS
                         Timeout for post-install doctor/auth/setup probes (default: 45; 0 disables)
  HERMES_RELEASE_BASE_URL
                         Folder URL holding the release assets (default: the GitHub release)
  HERMES_REQUIRE_SIGNATURE
                         Refuse to install unless cosign verifies SHA256SUMS (default: false;
                         cosign is used whenever it is on PATH)

Every downloaded archive must match the release's SHA256SUMS. When cosign is on
PATH, SHA256SUMS must also carry a valid signature from this repository's
release workflow.

Defaults:
  - Linux root install:  /usr/local/bin (FHS-style command location)
  - User install:         ~/.local/bin
  - Termux install:       $PREFIX/bin
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      show_help
      exit 0
      ;;
    --version)
      if [[ $# -lt 2 ]]; then
        echo "--version requires a value" >&2
        exit 1
      fi
      VERSION="$2"
      shift 2
      ;;
    --setup)
      RUN_SETUP_MODE="always"
      shift
      ;;
    --skip-setup)
      RUN_SETUP_MODE="never"
      shift
      ;;
    --dir)
      if [[ $# -lt 2 ]]; then
        echo "--dir requires a value" >&2
        exit 1
      fi
      INSTALL_DIR="$2"
      INSTALL_DIR_EXPLICIT=true
      shift 2
      ;;
    --hermes-home)
      if [[ $# -lt 2 ]]; then
        echo "--hermes-home requires a value" >&2
        exit 1
      fi
      HERMES_HOME="$2"
      shift 2
      ;;
    --*)
      echo "Unknown option: $1" >&2
      show_help
      exit 1
      ;;
    *)
      if [[ -n "${POSITIONAL_VERSION}" ]]; then
        echo "Unexpected extra positional argument: $1" >&2
        show_help
        exit 1
      fi
      POSITIONAL_VERSION="$1"
      shift
      ;;
  esac
done

if [[ -n "${POSITIONAL_VERSION}" ]]; then
  VERSION="${POSITIONAL_VERSION}"
fi

case "${RUN_SETUP_MODE}" in
  auto|always|never) ;;
  *)
    echo "Invalid RUN_SETUP_MODE: ${RUN_SETUP_MODE} (expected auto|always|never)" >&2
    exit 1
    ;;
esac

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 1
  }
}

release_url() {
  local asset="$1"
  if [[ -n "${HERMES_RELEASE_BASE_URL}" ]]; then
    echo "${HERMES_RELEASE_BASE_URL%/}/${asset}"
  elif [[ "$VERSION" == "latest" ]]; then
    echo "https://github.com/${REPO}/releases/latest/download/${asset}"
  else
    echo "https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "Missing required command: sha256sum or shasum" >&2
    exit 1
  fi
}

refuse_install() {
  echo "Refusing to install: $*" >&2
  exit 1
}

# Pins "latest" to the tag it redirects to now, so every download comes from
# one release and the signature check can require that exact tag. Left as
# "latest" when GitHub has no release, so a checkout can still build from
# source.
resolve_latest_tag() {
  [[ "${VERSION}" == "latest" && -z "${HERMES_RELEASE_BASE_URL}" ]] || return 0
  local url
  url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" 2>/dev/null)" || return 0
  [[ "${url}" == */releases/tag/?* ]] && VERSION="${url##*/releases/tag/}"
  return 0
}

regex_escape() {
  printf '%s' "$1" | sed 's/[][\.*^$(){}+?|]/\\&/g'
}

# Checks the downloaded archive against the release's SHA256SUMS, and that file
# against its cosign signature, before anything is extracted.
verify_release_asset() {
  local archive="$1" asset="$2" sums="${TMP_DIR}/SHA256SUMS"
  curl -fsSL "$(release_url SHA256SUMS)" -o "${sums}" \
    || refuse_install "could not download SHA256SUMS for ${asset}."
  if command -v cosign >/dev/null 2>&1; then
    [[ "${VERSION}" != "latest" ]] \
      || refuse_install "could not resolve the latest release tag, so its signature cannot be checked; pass a version."
    curl -fsSL "$(release_url SHA256SUMS.sigstore.json)" -o "${sums}.sigstore.json" \
      || refuse_install "could not download the signature for SHA256SUMS."
    if ! cosign verify-blob \
      --bundle "${sums}.sigstore.json" \
      --certificate-identity-regexp "^https://github\.com/$(regex_escape "${REPO}")/\.github/workflows/release\.yml@refs/tags/$(regex_escape "${VERSION}")\$" \
      --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
      "${sums}" >"${TMP_DIR}/cosign.log" 2>&1; then
      cat "${TMP_DIR}/cosign.log" >&2
      local cosign_major
      cosign_major="$(cosign version 2>/dev/null | sed -n 's/^GitVersion:[[:space:]]*v\{0,1\}\([0-9][0-9]*\)\..*/\1/p' | head -n 1)"
      if [[ -n "${cosign_major}" && "${cosign_major}" -lt 3 ]]; then
        refuse_install "cosign ${cosign_major} could not verify SHA256SUMS; releases after v0.22.0 are signed with cosign 3, so install cosign 3.0 or later and retry."
      fi
      refuse_install "SHA256SUMS is not signed by ${REPO}'s release workflow for ${VERSION}."
    fi
    echo "Verified the signature on SHA256SUMS."
  elif truthy_env "${HERMES_REQUIRE_SIGNATURE}"; then
    refuse_install "HERMES_REQUIRE_SIGNATURE is set but cosign is not on PATH."
  else
    echo "cosign not found; checking SHA256SUMS without its signature. Install cosign to verify it."
  fi
  local expected actual
  expected="$(awk -v name="${asset}" '$2 == name || $2 == "*" name {print $1; exit}' "${sums}")"
  [[ -n "${expected}" ]] || refuse_install "${asset} is not listed in SHA256SUMS."
  actual="$(sha256_of "${archive}")"
  [[ "${actual}" == "${expected}" ]] \
    || refuse_install "${asset} does not match SHA256SUMS (expected ${expected}, got ${actual})."
  echo "Verified ${asset} against SHA256SUMS."
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux) os="linux" ;;
    Darwin) os="macos" ;;
    *) echo "Unsupported OS: $os" >&2; exit 1 ;;
  esac

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *) echo "Unsupported arch: $arch" >&2; exit 1 ;;
  esac

  echo "${os}-${arch}"
}

prompt_yes_no() {
  local question="$1"
  local default_yes="${2:-yes}"
  local answer=""
  local suffix
  case "${default_yes}" in
    [yY]|[yY][eE][sS]|[tT][rR][uU][eE]|1)
      suffix="[Y/n]"
      ;;
    *)
      suffix="[y/N]"
      ;;
  esac

  if [[ "${IS_INTERACTIVE}" == "true" ]]; then
    read -r -p "${question} ${suffix} " answer || answer=""
  elif [[ -r /dev/tty && -w /dev/tty ]]; then
    printf "%s %s " "${question}" "${suffix}" > /dev/tty
    IFS= read -r answer < /dev/tty || answer=""
  else
    answer=""
  fi

  answer="${answer#"${answer%%[![:space:]]*}"}"
  answer="${answer%"${answer##*[![:space:]]}"}"
  if [[ -z "${answer}" ]]; then
    case "${default_yes}" in
      [yY]|[yY][eE][sS]|[tT][rR][uU][eE]|1) return 0 ;;
      *) return 1 ;;
    esac
  fi
  case "${answer}" in
    y|Y|yes|YES|Yes) return 0 ;;
    *) return 1 ;;
  esac
}

truthy_env() {
  case "${1:-}" in
    1|true|TRUE|True|yes|YES|Yes|on|ON|On) return 0 ;;
    *) return 1 ;;
  esac
}

run_bounded_post_install_cmd() {
  local label="$1"
  shift
  local seconds="${HERMES_INSTALL_PROBE_TIMEOUT_SECONDS:-45}"
  case "${seconds}" in
    ''|*[!0-9]*) seconds=45 ;;
  esac

  if [[ "${seconds}" -eq 0 ]]; then
    "$@"
    return $?
  fi

  local safe_label="${label//[^A-Za-z0-9_]/_}"
  local marker="${TMP_DIR:-/tmp}/hermes-install-timeout-${safe_label}-$$-${RANDOM}"
  local done_marker="${TMP_DIR:-/tmp}/hermes-install-done-${safe_label}-$$-${RANDOM}"
  rm -f "${marker}" "${done_marker}"

  "$@" &
  local cmd_pid=$!
  (
    local elapsed=0
    while [[ "${elapsed}" -lt "${seconds}" ]]; do
      sleep 1
      if [[ -f "${done_marker}" ]]; then
        exit 0
      fi
      elapsed=$((elapsed + 1))
    done
    if [[ ! -f "${done_marker}" ]] && kill -0 "${cmd_pid}" 2>/dev/null; then
      : > "${marker}"
      kill -TERM "${cmd_pid}" 2>/dev/null || true
      sleep 2
      kill -KILL "${cmd_pid}" 2>/dev/null || true
    fi
  ) &
  local watcher_pid=$!

  local status=0
  wait "${cmd_pid}" || status=$?
  : > "${done_marker}"
  wait "${watcher_pid}" 2>/dev/null || true

  if [[ -f "${marker}" ]]; then
    rm -f "${marker}" "${done_marker}"
    echo "${label} timed out after ${seconds}s; continuing. Run manually:"
    echo "  $*"
    return 124
  fi

  rm -f "${done_marker}"
  return "${status}"
}

run_post_install_flow() {
  local bin_path="$1"
  local -a clean_python_env=(
    env
    -u PYTHONHOME
    -u PYTHONPATH
    -u PYTHONSTARTUP
    -u VIRTUAL_ENV
    -u CONDA_PREFIX
    -u CONDA_DEFAULT_ENV
    -u PIP_REQUIRE_VIRTUALENV
    -u PYTHONUSERBASE
  )

  echo
  echo "Running post-install verification..."
  run_bounded_post_install_cmd "doctor" "${clean_python_env[@]}" "${bin_path}" doctor || true

  echo
  echo "Current auth/platform status:"
  run_bounded_post_install_cmd "auth status" "${clean_python_env[@]}" "${bin_path}" auth status || true

  if [[ -t 0 ]]; then
    echo
    if prompt_yes_no "Run interactive setup now?" "yes"; then
      run_bounded_post_install_cmd "setup" "${clean_python_env[@]}" "${bin_path}" setup || true
    else
      echo "Skipped setup. Run this anytime:"
      echo "  ${bin_path} setup"
    fi
  else
    echo
    echo "Interactive setup skipped (non-interactive shell)."
    echo "Run later:"
    echo "  ${bin_path} setup"
  fi
}

need_cmd curl
need_cmd tar
need_cmd install

TARGET="$(detect_target)"
resolve_install_dir

# Keep installer-invoked commands deterministic in non-login shells (e.g. root on RHEL)
# where /usr/local/bin may not be present in PATH yet.
if [[ ":${PATH}:" != *":${INSTALL_DIR}:"* ]]; then
  export PATH="${INSTALL_DIR}:${PATH}"
  PATH_GUARD_APPLIED=true
fi

ASSET_CANDIDATES=("${RELEASE_BIN_BASENAME}-${TARGET}.tar.gz")
if [[ "$TARGET" == "macos-aarch64" ]]; then
  ASSET_CANDIDATES+=("${RELEASE_BIN_BASENAME}-macos-arm64.tar.gz")
elif [[ "$TARGET" == "linux-aarch64" ]]; then
  ASSET_CANDIDATES+=("${RELEASE_BIN_BASENAME}-linux-arm64.tar.gz")
fi

mkdir -p "${INSTALL_DIR}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

SOURCE_BIN=""
DOWNLOADED_ASSET=""
resolve_latest_tag
for asset in "${ASSET_CANDIDATES[@]}"; do
  URL="$(release_url "${asset}")"
  echo "Installing ${CANONICAL_BIN_NAME} from ${URL}"
  if curl -fsSL "${URL}" -o "${TMP_DIR}/${RELEASE_BIN_BASENAME}.tar.gz"; then
    DOWNLOADED_ASSET="${asset}"
    break
  fi
done

if [[ -n "${DOWNLOADED_ASSET}" ]]; then
  verify_release_asset "${TMP_DIR}/${RELEASE_BIN_BASENAME}.tar.gz" "${DOWNLOADED_ASSET}"
  tar -xzf "${TMP_DIR}/${RELEASE_BIN_BASENAME}.tar.gz" -C "${TMP_DIR}"
  for candidate in "${CANONICAL_BIN_NAME}" "${PRIMARY_BIN_NAME}" "${RELEASE_BIN_BASENAME}" "${LEGACY_BIN_NAME}"; do
    if [[ -f "${TMP_DIR}/${candidate}" ]]; then
      SOURCE_BIN="${TMP_DIR}/${candidate}"
      break
    fi
  done
fi

if [[ -z "${SOURCE_BIN}" ]]; then
  if truthy_env "${HERMES_REQUIRE_SIGNATURE}"; then
    refuse_install "HERMES_REQUIRE_SIGNATURE is set but there is no signed release archive for ${TARGET} in ${VERSION}, and a source build carries no signature."
  fi
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  SOURCE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
  if [[ -f "${SOURCE_ROOT}/Cargo.toml" ]] && command -v cargo >/dev/null 2>&1; then
    echo "Release asset not available for target (${TARGET}); building from source at ${SOURCE_ROOT}"
    (
      cd "${SOURCE_ROOT}"
      cargo build --release -p hermes-cli --bin "${CANONICAL_BIN_NAME}"
    )
    SOURCE_BIN="${SOURCE_ROOT}/target/release/${CANONICAL_BIN_NAME}"
  else
    echo "No executable release asset found for target (${TARGET}) in repo ${REPO}." >&2
    echo "Tried assets: ${ASSET_CANDIDATES[*]}" >&2
    exit 1
  fi
fi

if [[ ! -f "${SOURCE_BIN}" ]]; then
  echo "Built binary not found at expected path: ${SOURCE_BIN}" >&2
  exit 1
fi

install -m 0755 "${SOURCE_BIN}" "${INSTALL_DIR}/${CANONICAL_BIN_NAME}"
if [[ "${PRIMARY_BIN_NAME}" != "${CANONICAL_BIN_NAME}" ]]; then
  ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${PRIMARY_BIN_NAME}"
fi
if truthy_env "${INSTALL_LEGACY_ALIAS}" && [[ -n "${LEGACY_BIN_NAME}" ]] && [[ "${LEGACY_BIN_NAME}" != "${CANONICAL_BIN_NAME}" ]]; then
  ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${LEGACY_BIN_NAME}"
elif [[ -n "${LEGACY_BIN_NAME}" ]] && [[ "${LEGACY_BIN_NAME}" != "${CANONICAL_BIN_NAME}" ]]; then
  echo "Legacy ${LEGACY_BIN_NAME} alias not installed by default; existing upstream ${LEGACY_BIN_NAME} commands are left untouched."
fi

echo "Installed to ${INSTALL_DIR}/${CANONICAL_BIN_NAME}"
if [[ ! -x "${INSTALL_DIR}/${CANONICAL_BIN_NAME}" ]]; then
  echo "Install appears incomplete: ${INSTALL_DIR}/${CANONICAL_BIN_NAME} is not executable." >&2
  exit 1
fi

mkdir -p "${HERMES_HOME}"
if [[ ! -f "${HERMES_HOME}/SOUL.md" ]]; then
  printf '%s' "${DEFAULT_SOUL_MD}" > "${HERMES_HOME}/SOUL.md"
  echo "Created ${HERMES_HOME}/SOUL.md (edit to customize personality)."
fi

if command -v "${CANONICAL_BIN_NAME}" >/dev/null 2>&1; then
  echo "Detected on PATH: $(command -v "${CANONICAL_BIN_NAME}")"
  if command -v "${PRIMARY_BIN_NAME}" >/dev/null 2>&1; then
    echo "Primary command available: $(command -v "${PRIMARY_BIN_NAME}")"
  fi
  if truthy_env "${INSTALL_LEGACY_ALIAS}" && [[ -n "${LEGACY_BIN_NAME}" ]] && command -v "${LEGACY_BIN_NAME}" >/dev/null 2>&1; then
    echo "Legacy alias available: $(command -v "${LEGACY_BIN_NAME}")"
  fi
else
  echo "Binary is not currently on PATH."
  if [[ "${ROOT_FHS_LAYOUT}" == "true" ]]; then
    echo "/usr/local/bin is usually already on PATH. Open a new shell and try again."
  else
    echo "Add this line to your shell config, then restart your shell:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo
    echo "zsh quick apply:"
    echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc && source ~/.zshrc"
    echo "  exec zsh -l"
  fi
fi

if [[ "${PATH_GUARD_APPLIED}" == "true" ]]; then
  echo "PATH guard applied for this install shell: ${INSTALL_DIR}"
fi

BIN_PATH="${INSTALL_DIR}/${CANONICAL_BIN_NAME}"
if [[ -x "${BIN_PATH}" ]]; then
  case "${RUN_SETUP_MODE}" in
    always)
      run_post_install_flow "${BIN_PATH}"
      ;;
    auto)
      if [[ "${IS_INTERACTIVE}" != "true" ]]; then
        echo "Post-install setup skipped (non-interactive install). Run later:"
        echo "  ${BIN_PATH} setup"
        echo "To run verification during install, pass --setup or set RUN_SETUP_MODE=always."
      elif prompt_yes_no "Run post-install setup flow (doctor + auth status + setup)?" "yes"; then
        run_post_install_flow "${BIN_PATH}"
      else
        echo "Post-install setup skipped. Run later:"
        echo "  ${BIN_PATH} setup"
      fi
      ;;
    never)
      echo "Post-install setup skipped (--skip-setup)."
      ;;
  esac
fi

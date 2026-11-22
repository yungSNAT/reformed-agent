#!/usr/bin/env bash
# HyperFrames setup for Hermes.
#
# Verifies Node >= 22 and FFmpeg, installs the `hyperframes` CLI with `npm ci`
# from the package-lock.json beside this script, links it where a global npm
# install would put it, pre-caches `chrome-headless-shell`, and runs
# `hyperframes doctor`.
#
# The CLI is pinned to the version in package.json rather than taken at its
# latest release, and `npm ci` checks every package against the integrity hash
# in package-lock.json. The pin must stay at 0.4.2 or later, which carries the
# OpenClaw/Chromium-147 fix from
# https://github.com/heygen-com/hyperframes/issues/294 (commit 4c72ba4): the
# engine auto-detects `HeadlessExperimental.beginFrame` support and falls back
# to screenshot capture otherwise. To move the pin, change the version in
# package.json and run `npm install --package-lock-only` beside it.
#
# HYPERFRAMES_PREFIX sets the install folder (default
# ${HERMES_HOME:-~/.hermes}/tools/hyperframes).
#
# Idempotent: safe to re-run.

set -euo pipefail

MIN_NODE_MAJOR=22
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HYPERFRAMES_PREFIX="${HYPERFRAMES_PREFIX:-${HERMES_HOME:-$HOME/.hermes}/tools/hyperframes}"

red()    { printf '\033[31m%s\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
bold()   { printf '\033[1m%s\033[0m\n' "$*"; }

bold "==> HyperFrames setup"

# --- 1. Node.js --------------------------------------------------------------

if ! command -v node >/dev/null 2>&1; then
  red "✗ Node.js is not installed."
  echo "   Install Node.js >= ${MIN_NODE_MAJOR} (nvm, Homebrew, or your package manager) and re-run."
  exit 1
fi

node_version="$(node --version | sed 's/^v//')"
node_major="$(echo "$node_version" | cut -d. -f1)"
if [ "$node_major" -lt "$MIN_NODE_MAJOR" ]; then
  red "✗ Node.js ${node_version} is too old. HyperFrames requires Node.js >= ${MIN_NODE_MAJOR}."
  echo "   Upgrade with 'nvm install ${MIN_NODE_MAJOR} && nvm use ${MIN_NODE_MAJOR}' or your package manager."
  exit 1
fi
green "✓ Node.js ${node_version}"

# --- 2. FFmpeg ---------------------------------------------------------------

if ! command -v ffmpeg >/dev/null 2>&1; then
  red "✗ FFmpeg is not installed."
  case "$(uname -s)" in
    Linux*)   echo "   sudo apt-get install -y ffmpeg   # Debian/Ubuntu"
              echo "   sudo dnf install -y ffmpeg       # Fedora/RHEL";;
    Darwin*)  echo "   brew install ffmpeg";;
    MINGW*|MSYS*|CYGWIN*) echo "   winget install Gyan.FFmpeg";;
    *)        echo "   See https://ffmpeg.org/download.html";;
  esac
  exit 1
fi
green "✓ FFmpeg $(ffmpeg -version 2>&1 | head -1 | awk '{print $3}')"

# --- 3. npm ------------------------------------------------------------------

if ! command -v npm >/dev/null 2>&1; then
  red "✗ npm is not installed (should ship with Node.js)."
  exit 1
fi

# --- 4. Install the pinned hyperframes CLI ----------------------------------

HYPERFRAMES_VERSION="$(node -p 'require(process.argv[1]).dependencies.hyperframes' "$SCRIPT_DIR/package.json")"
bold "==> Installing hyperframes CLI ${HYPERFRAMES_VERSION}"

mkdir -p "$HYPERFRAMES_PREFIX"
cp "$SCRIPT_DIR/package.json" "$SCRIPT_DIR/package-lock.json" "$HYPERFRAMES_PREFIX/"
if ! npm ci --prefix "$HYPERFRAMES_PREFIX" --no-audit --no-fund >/dev/null 2>&1; then
  red "✗ npm ci failed in ${HYPERFRAMES_PREFIX}."
  echo "   Re-run it to see why: npm ci --prefix \"${HYPERFRAMES_PREFIX}\""
  exit 1
fi
hyperframes_bin="$HYPERFRAMES_PREFIX/node_modules/.bin/hyperframes"

installed_version="$("$hyperframes_bin" --version 2>/dev/null | tail -1 | sed 's/^v//')"
if [ "$installed_version" != "$HYPERFRAMES_VERSION" ]; then
  red "✗ Expected hyperframes ${HYPERFRAMES_VERSION} in ${HYPERFRAMES_PREFIX}, found '${installed_version}'."
  exit 1
fi

# Link the CLI into npm's global bin folder, where `npm install -g` used to put
# it, so `hyperframes` and `npx hyperframes` keep finding it.
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    green "✓ hyperframes ${installed_version} installed in ${HYPERFRAMES_PREFIX}"
    yellow "  Add ${HYPERFRAMES_PREFIX}/node_modules/.bin to PATH to run it as 'hyperframes'."
    ;;
  *)
    global_bin="$(npm prefix -g)/bin"
    if ! { mkdir -p "$global_bin" && ln -sfn "$hyperframes_bin" "$global_bin/hyperframes"; } 2>/dev/null; then
      red "✗ Could not link hyperframes into ${global_bin}."
      echo "   Try: sudo ln -sfn \"$hyperframes_bin\" \"$global_bin/hyperframes\""
      echo "   Or add ${HYPERFRAMES_PREFIX}/node_modules/.bin to PATH."
      exit 1
    fi
    green "✓ hyperframes ${installed_version} installed in ${HYPERFRAMES_PREFIX} and linked into ${global_bin}"
    ;;
esac

# --- 5. Pre-cache chrome-headless-shell --------------------------------------
#
# Chromium 147+ removed HeadlessExperimental.beginFrame. System Chrome (e.g.
# /usr/bin/google-chrome) can't render with the fast path, so the engine
# auto-detects and falls back to screenshot mode — but BeginFrame mode is
# faster and produces higher-quality output. Install chrome-headless-shell
# up front so the engine picks it over system Chrome.

bold "==> Pre-caching chrome-headless-shell (for best render quality)"

if ! npx --yes puppeteer browsers install chrome-headless-shell >/dev/null 2>&1; then
  yellow "⚠ Could not pre-install chrome-headless-shell."
  yellow "  Rendering will still work via screenshot-mode fallback (slower)."
  yellow "  If you hit HeadlessExperimental.beginFrame errors:"
  yellow "     export PRODUCER_FORCE_SCREENSHOT=true"
  yellow "  See references/troubleshooting.md."
else
  green "✓ chrome-headless-shell installed"
fi

# --- 6. Doctor ---------------------------------------------------------------

bold "==> Running hyperframes doctor"

if "$hyperframes_bin" doctor; then
  green "✓ HyperFrames is ready"
  echo
  echo "   Scaffold a project:   npx hyperframes init my-video"
  echo "   Preview:              npx hyperframes preview"
  echo "   Render:               npx hyperframes render"
else
  yellow "⚠ hyperframes doctor reported issues."
  yellow "  See references/troubleshooting.md or re-run 'hyperframes doctor'."
  exit 1
fi

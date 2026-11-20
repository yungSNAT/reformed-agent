#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UPSTREAM_REMOTE="${UPSTREAM_REMOTE:-upstream}"
UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/NousResearch/hermes-agent.git}"
UPSTREAM_REVISION="${UPSTREAM_REVISION:-$(tr -d '[:space:]' < "${REPO_ROOT}/docs/parity/upstream-reference.txt")}"
UPSTREAM_BRANCH="parity-reference"
if [[ -n "${UPSTREAM_REF:-}" ]]; then
  echo "run-repo-ci: UPSTREAM_REF is ignored; the parity gates use the pinned revision (set UPSTREAM_REVISION to compare against another)." >&2
fi
UPSTREAM_REF="${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"
SKIP_UPSTREAM_FETCH="${SKIP_UPSTREAM_FETCH:-false}"

usage() {
  cat <<'USAGE'
Usage: scripts/run-repo-ci.sh [--skip-upstream-fetch]

Run the repository-local CI contract. This is the authoritative gate for local
changes; hosted GitHub Actions are a convenience mirror when they run.

The parity gates compare against one pinned upstream revision, recorded in
docs/parity/upstream-reference.txt and fetched as upstream/parity-reference, so
a result depends on the change under test and not on that day's upstream. The
scheduled parity audit reports drift from upstream's live main.

Environment:
  CARGO_TARGET_DIR       Optional external Cargo target directory.
  CARGO_INCREMENTAL      Optional Cargo incremental setting.
  PARITY_PYTHON          Python with pytest 9.2.0 for parity governance tests (default: python3).
  UPSTREAM_REMOTE        Upstream remote name (default: upstream).
  UPSTREAM_URL           Upstream remote URL (default: NousResearch/hermes-agent).
  UPSTREAM_REVISION      Upstream commit or branch to gate against (default: the
                         pinned revision; use main to measure current drift).
  SKIP_UPSTREAM_FETCH    true/false; use the existing upstream/parity-reference.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-upstream-fetch)
      SKIP_UPSTREAM_FETCH=true
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

run() {
  echo
  echo "[repo-ci] $*"
  "$@"
}

refresh_upstream() {
  if [[ "${SKIP_UPSTREAM_FETCH}" == "true" ]]; then
    echo "[repo-ci] skipping upstream fetch; using ${UPSTREAM_REF} as-is"
    return
  fi
  if ! git -C "${REPO_ROOT}" remote get-url "${UPSTREAM_REMOTE}" >/dev/null 2>&1; then
    git -C "${REPO_ROOT}" remote add "${UPSTREAM_REMOTE}" "${UPSTREAM_URL}"
  fi
  run git -C "${REPO_ROOT}" fetch --no-tags --depth=1 \
    "${UPSTREAM_REMOTE}" \
    "+${UPSTREAM_REVISION}:refs/remotes/${UPSTREAM_REF}"
}

cd "${REPO_ROOT}"

refresh_upstream
run bash scripts/run-python-parity-governance-tests.sh

run cargo run --locked -q -p hermes-parity-tools -- generate-parity-matrix --local-ref HEAD --upstream-branch "${UPSTREAM_BRANCH}" --no-fetch
run cargo run --locked -q -p hermes-parity-tools -- generate-workstream-status --upstream-ref "${UPSTREAM_REF}" --no-fetch
run cargo run --locked -q -p hermes-parity-tools -- generate-test-intent-mapping
run cargo run --locked -q -p hermes-parity-tools -- validate-intentional-divergence --check --allow-warnings --upstream-ref "${UPSTREAM_REF}" --no-fetch
run cargo run --locked -q -p hermes-parity-tools -- generate-test-coverage-audit
run cargo run --locked -q -p hermes-parity-tools -- generate-test-coverage-audit --check
run cargo run --locked -q -p hermes-parity-tools -- generate-adapter-matrix
run cargo run --locked -q -p hermes-parity-tools -- generate-shared-diff-backlog --local-ref HEAD --no-fetch --upstream-branch "${UPSTREAM_BRANCH}"
run python3 scripts/generate-upstream-patch-queue.py --local-ref HEAD --max-commits 0 --upstream-branch "${UPSTREAM_BRANCH}" --no-fetch
run python3 scripts/generate-global-parity-proof.py
run python3 scripts/generate-release-readiness-summary.py --check

run cargo fmt --all --check
run bash scripts/clippy-warning-gate.sh --check
run cargo test -p hermes-source-parity-tests --test rust_module_size_policy -- --nocapture
run bash scripts/check-runtime-placeholders.sh
run cargo test --workspace
run cargo test -p hermes-parity-tests
run cargo test -p hermes-source-parity-tests --test cli_command_contract
run cargo test -p hermes-protocol-parity-tests --test protocol_differential_contracts
run cargo run --locked -q -p hermes-parity-tools -- run-upstream-slash-parity-gate --upstream-ref "${UPSTREAM_REF}"
run cargo run --locked -q -p hermes-parity-tools -- run-upstream-surface-coverage-gate --repo-root . --upstream-ref "${UPSTREAM_REF}" --local-ref HEAD
run cargo test -p hermes-source-parity-tests --test global_parity_governance

for script in \
  scripts/upstream_webhook_sync.py \
  scripts/publish_automation_summary.py \
  scripts/run-golden-parity-harness.py \
  scripts/generate-release-readiness-summary.py \
  scripts/release_secret_scan.py; do
  run python3 -m py_compile "${script}"
done

echo
printf '[repo-ci] PASS\n'

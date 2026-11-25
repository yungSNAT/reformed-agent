# Upstream Sync Automation

This fork includes automation to keep `main` close to upstream while preserving
fork-specific history.

## Subsystem refresh for v0.23.2

Refresh one subsystem per pull request against the commit in the
[upstream reference](parity/upstream-reference.txt). Start by comparing the
upstream behaviour and its tests with the shipped Rust path. Preserve any
fork-specific behaviour in the same path with a regression test.

The v0.23.2 baseline at `bd8f0102` has 22 deferred shared-file classifications.
The 3 coverage manifests have 141 deferred rows and 337 older coverage claims
whose upstream file changed. These are counts of report rows. A behaviour may
appear in several reports.

| Subsystem | Evidence for the next refresh |
| --- | --- |
| Gateway, sessions and providers | Read the deferred gateway tests and plugin entries in the [shared-file backlog](parity/shared-diff-backlog.md). Check trust boundaries first. Exercise credentials, session transitions and requests with local stubs where the upstream test requires a service. |
| Core runtime and tools | Start with the 46 deferred and 131 older rows in the [Python suite manifest](parity/python-test-suite-coverage.json). Map each applicable upstream case to a Rust runtime test. |
| CLI | Start with the 36 deferred and 110 older rows in the [CLI manifest](parity/hermes-cli-test-coverage.json). Exercise the installed command or its parser where upstream asserts its exit status, arguments or output. |
| TUI and desktop | Start with the 59 deferred and 96 older rows in the [TUI manifest](parity/ui-tui-source-coverage.json). Run the client fixture against the corresponding Rust gateway for changes to the connection or session contract. |
| Bundled plugins and skills | Compare the upstream interface and the shipped bundle. Use the [intentional divergence registry](parity/intentional-divergence.json) for a deliberate difference and record where the shipped behaviour is checked. |

Update each reviewed manifest row with the matching Rust test or an explanation
for the remaining gap. Preserve evidence of which upstream case the test
exercises. Regenerate the [parity reports](parity/README.md) against the same
upstream pin and run the governance checks before merging. The public parity
audit reports live upstream drift separately from the checks against this pin.

## Scripts

- `scripts/sync-upstream.sh`
  - One-shot upstream sync (`fetch -> strategy apply -> test -> push`)
  - Default mode: create a sync branch and open a PR
  - Supports `--draft-pr` for safer review-first PR creation
  - Supports `--pr-labels` to apply metadata/risk labels on the created PR
  - Runs adversarial regression gate by default (`cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate`)
  - Supports `--no-redteam-gate` and `--redteam-cmd` overrides
  - Supports optional consolidated elite gate: `--elite-gate` / `--elite-cmd`
  - Refreshes generated parity docs by default (`README` sync status + parity dashboard)
  - Supports `--no-doc-refresh` and `--doc-refresh-cmd` overrides
  - Supports `--strategy merge|cherry-pick`
  - Supports strict risk gating via `--strict-risk-gate`
  - Emits timestamped reports under `.sync-reports/`
  - Can auto-create a labeled GitHub issue on conflict
- `scripts/cron-upstream-sync.sh`
  - Cron-safe wrapper around `sync-upstream.sh`
  - Uses a lock file (`~/.hermes/locks/upstream-sync.lock`) to avoid overlap
  - Reads optional env knobs: `SYNC_STRATEGY`, `REPORT_DIR`, `CONFLICT_LABEL`, `CREATE_CONFLICT_ISSUE`, `STRICT_RISK_GATE`, `ALLOW_RISK_PATHS`, `RISK_PATHS_FILE`
- `scripts/install-upstream-sync-cron.sh`
  - Installs/updates a crontab entry with a stable marker
- `scripts/upstream_webhook_sync.py`
  - Webhook listener/worker path with SQLite/SQS/Kafka queue backends
  - Includes per-upstream-commit upkeep issue queue automation (default parent issue: `#43`)
- `run-adapter-chaos-harness` (`crates/hermes-parity-tools`)
  - Runs deterministic adapter chaos scenarios (timeout/5xx/rate-limit)
  - Emits JSON diagnostics under `.sync-reports/adapter-chaos-<timestamp>.json`
- `run-zero-copy-hotpath-bench` (`crates/hermes-parity-tools`)
  - Runs zero-copy policy hot-path benchmark test and captures ns/eval evidence
  - Emits JSON diagnostics under `.sync-reports/zero-copy-hotpath-<timestamp>.json`
- `scripts/run-elite-sync-gate.py`
  - Runs red-team + adapter chaos + zero-copy hot-path + differential parity + eval trend + security gate v2 + performance autopilot as one gate
  - Emits JSON diagnostics under `.sync-reports/elite-sync-gate-<timestamp>.json`
- `cargo run --locked -q -p hermes-parity-tools -- run-security-release-gate-v2 --repo-root .`
  - Runs the release scan, SBOM inventory (`cargo metadata`), provenance signature, redaction and ACP multimodal tests
  - Emits JSON diagnostics under `.sync-reports/security-release-gate-v2-<timestamp>.json`
- `run-performance-autopilot` (`crates/hermes-parity-tools`)
  - Runs hot-path + eval trend checks and emits deterministic tuning recommendations (`json` + `markdown`)
- `audit-background-queue` (`crates/hermes-parity-tools`)
  - Audits background job manifests for malformed JSON, stale running jobs, and duplicate active tasks
  - Optional repair mode: `--repair`
- `run-differential-parity-gate` (`crates/hermes-parity-tools`)
  - Compares local CLI command/action surface with `upstream/main`
  - Emits gate artifact under `.sync-reports/differential-parity-gate-<timestamp>.json`
- `run-upstream-surface-coverage-gate` (`crates/hermes-parity-tools`)
  - Verifies required upstream surfaces are present locally (`skills`, `optional-skills`, `plugins`, `tests`, `website`, `ui-tui`, `docs`)
  - Fails on unclassified missing upstream files under those prefixes
  - Classifies approved Rust-first divergences from `docs/parity/intentional-divergence.json` separately from actionable missing paths
  - Supports `--local-mode worktree` for pre-commit validation while changes are still unstaged/uncommitted
  - Emits gate artifact under `.sync-reports/upstream-surface-coverage-gate-<timestamp>.json`
- `run-upstream-slash-parity-gate` (`crates/hermes-parity-tools`)
  - Compares upstream `/` command surface (`hermes_cli/commands.py`) against Rust `SLASH_COMMANDS`
  - Fails on missing upstream commands unless explicitly allowlisted in `docs/parity/slash-command-divergence.json`
  - Emits gate artifact under `.sync-reports/upstream-slash-parity-gate-<timestamp>.json`
- `run-eval-trend-gate` (`crates/hermes-parity-tools`)
  - Compares eval run baselines and enforces regression thresholds
  - Emits gate artifact under `.sync-reports/eval-trend-gate-<timestamp>.json`
- `run-self-evolution-loop` (`crates/hermes-parity-tools`)
  - Runs a unified intelligence loop across golden parity + eval trend + elite sync gates
  - Emits recommendations and intelligence index under `.sync-reports/self-evolution-loop-<timestamp>.json`
- `scripts/run-nightly-elite-gate.sh`
  - Nightly local validation gate (`cargo test -p hermes-cli` + deterministic replay + security gate v2 + performance autopilot + queue audit)
  - Auto-publishes run summary via `contextlattice -> github -> local` sink chain
- `scripts/publish_automation_summary.py`
  - Publishes automation summaries with sink priority:
    - `contextlattice` (default)
    - `github` issue comment fallback
    - `local` file fallback
  - Used by nightly and webhook automation paths for resilient summary reporting
- `compare-adapter-chaos-reports` (`crates/hermes-parity-tools`)
  - Compares chaos reports and fails on attempts/fallback/outcome regressions
- `generate-readme-sync-status` (`crates/hermes-parity-tools`)
  - Regenerates README “Live Upstream Sync Status” block from latest `.sync-reports/upstream-sync-*.txt`
- `hermes-parity-tools generate-parity-dashboard`
  - Builds `docs/parity/PARITY_DASHBOARD.md` from parity JSON artifacts

## One-shot Manual Sync

```bash
bash scripts/sync-upstream.sh --dry-run
bash scripts/sync-upstream.sh
bash scripts/sync-upstream.sh --draft-pr
bash scripts/sync-upstream.sh --draft-pr --pr-labels "upstream-sync,parity-sync,risk-reviewed"
bash scripts/sync-upstream.sh --redteam-cmd "cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate --suite scripts/redteam-cases.json"
cargo run --locked -q -p hermes-parity-tools -- run-adapter-chaos-harness --repo-root .
cargo run --locked -q -p hermes-parity-tools -- run-zero-copy-hotpath-bench --repo-root .
python3 scripts/run-elite-sync-gate.py --repo-root .
cargo run --locked -q -p hermes-parity-tools -- run-upstream-surface-coverage-gate --repo-root . --upstream-ref upstream/main --local-mode worktree
cargo run --locked -q -p hermes-parity-tools -- run-self-evolution-loop --repo-root . --json
cargo run --locked -q -p hermes-parity-tools -- generate-parity-dashboard --repo-root .
cargo run --locked -q -p hermes-parity-tools -- generate-readme-sync-status --repo-root .
bash scripts/sync-upstream.sh --elite-gate
bash scripts/sync-upstream.sh --elite-gate --elite-rollback-cmd "git reset --hard origin/main"
```

Cherry-pick mode for linear upstream replay:

```bash
bash scripts/sync-upstream.sh --strategy cherry-pick
```

Strict risk gate (blocks sync if high-risk paths changed upstream):

```bash
bash scripts/sync-upstream.sh --strict-risk-gate
```

Explicit one-run bypass after manual review:

```bash
bash scripts/sync-upstream.sh --strict-risk-gate --allow-risk-paths
```

## Direct-to-main Mode (No PR)

```bash
bash scripts/sync-upstream.sh --mode direct-main
```

Use this only when you explicitly want automation to push `main` directly.

## Install Cron Schedule

Default schedule is every 6 hours at minute 17:

```bash
bash scripts/install-upstream-sync-cron.sh
```

Custom schedule example (daily at 03:10):

```bash
bash scripts/install-upstream-sync-cron.sh "10 3 * * *"
```

Default log path:
- `~/.hermes/logs/upstream-sync.log`

Default report path:
- `.sync-reports/upstream-sync-<timestamp>.txt`

## Operational Notes

- Requires configured `origin` and `upstream` remotes.
- By default, sync validates that `upstream` points to `NousResearch/hermes-agent`.
  - Override only when intentional: `ALLOW_NON_OFFICIAL_UPSTREAM=1`.
  - Optional override target repo string: `EXPECTED_UPSTREAM_REPO=<owner/repo>`.
- Requires a clean working tree.
- `gh` CLI is optional; without it the script still pushes the sync branch. Conflict issue auto-creation is disabled when `gh` is unavailable.
- Sync PR bodies now include parity queue summary, drift artifact paths, and test guidance for merge reviewers.
- Sync report includes `redteam_report` when adversarial gate runs.
- Sync report includes `elite_report` when consolidated elite gate runs.
- Cron entry exports `REPO_ROOT` explicitly so the wrapper runs against the
  intended repository path.
- On conflicts, the script writes `.sync-reports/upstream-sync-<timestamp>-conflict.txt`.
  - For `--strategy cherry-pick`, it also records rollback tag `rollback/upstream-sync-<timestamp>`.
- Strict risk patterns default file:
  - `scripts/upstream-risk-paths.txt`
- Cron installer defaults to `STRICT_RISK_GATE=1` so unattended syncs pause on sensitive upstream changes.
- Default verification command is:
  - `cargo test -p hermes-gateway`
- Post-sync verification also runs `cargo run --locked -q -p hermes-parity-tools -- check-fork-carry`.
  The [fork carry registry](parity/fork-carry-contracts.v1.json) records the required library tests and known coverage gaps.
  Each required test must run and pass. The sync report records `fork_carry: skipped` when `--no-tests` is used.

## Diverged / Unrelated History Handling

For Hermes Agent Ultra, upstream and local history may diverge enough that direct
merge is either non-linear or unrelated.

Recommended handling for this Rust-first repo:

- Always fetch upstream first (`git fetch upstream main --prune`).
- Use parity queue/gate workflows (`docs/parity/`, `.sync-reports/`) as the
  source of truth for what needs to be ported.
- Prefer controlled branch/PR sync with risk gates over direct unrelated-history
  merges into `main`.
- Treat direct unrelated-history merge attempts as exceptional recovery actions,
  not default upkeep behavior.

## Webhook + Queue Mode

For event-driven sync (push-triggered, not schedule-triggered), see:

- `docs/upstream-webhook-sync.md`

This path supports SQLite (default), SQS, and Kafka queue backends and is the
recommended architecture when you want near-real-time upstream ingestion with
strict risk gating.

## Parity Matrix Snapshot

To generate a reproducible parity snapshot against `upstream/main`:

```bash
cargo run --locked -q -p hermes-parity-tools -- generate-parity-matrix
```

Artifacts are written to:

- `docs/parity/parity-matrix.json`
- `docs/parity/parity-matrix.md`
- `docs/parity/intentional-divergence.json` (tracked divergence registry)

The matrix uses tree-level blob comparison (works with divergent histories) and
`git cherry` patch-id mapping for represented vs missing upstream commits.

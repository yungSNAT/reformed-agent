# Test Coverage Audit

Generated: `2026-11-20T13:04:41.126540+00:00`

## Gate

- Audit gate: **PASS**
- Critical gaps: `0`
- Advisory gaps: `9`

## Summary

| Metric | Value |
| --- | ---: |
| `tracked_behavior_rows` | 511 |
| `covered_behavior_rows` | 33 |
| `tracked_behavior_coverage_ratio` | 0.0646 |
| `rust_test_files` | 496 |
| `rust_test_functions` | 9121 |
| `coverage_manifest_entries` | 501 |
| `coverage_manifest_entries_with_valid_rust_tests` | 23 |
| `coverage_manifest_deferred_entries` | 141 |
| `coverage_manifest_not_rechecked_entries` | 337 |
| `missing_rust_test_refs` | 0 |
| `queue_pending` | 0 |
| `queue_total` | 1 |
| `test_intents_total` | 10 |
| `test_intents_mapped` | 10 |

## Coverage Manifests

| Manifest | Entries | Valid Rust-test entries | Deferred, not covered | Not rechecked at the pin | Referenced Rust tests | Missing refs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `docs/parity/python-test-suite-coverage.json` | 177 | 0 | 46 | 131 | 105 | 0 |
| `docs/parity/hermes-cli-test-coverage.json` | 147 | 1 | 36 | 110 | 98 | 0 |
| `docs/parity/ui-tui-source-coverage.json` | 177 | 22 | 59 | 96 | 68 | 0 |

## Test Intent Domains

| Intent | Classification | Evidence files | Direct test evidence |
| --- | --- | ---: | ---: |
| `gateway-platform-behavior` | `direct_rust_test` | 25 | 22 |
| `tool-runtime-behavior` | `direct_rust_test` | 133 | 108 |
| `cli-command-surface` | `direct_rust_test` | 190 | 59 |
| `agent-loop-and-runtime` | `direct_rust_test` | 117 | 82 |
| `acp-protocol-and-transport` | `direct_rust_test` | 21 | 15 |
| `skills-management-contract` | `direct_rust_test` | 78 | 24 |
| `cron-and-scheduler-runtime` | `direct_rust_test` | 81 | 25 |
| `memory-plugin-integration` | `direct_rust_test` | 12 | 12 |
| `environment-lifecycle-contract` | `direct_rust_test` | 17 | 11 |
| `tool-call-parser-contract` | `direct_rust_test` | 3 | 3 |

## Critical Gaps

- none

## Advisory Gaps

- `coverage_manifest_deferred`: docs/parity/python-test-suite-coverage.json has rows deferred without Rust test coverage
- `coverage_manifest_not_rechecked`: docs/parity/python-test-suite-coverage.json has rows whose upstream file changed after their Rust tests were cited, so they do not count as covered
- `coverage_manifest_deferred`: docs/parity/hermes-cli-test-coverage.json has rows deferred without Rust test coverage
- `coverage_manifest_not_rechecked`: docs/parity/hermes-cli-test-coverage.json has rows whose upstream file changed after their Rust tests were cited, so they do not count as covered
- `coverage_manifest_deferred`: docs/parity/ui-tui-source-coverage.json has rows deferred without Rust test coverage
- `coverage_manifest_not_rechecked`: docs/parity/ui-tui-source-coverage.json has rows whose upstream file changed after their Rust tests were cited, so they do not count as covered
- `nonzero_tree_drift`: max_commits_behind remains nonzero in parity matrix
- `nonzero_tree_drift`: max_upstream_patch_missing remains nonzero in parity matrix
- `nonzero_tree_drift`: max_files_only_upstream remains nonzero in parity matrix

## Completed Sigma Harness Moves

- **Coverage trend ledger**: `docs/parity/harness-trend-ledger.json`, `docs/parity/harness-trend-ledger.md`
- **ContextLattice replay evidence index**: `docs/parity/contextlattice-replay-evidence-index.json`, `docs/parity/contextlattice-replay-evidence-index.md`
- **Cross-version harness budget**: `docs/parity/harness-budget.json`, `docs/parity/harness-budget.md`

## Next Sigma Harness Moves

- none

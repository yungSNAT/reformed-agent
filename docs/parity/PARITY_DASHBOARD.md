# Parity Dashboard

_Generated from source artifacts: `2026-11-18T23:38:18.439998+00:00`_

## Snapshot

- Upstream target: `upstream/parity-reference` @ `ae2a81ccc00e9b7c06301523e5bb09608397149c`
- Workstream snapshot generated: `2026-11-19T10:37:43+11:00`
- Parity matrix generated: `2026-11-18T23:38:12.035165+00:00`
- Queue snapshot generated: `2026-11-18T23:38:18.186789+00:00`
- Proof snapshot generated: `2026-11-18T23:38:18.439998+00:00`

## Gate Status

- Release gate: **FAIL**
- CI/tree-drift gate: **FAIL**
- Test coverage audit: **PASS**
- SOTA harness matrix: **PASS**
- Behavioral similarity diff: **PASS**
- Deep problem-solving diff: **PASS**
- Release gate failures: min_test_coverage_tracked_behavior_ratio (actual=0.728, limit=1.0)
- CI gate failures: max_files_only_upstream (actual=24117.0, limit=2600); min_test_coverage_tracked_behavior_ratio (actual=0.728, limit=1.0)
- CI gate warnings: none

## Test Coverage Audit

| Metric | Value |
| --- | ---: |
| Tracked behavior rows | 511 |
| Covered behavior rows | 372 |
| Tracked behavior coverage ratio | 0.7280 |
| Rust test functions | 8785 |
| Missing Rust test refs | 0 |
| Critical gaps | 0 |

## SOTA Harness Matrix

| Metric | Value |
| --- | ---: |
| Domains total | 3 |
| Domains passing | 3 |
| Domain coverage ratio | 1.0000 |
| Direct Rust tests | 12 |
| Critical gaps | 0 |
| Missing Rust test refs | 0 |

## Behavioral Similarity Diff

| Metric | Value |
| --- | ---: |
| Total cases | 13 |
| Equal or better cases | 13 |
| Superior cases | 8 |
| Similarity ratio | 1.0000 |
| Regressions | 0 |
| Gaps | 0 |
| Unverified cases | 0 |
| Missing Rust test refs | 0 |

## Deep Problem-Solving Diff

| Metric | Value |
| --- | ---: |
| Total cases | 10 |
| Equal or better cases | 10 |
| Superior cases | 10 |
| Deep problem-solving ratio | 1.0000 |
| Regressions | 0 |
| Gaps | 0 |
| Unverified cases | 0 |
| Missing Rust test refs | 0 |

## Queue Summary

| Metric | Value |
| --- | ---: |
| Total commits in queue | 1 |
| Pending | 0 |
| Ported | 0 |
| Superseded | 1 |

## Tree/Patch Drift

| Metric | Value |
| --- | ---: |
| commits_behind | 1 |
| commits_ahead | 1623 |
| upstream_patch_missing | 1 |
| upstream_patch_represented | 0 |
| local_patch_unique | 1352 |
| files_only_upstream | 24117 |
| files_only_local | 2021 |
| files_shared_identical | 565 |
| files_shared_different | 1508 |

## Workstream States

| State | Count |
| --- | ---: |
| complete | 2 |
| in_progress | 5 |

## Workstream Detail

| WS | Title | State |
| --- | --- | --- |
| WS2 | Core runtime parity | in_progress |
| WS3 | Tools/adapters parity | in_progress |
| WS4 | Skills parity | in_progress |
| WS5 | UX parity | in_progress |
| WS6 | Tests and CI parity | in_progress |
| WS7 | Security/secrets/store/webhook parity | complete |
| WS8 | Compatibility and divergence policy | complete |

## Source Artifacts

- `docs/parity/parity-matrix.json`
- `docs/parity/workstream-status.json`
- `docs/parity/upstream-missing-queue.json`
- `docs/parity/global-parity-proof.json`
- `docs/parity/test-coverage-audit.json`
- `docs/parity/sota-harness-matrix.json`
- `docs/parity/behavioral-similarity-diff.json`
- `docs/parity/deep-problem-solving-diff.json`


# Harness Budget

Generated: `2026-11-21T04:53:33.486694+00:00`

Budget gate: **FAIL**

| Budget | Actual | Operator | Limit | Status |
| --- | ---: | --- | ---: | --- |
| `queue_pending` | 0 | `<=` | 0 | PASS |
| `tracked_behavior_coverage_ratio` | 0.0646 | `>=` | 1.0 | FAIL |
| `sota_domain_coverage_ratio` | 1.0 | `>=` | 1.0 | PASS |
| `rust_test_functions` | 16421 | `<=` | 8000 | FAIL |
| `coverage_manifest_entries` | 501 | `<=` | 600 | PASS |
| `sota_workflow_replay_steps` | 9 | `<=` | 12 | PASS |
| `sota_protocol_cases` | 7 | `<=` | 20 | PASS |
| `sota_fault_scenarios` | 6 | `<=` | 16 | PASS |

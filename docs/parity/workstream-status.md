# Workstream Status

- Local HEAD: `7ed11d50979b93d9a87f72f3ad155a3aac08f4a4`
- Upstream: `upstream/parity-reference` (`ae2a81ccc00e9b7c06301523e5bb09608397149c`)

| Workstream | Title | State |
| --- | --- | --- |
| `WS2` | Core runtime parity | **in_progress** |
| `WS3` | Tools/adapters parity | **in_progress** |
| `WS4` | Skills parity | **in_progress** |
| `WS5` | UX parity | **in_progress** |
| `WS6` | Tests and CI parity | **in_progress** |
| `WS7` | Security/secrets/store/webhook parity | **complete** |
| `WS8` | Compatibility and divergence policy | **complete** |

## WS2 — Core runtime parity

- State: **in_progress**
- Live cron backend wired in gateway, app, chat, and ACP runtime paths.
- Runtime tool bridge refreshed from live registry in gateway handlers.
- Metrics: `{"wiring_sites_detected": 1}`

## WS3 — Tools/adapters parity

- State: **in_progress**
- send_message tool wired to live gateway backend in gateway runtime.
- clarify tool wired to channel backend in gateway and stdio backend in CLI runtimes.
- cronjob tool wired to live scheduler backend.
- 9 rows are deferred or not rechecked, so not yet shown to match in Rust (docs/parity/shared-diff-backlog.md and the coverage manifests).
- Metrics: `{"rows_not_shown_matching": 9, "runtime_wiring_functions": 4}`

## WS4 — Skills parity

- State: **in_progress**
- Upstream skills catalogs audited against local tree.
- Intentional divergence documented for skills and optional-skills vendoring.
- Metrics: `{"divergence_documented": false, "local_skill_files": 999, "upstream_skill_files": 1127}`

## WS5 — UX parity

- State: **in_progress**
- Rust CLI/TUI runtime validated through e2e_cli and gateway e2e smoke tests.
- Web/UI upstream trees classified as intentional divergence in Rust-first mode.
- 155 rows are deferred or not rechecked, so not yet shown to match in Rust (docs/parity/shared-diff-backlog.md and the coverage manifests).
- Metrics: `{"divergence_documented": true, "e2e_cli_tests": 16, "rows_not_shown_matching": 155}`

## WS6 — Tests and CI parity

- State: **in_progress**
- CI workflow enforces format, clippy gate, placeholder gate, workspace tests, parity fixture tests.
- 336 rows are deferred or not rechecked, so not yet shown to match in Rust (docs/parity/shared-diff-backlog.md and the coverage manifests).
- Metrics: `{"ci_jobs": 0, "parity_gate_present": true, "rows_not_shown_matching": 336}`

## WS7 — Security/secrets/store/webhook parity

- State: **complete**
- Webhook listener/worker supports sqlite, SQS, Kafka queue backends.
- Launchd setup includes runtime-role/host guards and webhook secret automation.
- Upstream sync script includes strict risk gate.
- Metrics: `{"security_scripts_present": 4}`

## WS8 — Compatibility and divergence policy

- State: **complete**
- Compatibility policy defines rust-native default, bounded FFI fallback, and divergence governance.
- Intentional divergences are codified in docs/parity/intentional-divergence.json.
- Metrics: `{"divergence_items": 23, "policy_exists": true}`


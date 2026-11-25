# OpenHuman P0/P1 Surface Coverage Matrix

## P0 Matrix

| Surface | Primary Entry | Core Runtime Path | Test Coverage |
|---|---|---|---|
| Command palette IA | `/commands` | `handle_commands_catalog_command`, `render_command_catalog` | `test_p0_p1_surface_commands_registered_and_completable` |
| Boot readiness gate | `/boot`, `/boot quick` | `handle_boot_command`, `collect_boot_readiness_checks`, `render_boot_readiness_report` | `p0_walkthrough_and_integrations_commands_emit_expected_sections` (walkthrough+integration flow), plus compile-level coverage of boot handler paths |
| Guided walkthrough | `/walkthrough ...`, `/onboard` | `handle_walkthrough_command`, `walkthrough_steps_for_mode`, state read/write | `p0_walkthrough_and_integrations_commands_emit_expected_sections`, `test_p0_p1_surface_commands_registered_and_completable` |
| Layered compression UX | `/compress rules ...` | `handle_compress_rules_command`, merged policy apply, env projection | `p0_compress_rules_set_and_apply_updates_runtime_env`, `test_format_tool_message_lines_truncates_large_payload` |
| Coverage gate discipline | `cargo run --locked -q -p hermes-parity-tools -- run-openhuman-p0-p1-gate` | static checks + targeted tests | script itself + targeted `cargo test` pass |

## P1 Matrix

| Surface | Primary Entry | Core Runtime Path | Test Coverage |
|---|---|---|---|
| Trigger triage | `/triage ...` | `evaluate_trigger_triage`, `handle_trigger_triage_command` | `p1_trigger_triage_escalates_high_severity_events` |
| Subconscious queue | `/subconscious ...` | `handle_subconscious_command`, queue state read/write | integrated through command compile/tests and slash surface registration checks |
| OAuth runtime gate | `/auth status|verify|refresh` | `oauth_runtime_gate_for_provider`, `version_at_least`, auth command guards | covered through auth path compile/tests and runtime command checks |
| Integration health IA | `/integrations ...` | `handle_integrations_command` | `p0_walkthrough_and_integrations_commands_emit_expected_sections` |

## Gate Command

```bash
cargo run --locked -q -p hermes-parity-tools -- run-openhuman-p0-p1-gate
```


# OpenHuman P0/P1 Surfaces Runbook

This runbook covers the P0/P1 surfaces added for Hermes Agent Ultra.

Follow-on tranche docs:

- `docs/implementation/openhuman-p2a-p2b-runbook.md`
- `docs/implementation/openhuman-p2a-p2b-surface-matrix.md`

## P0 Operator Surfaces

### 1) Command Palette IA
- `/commands`
- `/commands search <term>`

What it does:
- Prints a categorized command catalog with quick search so operators can discover control-plane surfaces quickly.

### 2) Boot Readiness Gate
- `/boot` (full)
- `/boot quick`

What it does:
- Runs a startup readiness gate with `PASS/WARN/FAIL` checks for home/config/auth/tooling/memory connectivity.
- Includes per-check remediation guidance.

### 3) Guided Walkthrough
- `/walkthrough start quick`
- `/walkthrough start full`
- `/walkthrough next`
- `/walkthrough done <step-id>`
- `/walkthrough status`
- `/walkthrough reset`
- `/onboard` (alias)

What it does:
- Persists onboarding progress in `~/.hermes-agent-ultra/walkthrough/state.json`.
- Enables deterministic first-run and re-onboarding flows.

### 4) Layered Compression Rules
- `/compress rules status`
- `/compress rules preview`
- `/compress rules set user <key> <value>`
- `/compress rules set project <key> <value>`
- `/compress rules apply`
- `/compress rules clear user|project`

What it does:
- Maintains builtin + user + project planes and computes merged render policy.
- Applies merged values into runtime env for TUI rendering bounds.

## P1 Operator Surfaces

### 1) Integration Health IA
- `/integrations status`
- `/integrations all`
- `/integrations auth|providers|gateway|memory`

What it does:
- Panelized integration health with provider/auth/runtime gates and memory/gateway visibility.

### 2) Trigger Triage
- `/triage status`
- `/triage list`
- `/triage eval <source> <payload>`
- `/triage queue <source> <payload>`

What it does:
- Scores incoming event severity and decides `drop|notify|agent-run|escalate`.
- Escalations can be parked into the subconscious queue with approval required.

### 3) Subconscious Queue
- `/subconscious status`
- `/subconscious add <prompt>`
- `/subconscious approve <id>`
- `/subconscious reject <id>`
- `/subconscious run [n]`
- `/subconscious clear`

What it does:
- Persists asynchronous background candidate tasks at `~/.hermes-agent-ultra/subconscious/queue.json`.
- Applies risk/approval policy before execution.

### 4) OAuth Runtime Gate
- Integrated into `/auth status`, `/auth verify`, and `/auth refresh`.

What it does:
- Ensures runtime version satisfies minimum provider OAuth gate before completing OAuth-bound commands.

## Local Acceptance Gate

Run:

```bash
cargo run --locked -q -p hermes-parity-tools -- run-openhuman-p0-p1-gate
```

This validates:
- slash registrations
- presence of tranche-critical tests
- execution of targeted tests

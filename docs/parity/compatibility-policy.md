# Rust Parity Compatibility Policy

## Purpose
This policy defines how `hermes-agent-ultra` maintains parity with `NousResearch/hermes-agent`
while keeping Rust-first implementation ownership.

## Decision Rules
1. Default rule: implement required behavior in Rust.
2. If Rust implementation is blocked, FFI fallback is allowed only with explicit rationale and
   a follow-up Rust-native migration issue.
3. Python/TS runtime file imports are not used as the parity mechanism.
4. Upstream Python test-only surfaces (`tests/`, `test/`) are `superseded` by default under
   Rust-only parity guard and are not opened as actionable parity implementation work unless
   explicitly approved.
5. Divergences must be declared in `docs/parity/intentional-divergence.json`.
6. Upstream commit deltas are processed through the parity upkeep queue before release.

## Compatibility Tiers
1. `rust-native`: feature implemented and validated in Rust crates.
2. `rust-native+ultra`: Rust parity plus intentional ultra extension.
3. `ffi-temporary`: temporary bridge pending Rust replacement.
4. `intentional-divergence`: upstream behavior intentionally not mirrored; rationale required.

## WS Mapping
1. WS2: core runtime parity in Rust (`crates/hermes-agent`, `crates/hermes-cli`, config/runtime wiring).
2. WS3: tool/adapter runtime parity (live backends, platform wiring).
3. WS4: skills parity strategy (catalog governance and divergence classification).
4. WS5: UX parity strategy (CLI/TUI-first surface and explicit web divergence policy).
5. WS6: test/CI parity gate.
6. WS7: security/secrets/store/webhook hardening parity.
7. WS8: this compatibility contract and auditability rules.

## Gates
Both gates are functional: they check what the Rust port does, never how far the upstream
tree has moved. Upstream tree drift (commits behind, missing patches, upstream-only files) is
reported against `drift_alert_thresholds` in the release readiness summary and gates nothing.
The limits live in `docs/parity/global-parity-thresholds.json`.

1. Pull request gate (`ci_thresholds`): no functional regression. It checks divergence
   ownership and review dates, test-intent mapping, coverage audit integrity, and the SOTA,
   behavioural and deep problem-solving harnesses. Rows deferred or not rechecked at the pin do
   not fail it; `global_parity_governance` ratchets their counts so they can only fall.
2. Release gate (`release_thresholds`): every pull request check at the same or a stricter
   limit, plus every tracked row shown matching (coverage ratio 1.0, no deferred shared-diff
   rows, no rows not rechecked at the pin), an empty upstream queue and GPAR-01 to GPAR-11
   complete.

`global_parity_governance` fails if either gate holds a drift metric or if the release gate is
less strict than the pull request gate on any shared check.

A parity release also requires:
1. The `docs/parity/` artefacts regenerated against the pinned revision in
   `docs/parity/upstream-reference.txt`.
2. `docs/parity/workstream-status.json` showing complete status for WS2-WS8.
3. Any non-`rust-native` item documented with rationale and owner.

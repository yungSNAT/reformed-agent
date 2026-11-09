# Contributing to Reformed Agent

Reformed Agent is a fork of Hermes Agent Ultra, a Rust-first parity and reliability fork of `NousResearch/hermes-agent`. Contributions should preserve that contract: port upstream behavior intentionally, keep Ultra-only and Reformed-only extensions explicit, and avoid placeholder runtime paths.

## Getting Started

```bash
git clone https://github.com/yungSNAT/reformed-agent.git
cd reformed-agent
cargo build --workspace
cargo test --workspace
```

Install the local CLI while testing end-to-end behavior:

```bash
cargo install --path crates/hermes-cli --force
hermes --version
hermes doctor --deep --snapshot
```

## Development Workflow

- Branch from `main` and open a pull request for repository changes.
- Keep PRs scoped to one feature, fix, parity port, or release-prep concern.
- Follow existing Rust formatting and module layout before introducing new crates or top-level dependencies.
- Use `tracing::{debug, info, warn, error}` for runtime logs; reserve direct stdout for user-facing CLI output.
- Never commit secrets, provider keys, private prompts, or live operator traces.

## Completeness Standard

Do not submit stubs or partially implemented runtime behavior. If a feature cannot be completed in the PR, leave it out or guard it behind a documented, tested error path. Before requesting review, run:

```bash
bash scripts/check-runtime-placeholders.sh
rg -n -i "not yet implemented|TODO: implement|unimplemented!\(|todo!\(|stub-only|placeholder" crates scripts
```

Legitimate future-work notes belong in docs or issues, not in callable runtime branches that pretend to work.

## Required Checks

Run the narrowest relevant tests while developing, then these release-grade checks before merge when the change affects runtime behavior:

```bash
cargo fmt --all --check
bash scripts/clippy-warning-gate.sh --check
bash scripts/check-runtime-placeholders.sh
cargo test --workspace
git fetch --no-tags --depth=1 upstream "+$(cat docs/parity/upstream-reference.txt):refs/remotes/upstream/parity-reference"
python3 scripts/run-upstream-slash-parity-gate.py --upstream-ref upstream/parity-reference
python3 scripts/run-upstream-surface-coverage-gate.py --repo-root . --upstream-ref upstream/parity-reference --local-ref HEAD
```

`bash scripts/run-repo-ci.sh` runs all of these and the governance checks in one pass. The compiler is the version pinned in `rust-toolchain.toml`.

The parity gates compare against the upstream revision recorded in `docs/parity/upstream-reference.txt`, not against upstream `main`, so upstream's daily commits cannot fail an unrelated change. To move the pin, change that file in its own PR together with the ports and regenerated `docs/parity/` artifacts the new revision needs.

If your change touches parity governance artifacts, regenerate and commit the affected files under `docs/parity/`.

## Parity Contributions

For upstream parity ports, inspect the upstream source first, then implement the Rust equivalent in the relevant crate. Add or update tests and fixtures when behavior is comparable. Keep intentional divergence documented through the parity queue and surface-coverage allowlists rather than hiding it in code comments.

## Release Contributions

Release PRs should update release notes under `docs/releases/`, keep `Cargo.toml` workspace version accurate, pass the checks above, and tag only from a clean `main` after CI passes.

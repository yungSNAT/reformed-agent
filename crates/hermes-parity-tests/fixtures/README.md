# Parity fixtures

## Layout

- **`registry.json`** — lists **active** golden modules only.
- **`<module>/*.json`** — one file per focus area; each file has `schema_version`, `cases[]`.
- **`pending/`** — excluded from `run_all_active_fixtures` (scaffolding / future work).

## Case schema

Each case may include:

- `id`, `op`, `input`, `expected`
- **`skip`: `true`** — ignored by the harness (placeholder until implementations exist).

## Running

```bash
cargo test -p hermes-parity-tests
```

Recursive run over all non-`pending` JSON: `hermes_parity_tests::run_all_active_fixtures`.

## Recording

```bash
cargo run -p hermes-parity-tests --bin record_fixtures
```

Works without a Python checkout for checkpoint-manager pure helpers. With a Hermes Python checkout at `../hermes-agent` beside this repository, it also records the Anthropic adapter cases by calling upstream's `agent.anthropic_adapter` through `PARITY_PYTHON` (default `python3`).

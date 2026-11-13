# Reformed Agent

Rust-first autonomous agent runtime with functional parity goals against `NousResearch/hermes-agent`. Reformed Agent is a fork of [Hermes Agent Ultra](https://github.com/sheawinkler/hermes-agent-ultra) and keeps its reliability, security and operator-control layer. It installs as Rust binaries and needs no Python or Node.js runtime to run the agent, whilst upstream's source installer adds Python 3.14, Node.js, npm, pnpm, ripgrep and FFmpeg; tools that call another program, such as FFmpeg for video or `npx` for some MCP servers, still need it.

Translations of an earlier README, which predate this fork: [中文](./README_ZH.md), [日本語](./README_JA.md), [한국어](./README_KO.md).

## Status

- **Base:** Hermes Agent Ultra [`v0.22.0`](./docs/releases/v0.22.0.md) at commit `c002f8c4`.
- **Releases:** none published from this fork yet. Build from source (see [Install](#install)).
- **Names:** the binaries keep Ultra's names, `hermes-agent-ultra` and `hermes-ultra`, and its config home, `~/.hermes-agent-ultra`.
- **Parity:** measured against the upstream revision pinned in [`docs/parity/upstream-reference.txt`](./docs/parity/upstream-reference.txt) (`f64e4f4f`, 8 July 2026). The slash-command and surface gates pass at that revision.
- **Drift:** upstream has moved since the pin. The daily [Parity Audit](https://github.com/yungSNAT/reformed-agent/actions/workflows/parity-audit.yml) run reports the current gap against upstream `main` in its job summary. Moving the pin forward is the next parity work.
- **Windows:** hosted CI only cross-builds for Windows. The tests of `hermes-config`, `hermes-cron`, `hermes-environments` and `hermes-tools` pass in a local Windows run (`cargo test -p hermes-config -p hermes-cron -p hermes-environments -p hermes-tools`); the other crates' tests have not been run on Windows. Local shell commands and cron scripts need a POSIX `sh` from Git for Windows or MSYS2 on PATH. They run without shell init files.
- **Footprint:** measured on 13 November 2026 against Hermes Agent v0.23.1 (see the table below).

| Measure | Reformed Agent at `5cdde8fd` | Hermes Agent v0.23.1 (`3b421578`) |
| --- | --- | --- |
| Installed size | 59.7 MB binary (21.0 MB as the release `.tar.gz`) | 901.6 MB install directory, 2.68 GB home after install |
| `--version`, cold page cache | 35.5 ms, 13.9 MiB peak RSS | 881 ms, 36.5 MiB peak RSS |
| `--version`, warm | 4.6 ms, 13.9 MiB | 353 ms, 37.0 MiB |
| `doctor`, cold page cache | 55.0 ms, 14.2 MiB | not comparable (see below) |
| `doctor`, warm | 9.2 ms, 14.3 MiB | not comparable |

Method:

- Machine: one x86_64 Linux machine (AMD Ryzen 7 4800H, 4 CPU threads available to the run, 31 GiB RAM) running CachyOS with Linux 7.3.3 and rustc 1.99.1. It was not idle: the load average was 2.46 before the runs and 2.35 after.
- Network: none. Both tools ran in a network namespace with no interfaces up, each in its own scratch home (empty before the first run for Reformed, as the installer left it for Hermes), with no account or key.
- Builds: Reformed was built with `cargo build --locked --release -p hermes-cli --bin hermes-agent-ultra`. Hermes was installed by its own installer (`install.sh --non-interactive --skip-browser --skip-computer-use`).
- Figures: wall-clock medians of 5 cold runs (page cache dropped before each) and 10 warm runs. Peak RSS is the largest resident set of any process in the run.
- Hermes `doctor`: it runs 44 network connectivity checks, so offline it took about 95 s and exited 1. That is not a start-up time.

These are start-up and size figures from 1 machine, not a general speed comparison.

Repo-local CI is the authoritative gate for development and release confidence.
GitHub Actions are a hosted mirror of the repo contract when they run; they are
not required as the source of truth.

```bash
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} bash scripts/run-repo-ci.sh
```

The Rust toolchain is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml). See [docs/demo.md](./docs/demo.md) for the one-command demo/readiness path.

## What Reformed Agent Changes

- `Reformation skin`: the default terminal theme, a palette for a pure black canvas
- `Hermes history import`: `hermes-ultra import-hermes-sessions <export.jsonl> --profile-dir <dir>` converts a Hermes JSONL session export into resumable, searchable sessions in a new profile
- `Tool guard`: `security.tool_guard` names a program that speaks Claude Code's PreToolUse hook format; when it is set, local terminal commands, `write_file`, `patch` and cron scripts ask it first and run only on a clean allow (see [security.md](./website/docs/user-guide/security.md))
- `Stream lifetime cap`: `agent.stream_max_lifetime`, ported from Hermes
- `Pinned parity reference`: the parity gates compare against one recorded upstream revision, so a day's upstream commits cannot fail an unrelated change
- `Provider routing`: retries keep the configured endpoint and provider-specific credentials. Vertex AI supports service-account credentials and application default credentials.
- `Gateway replies`: API replies keep the final answer after tool progress messages and handle streaming previews and UTF-8 chunks.
- `Browser tools`: local and cloud browsers use Chrome DevTools Protocol, with HTTP discovery for cloud sessions and screenshot analysis through the configured vision model.
- `Windows`: cron scripts and local shell commands find `sh` through the inherited PATH, cron refuses rooted and drive-relative script paths, text-to-speech command providers run without a shell and debug builds of the CLI no longer overflow the stack.
- `Container image`: the Dockerfile builds with the pinned toolchain and the image runs as an unprivileged user.
- `Lint gate`: `cargo clippy --workspace --all-targets -- -D warnings` passes on Linux and Windows, and the warning allowlist is empty.

Everything below this section is inherited from Ultra and describes the runtime this fork builds on.

## What You Get

- Fully Rust-native core runtime (agent loop, tools, gateway, skills, CLI/TUI)
- Multi-provider inference routing and OAuth-capable provider flows
- First-class local/self-host backends: Ollama, llama.cpp/llamafile, vLLM, MLX, Apple ANE endpoint, SGLang, TGI, LM Studio, LMDeploy, LocalAI, KoboldCpp, text-generation-webui, TabbyAPI
- Tool runtime with policy enforcement, MCP integration, cron, and memory backends
- Parity upkeep system for upstream drift triage and controlled roll-forward
- Production operations surface (`doctor`, replay traces, sync gates, parity artifacts)

## Why Ultra Exists

`NousResearch/hermes-agent` is the canonical upstream product surface.  
Hermes Agent Ultra keeps that surface in scope while focusing on:

- deterministic Rust execution paths
- explicit safety and policy controls
- better observability and incident debugging
- easier operator workflows for long-running local and gateway sessions

## Differentiation vs Upstream

Ultra keeps parity work separate from intentional extensions.

- `Runtime policy engine`: enforce/audit/simulate tool policy decisions at runtime
- `Session branching + time-travel`: checkpoint/rollback/replay navigation from the TUI
- `Tool-call simulator`: preview policy allow/deny outcomes before running risky tool invocations
- `Adaptive repo-review budget controls`: tune discovery-loop trimming live (`balanced/aggressive/relaxed/off`)
- `Semantic repo graph`: inspect dependency hubs/edges with inline Mermaid preview
- `Provider QoS router controls`: inspect route learning/health and apply autotune from chat
- `Live session eval harness`: score real saved sessions and gate quality trends from actual usage
- `RTK raw-mode controls`: inspect unwrapped tool payloads when debugging integrations
- `Memory fusion`: ContextLattice + external memory providers with scoring/fusion logic
- `One-true-harness cockpit`: `/harness` and `harness_cockpit` unify skills, proof, OIDC, replay, objectives, onboarding, and chaos probes
- `Ultra autonomy cockpit`: `ultra_autonomy`, `/harness autonomy`, and `harness.autonomy` expose task boards, loop detection, resource-governed subagents, ContextLattice-first memory lifecycle, channel surfaces, and objective-to-board planning
- `One-command always-on UX`: `hermes-ultra up` composes gateway service install/start/status into a single operator command
- `Advanced sync gates`: differential parity checks, red-team/adversarial gating, elite sync gate
- `Operational tooling`: deep doctor snapshots, replay traces, queue-based upstream webhook sync
- `Rust-only implementation strategy`: parity in Rust first; no direct Python runtime vendoring

## Install

### From source

```bash
cargo install --git https://github.com/yungSNAT/reformed-agent hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra
```

This installs `hermes-agent-ultra` and `hermes-ultra` only, leaving any existing
`hermes` command from upstream NousResearch Hermes untouched. Run setup
afterwards with `hermes-ultra setup`.

### From a checkout

```bash
git clone https://github.com/yungSNAT/reformed-agent.git
cd reformed-agent
bash scripts/install.sh
```

The installer looks for a release asset first. This fork has published none, so
in a checkout with `cargo` available it builds from source instead. Set a custom
install path with `INSTALL_DIR=/usr/local/bin`, and pass `--setup` to run the
setup wizard after installing. Ultra's one-line `curl | bash` install needs a
published release, so it does not work for this fork yet.

## Quick Start

Need a shorter path? See [README_QUICKSTART.md](./README_QUICKSTART.md).

Setup:

```bash
hermes-ultra setup
```

Mem0 memory setup supports cloud, self-hosted, and OSS-style endpoints:

```bash
hermes-ultra memory setup mem0 --mode selfhosted --host http://127.0.0.1:24220 -y
```

Add `--api-key ...` only when the self-hosted server requires auth; Hermes writes the host to `mem0.json` and the secret to `$HERMES_HOME/.env`.

Interactive session:

```bash
hermes-ultra
```

Interactive mode is single-instance per Hermes home by default (prevents accidental parallel TUI sessions sharing the same state).  
If you intentionally want parallel interactive sessions, run:

```bash
HERMES_ALLOW_PARALLEL_INTERACTIVE=1 hermes-ultra
```

One-shot query:

```bash
hermes-ultra chat --query "summarize this repository"
```

Gateway mode, in the foreground. Without chat platforms the gateway serves only cron jobs and HTTP webhooks; `hermes-ultra gateway setup` enables platforms:

```bash
hermes-ultra gateway run
```

## Skip API-key collection With Nous Portal

Hermes Agent Ultra still supports direct provider and per-tool keys. If you prefer one managed subscription for model access plus hosted tool backends, [Nous Portal](https://portal.nousresearch.com) can cover:

- 300+ models, selectable with `/model <name>`.
- Tool Gateway routing for web search, image generation, text-to-speech, and cloud browser backends.

Fresh install path:

```bash
hermes-ultra setup --portal
```

That starts Nous OAuth setup, sets Nous as the provider, and enables Tool Gateway routing. Inspect the current state with:

```bash
hermes-ultra portal info
```

You can still bring your own keys for individual tools; gateway routing is per backend, not all-or-nothing.

Deep diagnostics bundle:

```bash
hermes-ultra doctor --deep --snapshot --bundle
```

Optional Sentrux MCP profile:

Full MCP guide: [docs/mcp.md](./docs/mcp.md)

```bash
hermes-ultra mcp sentrux
hermes-ultra mcp sentrux-status
```

Key operator commands:

```bash
# Capability diagnostics for current or target model
/model explain
/model why-not --cap tools,reasoning --min-context 200000
/swarm status
/swarm plan graph
/swarm run 4 sequential

# Deterministic trace controls
/raw trace status
/raw trace verify
/raw trace export 200

# Runtime policy packs
/policy list
/policy strict
/policy standard
/policy dev

# Adaptive intelligence-performance autopilot
/ops autopilot status
/ops autopilot run
/ops autopilot recommend
/ops autopilot apply

# OpenHuman-derived P0/P1 operator control-plane
/commands search boot
/boot quick
/boot profile prod
/walkthrough start quick
/walkthrough insights
/integrations status
/integrations repair
/integrations snapshot
/triage eval webhook "secret leak panic outage"
/triage feedback webhook critical "secret leak panic outage"
/subconscious status
/subconscious profile strict
/subconscious run 2 --dry-run
/compress rules recommend
/compress rules autotune apply user

# Session time-travel + simulation
/timetravel list
/timetravel goto <snapshot>
/simulate terminal {"cmd":"ls -la"}

# QoS + eval runtime surfaces
/qos status
/qos health
/ops budget balanced
/ops eval run
```

## Local Backends

`hermes-ultra setup` now includes local/self-host provider options with no mandatory API key:

- `ollama-local` (default `http://127.0.0.1:11434/v1`)
- `llama-cpp` (default `http://127.0.0.1:8080/v1`)
- `vllm` (default `http://127.0.0.1:8000/v1`)
- `mlx` (default `http://127.0.0.1:8080/v1`)
- `apple-ane` (default `http://127.0.0.1:8081/v1`)
- `sglang` (default `http://127.0.0.1:30000/v1`)
- `tgi` (default `http://127.0.0.1:8082/v1`)
- `lmstudio` (default `http://127.0.0.1:1234/v1`)
- `lmdeploy` (default `http://127.0.0.1:23333/v1`)
- `localai` (default `http://127.0.0.1:8080/v1`)
- `koboldcpp` (default `http://127.0.0.1:5001/v1`)
- `text-generation-webui` (default `http://127.0.0.1:5000/v1`)
- `tabbyapi` (default `http://127.0.0.1:5000/v1`)

Override endpoint URLs via env vars:

- `OLLAMA_BASE_URL`
- `LLAMA_CPP_BASE_URL`
- `VLLM_BASE_URL`
- `MLX_BASE_URL`
- `APPLE_ANE_BASE_URL`
- `SGLANG_BASE_URL`
- `TGI_BASE_URL`
- `LMSTUDIO_BASE_URL`
- `LMDEPLOY_BASE_URL`
- `LOCALAI_BASE_URL`
- `KOBOLDCPP_BASE_URL`
- `TEXT_GENERATION_WEBUI_BASE_URL`
- `TABBYAPI_BASE_URL`

Detailed guide: [docs/local-backends.md](./docs/local-backends.md)

## Built-In Context + Memory Behavior

Ultra auto-loads high-value project and persona context:

- `SOUL.md`
- `AGENTS.md`
- `DESIGN.md`
- `.hermes.md` / `HERMES.md`
- `MEMORY.md` / `USER.md`

Subdirectory discovery is enabled so context follows the code path being edited.

## Skills and Registry Surface

Skills commands support multi-registry search/install and local tap flows.

- Default GitHub skill taps include OpenAI, Anthropic, VoltAgent, Matt Pocock,
  Addy Osmani, Google, Obsidian, design-skill, Loopy, GitHub Copilot, gstack,
  and MiniMax roots where those repos expose a clear skill directory.
- Registry-aware installs include:
  - `official/...`
  - `skills.sh/...`
  - `github/...`
  - `lobehub/...`
  - `clawhub/...`
  - `claude-marketplace/...`
- Mandatory skill security scanning runs before install and before use.

Use `/harness` in the TUI or the `harness_cockpit` tool for the curated
one-true-harness index, including Matt Pocock `teach`, `domain-modeling`,
`grill-with-docs`, `codebase-design`, and architecture-deepening workflows.
Use `/harness autonomy` or the `ultra_autonomy` tool for board execution,
loop detection, resource admission, ContextLattice-first memory lifecycle, and
objective-to-board planning. Dashboard clients can call `harness.autonomy` via
`/v1/rpc`; see [Ultra Autonomy Surfaces](./docs/ultra-autonomy-surfaces.md).

## Dashboard OIDC

`hermes-http` supports a Rust-native dashboard OIDC mode while preserving
`HERMES_HTTP_API_KEY` bearer access for machine clients.

Minimum OIDC env:

```bash
HERMES_DASHBOARD_AUTH_PROVIDER=oidc
HERMES_DASHBOARD_OIDC_ISSUER=https://issuer.example
HERMES_DASHBOARD_OIDC_CLIENT_ID=hermes-dashboard
HERMES_DASHBOARD_SESSION_SECRET='replace-with-strong-random-secret'
```

Optional env includes `HERMES_DASHBOARD_OIDC_CLIENT_SECRET`,
`HERMES_DASHBOARD_OIDC_REDIRECT_URI`, `HERMES_DASHBOARD_OIDC_SCOPES`,
`HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS`, `HERMES_DASHBOARD_OIDC_ALLOWED_DOMAINS`,
`HERMES_DASHBOARD_COOKIE_SECURE`, and explicit authorization/token/JWKS endpoint
overrides.

OpenHuman runbooks and matrices:
- `docs/implementation/openhuman-p0-p1-runbook.md`
- `docs/implementation/openhuman-p0-p1-surface-matrix.md`
- `docs/implementation/openhuman-p2a-p2b-runbook.md`
- `docs/implementation/openhuman-p2a-p2b-surface-matrix.md`
- `docs/implementation/openhuman-p3-swarms-runbook.md`
- `docs/implementation/openhuman-p3-swarms-surface-matrix.md`

## Security Posture

- Skill content security scanning blocks dangerous patterns and restricted URL targets
- Skill guard modes: `strict` (default), `relaxed` (only blocks destructive `rm` ops), `off`
- Policy-controlled tool execution modes: `off`, `audit`, `simulate`, `enforce`
- Tool policy presets: `strict`, `balanced`, `dev`, `relaxed`
- Sensitive field redaction in traces/log surfaces
- Guardrails for path traversal, unsafe file ops, and runtime boundary violations

Operator runtime overrides (env):

- `HERMES_SKILL_GUARD_MODE=relaxed`
- `HERMES_SKILL_LOAD_GUARD_MODE=relaxed` (scan mode for skills already on disk when loaded for use; `strict`, `relaxed` or `off`, unknown values mean `strict`; when unset, loading follows `HERMES_SKILL_GUARD_MODE`, which still governs install and creation)
- `HERMES_TOOL_POLICY_PRESET=relaxed`
- `HERMES_MAX_TURNS_UNLIMITED=1` (or set `max_turns: 0` in config/profile)
- `HERMES_FORCE_RUNTIME_AUTH_REFRESH=1`
- `HERMES_AUTH_REFRESH_MAX_RETRIES=6`

## Upstream Sync and Parity Upkeep

Ultra uses controlled sync workflows, not blind merges.

- Upstream source of truth: `NousResearch/hermes-agent`
- Fetch/sync tooling:
  - `scripts/sync-upstream.sh`
  - `scripts/upstream_webhook_sync.py`
- Parity artifacts:
  - `docs/parity/`
  - `.sync-reports/`

Note: this repository intentionally tracks parity via queue/gate workflows because upstream and ultra history can diverge materially.

## Contributing

Interested in helping? Start with [CONTRIBUTING.md](./CONTRIBUTING.md) for setup, PR expectations, parity rules, and the no-stub completeness gate.

## Official References and Attribution

Canonical/official upstream references:

- Upstream (official): https://github.com/NousResearch/hermes-agent
- Ultra (the project this repository forks): https://github.com/sheawinkler/hermes-agent-ultra
- Ultra fork archive (historical): https://github.com/sheawinkler/hermes-agent-rs-fork
- Reformed Agent (this repository): https://github.com/yungSNAT/reformed-agent

Integrated ecosystem references used in Ultra workflows:

- OpenAI skills repository: https://github.com/openai/skills
- Anthropic skills repository: https://github.com/anthropics/skills
- VoltAgent skills aggregation: https://github.com/VoltAgent/awesome-agent-skills
- Ratatui (TUI foundation): https://github.com/ratatui/ratatui
- tui-textarea (composer/editor behavior): https://github.com/rhysd/tui-textarea

Additional ownership, provenance, and credit notes are maintained in [UPSTREAM_ATTRIBUTION.md](./UPSTREAM_ATTRIBUTION.md).

## Architecture Map

Primary Rust workspace crates:

- `crates/hermes-agent`: agent loop, memory orchestration, provider control
- `crates/hermes-tools`: tool registry and execution backends
- `crates/hermes-cli`: CLI/TUI, setup, model/personality switching, operator commands
- `crates/hermes-gateway`: gateway adapters and live runtime paths
- `crates/hermes-skills`: skill storage, guardrails, hub and registry pathways
- `crates/hermes-mcp`: MCP transport/client/server support
- `crates/hermes-config`: config model and runtime loading
- `crates/hermes-telemetry`: tracing and metrics surfaces

## License

Distributed under this repository's license and notices.  
See [LICENSE](./LICENSE), [NOTICE](./NOTICE), and [UPSTREAM_ATTRIBUTION.md](./UPSTREAM_ATTRIBUTION.md).

<sub>Post tenebras lux.</sub>

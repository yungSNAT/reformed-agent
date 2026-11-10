---
sidebar_position: 2
title: "Installation"
description: "Install Hermes Agent Ultra on Linux, macOS, WSL2, or Android via Termux"
---

# Installation

This fork has published no release yet, so installing means building from
source. You need Git and a Rust toolchain (see [Prerequisites](#prerequisites)).

## Quick Install

### Linux / macOS / WSL2 / Android (Termux)

Run the installer from a checkout:

```bash
git clone https://github.com/yungSNAT/reformed-agent.git
cd reformed-agent
bash scripts/install.sh
```

That installs `hermes-agent-ultra` plus the shorter `hermes-ultra` command.
It does **not** install or overwrite a legacy `hermes` command unless you opt in
with `INSTALL_LEGACY_ALIAS=true`, so upstream `NousResearch/hermes-agent` and
Hermes Agent Ultra can coexist on the same machine.

Run the first setup flow explicitly when you want it:

```bash
hermes-ultra setup
```

Or opt into setup during install:

```bash
bash scripts/install.sh --setup
```

:::tip Windows users
Hermes Agent Ultra does not ship the upstream Python PowerShell installer.
Use WSL2 and run the POSIX installer above from a checkout, or install from
source with Rust using the Cargo command below.
:::

### What the Installer Does

The installer looks for a published GitHub Release binary for your platform
first. With no release asset available, it builds from source with Cargo, which
works only when run from a checkout with `cargo` on your PATH. It then:

- installs the canonical `hermes-agent-ultra` binary
- creates the `hermes-ultra` symlink
- leaves any existing `hermes` command untouched by default
- creates `~/.hermes/SOUL.md` on first install
- runs bounded post-install probes when setup is explicitly requested

#### Install Layout

Where the installer puts binaries depends on whether you're installing as a
normal user, root, or Termux:

| Installer | Binary location | Primary command | Data directory |
|---|---|---|---|
| Per-user (normal) | `~/.local/bin/hermes-agent-ultra` | `~/.local/bin/hermes-ultra` | `~/.hermes/` |
| Root-mode (installer run as root) | `/usr/local/bin/hermes-agent-ultra` | `/usr/local/bin/hermes-ultra` | `/root/.hermes/` or `$HERMES_HOME` |
| Termux | `$PREFIX/bin/hermes-agent-ultra` | `$PREFIX/bin/hermes-ultra` | `~/.hermes/` |

Set `HERMES_INSTALL_DIR` or pass `--dir` to choose a different binary
directory. Set `HERMES_HOME` or pass `--hermes-home` to choose a different data
directory.

### After Installation

Reload your shell and start chatting:

```bash
source ~/.bashrc   # or: source ~/.zshrc
hermes-ultra       # Start chatting!
```

To reconfigure individual settings later, use the dedicated commands:

```bash
hermes-ultra model          # Choose your LLM provider and model
hermes-ultra tools          # Configure which tools are enabled
hermes-ultra gateway setup  # Set up messaging platforms
hermes-ultra config set     # Set individual config values
hermes-ultra setup          # Or run the full setup wizard to configure everything at once
```

---

## Prerequisites

Every install currently builds from source, so install the Rust toolchain and
Git first. In a checkout, rustup uses the toolchain pinned in
`rust-toolchain.toml`.

:::info
Hermes Agent Ultra is Rust-first. Do not install the app from PyPI; use the
installer from a checkout or the Cargo source install.
:::

:::tip Nix users
If you use Nix (on NixOS, macOS, or Linux), there's a dedicated setup path with a Nix flake, declarative NixOS module, and optional container mode. See the **[Nix & NixOS Setup](./nix-setup.md)** guide.
:::

---

## Manual / Developer Installation

If you want to clone the repo and install from source — for contributing,
running from a specific branch, or auditing the full build — use Cargo:

```bash
cargo install --git https://github.com/yungSNAT/reformed-agent hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra
```

For contributor workflows, see the [Development Setup](../developer-guide/contributing.md#development-setup) section in the Contributing guide.

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `hermes-ultra: command not found` | Reload your shell (`source ~/.bashrc`) or check PATH |
| `API key not set` | Run `hermes-ultra model` to configure your provider, or `hermes-ultra config set OPENROUTER_API_KEY your_key` |
| Missing config after update | Run `hermes-ultra config check` then `hermes-ultra config migrate` |

For more diagnostics, run `hermes-ultra doctor` — it will tell you exactly what's missing and how to fix it.

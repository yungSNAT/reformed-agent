# Reformed Agent Quickstart

This guide is the fastest path from install to a working interactive session.

## 1) Install

From source:

```bash
cargo install --git https://github.com/yungSNAT/reformed-agent hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra
hash -r
```

Add `--bin hermes` only if you also want the `hermes` command; it replaces one
installed by upstream Hermes.

## 2) Run Setup Wizard

```bash
hermes-ultra setup
```

Recommended wizard flow:

1. Choose `Quick setup` (or `Full setup` if you want platform/tool tuning immediately).
2. Select provider first.
3. Complete OAuth/API-key auth for that provider.
4. Pick model from the provider model list.
5. Save config when prompted.

Setup writes config under:

- `~/.hermes-agent-ultra/config.yaml`
- `~/.hermes-agent-ultra/auth/`

## 3) Start Interactive Session

```bash
hermes-ultra
```

Useful in-session commands:

- `/help` — command catalog
- `/model` — interactive provider/model switcher
- `/personality list` — built-in personalities with usage guidance
- `/tools` — tool registry view
- `/about` — build + parity + upstream sync snapshot

## 4) Verify Runtime Health

```bash
hermes-ultra doctor --deep --snapshot
```

Optional support bundle:

```bash
hermes-ultra doctor --deep --snapshot --bundle
```

## 5) Gateway Mode (Optional)

Run the gateway in the foreground. Without chat platforms it serves only cron
jobs and HTTP webhooks; `hermes-ultra gateway setup` enables platforms.

```bash
hermes-ultra gateway run
```

## 6) Coming from Hermes Agent

Preview what `hermes-ultra migrate` would move from `~/.hermes`, then apply it:

```bash
hermes-ultra migrate
hermes-ultra migrate --apply
```

See [Migrating from Hermes](./README.md#migrating-from-hermes) for what moves
and what does not.

# Security

Reformed Agent runs tools on your machine, holds model and platform credentials, and can take messages from chat platforms through its gateway, so a flaw in any of these matters.

Please report a vulnerability privately through this repository's private vulnerability reporting (Security tab, 'Report a vulnerability', or <https://github.com/yungSNAT/reformed-agent/security/advisories/new>), not in a public issue. Include the commit, your platform, what you did and what you saw. You will get an acknowledgement, and a fix or a decision, as soon as the maintainer can review it; there is no fixed response time.

In scope: anything that runs a command or tool the user did not approve; reads, writes or sends files outside what the user allowed; exposes a credential, key or token; lets a gateway message act as someone other than its sender; or weakens a sandbox, approval or redaction step.

Out of scope:

- A program already running as you on the same machine. It can read your configuration and every file you can read.
- What a model provider or chat platform does with the data you choose to send it.
- A flaw in a dependency that Reformed Agent's code does not reach. Report it to that project.

## Supported versions

There are no releases yet. Fixes land on `main`.

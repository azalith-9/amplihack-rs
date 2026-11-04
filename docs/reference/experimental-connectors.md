# Experimental agent connectors

These connectors launch installed CLIs. They do not yet provide autonomous
workflow execution or the hook-based quality gates of the existing integrations.

| Command | Executable | Staged framework assets |
| --- | --- | --- |
| `amplihack hermes` | `hermes` | Skills in `~/.hermes/skills/amplihack/` |
| `amplihack zcode` | `zcode` | Managed instruction block in `~/.zcode/AGENTS.md` |
| `amplihack antigravity` | `agy` | No agent-specific assets |

Install the agent CLI separately and make its executable available on PATH.
Antigravity resolves `agy`; it never probes or launches the desktop
`antigravity` executable. Pass native agent options as trailing arguments.

All three commands reject `--auto`. The headless command builders currently
support argv delivery only; long prompts cannot fall back to plain stdin.
Hermes skill discovery in the nested namespace still needs live validation.
ZCode installations that expose only a desktop wrapper also need a verified
headless CLI entry point before use by an output-parsing executor.

## Installation ownership

Hermes owns only its `skills/amplihack` namespace. ZCode owns only its marked
instruction block; surrounding user instructions are preserved. Ownership
records live under `~/.amplihack/hermes/` and `~/.amplihack/zcode/`.
Install and uninstall refuse symlinks in these destinations or their parents
below the user home. Uninstall requires the exact expected destination in the
ownership record. These checks do not lock directories against concurrent
changes by another process.

ZCode repairs a missing or stale ownership record on a repeat installation even
when its instruction block is already current. A malformed manifest is reported
for manual reconciliation.

Hermes currently supports the default `~/.hermes` home only. An explicit
`HERMES_HOME` naming that same path is accepted. Other overrides are rejected
before staging assets, so a profile cannot silently launch without the staged
skills. Custom profile support requires separate ownership tracking.

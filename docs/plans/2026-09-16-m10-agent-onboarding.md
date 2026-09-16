# M10: agent onboarding from the command line

**Created:** 2026-09-16
**Status:** built, 2026-09-16. `pessimal-agent init` ships in the next release.

## Goal

Installing the agent and pointing it at a backend is one command that asks questions:

```
pessimal-agent init
```

Today it is: read the caveats, find the config file, edit TOML by hand, remember to `chmod 600` it
because the API key lives in it, run `--check`, then work out which service manager this machine
uses. That is not an onboarding flow, and it is the same work on every host.

## What `init` does

1. Works out where the config belongs, and says so before writing.
2. Asks for the backend, the endpoint, the protocol, the API key, the environment and the interval.
   An existing config supplies the defaults, so `init` also reconfigures.
3. Validates every answer as it is typed, and asks again rather than failing at the end.
4. Writes the file at mode 0600, atomically, keeping any previous file as `<name>.bak`.
5. Exports one batch to the backend and reports what happened. This is the CLI's "test connection".
6. Names the exact command that starts the service on this machine, and offers to run it.

Non-interactive, for a configuration management tool:

```
pessimal-agent init --non-interactive --preset signoz --endpoint https://… \
  --environment infrastructure --api-key-stdin < key.txt
```

## Where the config goes

In order, first that applies:

| Condition | Path |
|---|---|
| `--config PATH` or `PESSIMAL_CONFIG` | that path |
| A Homebrew install (the formula's `etc/pessimal` exists) | `<brew prefix>/etc/pessimal/pessimal.toml` |
| Running as root on Linux | `/etc/pessimal/pessimal.toml` |
| Otherwise | `$XDG_CONFIG_HOME/pessimal/pessimal.toml`, else `~/.config/pessimal/pessimal.toml` |

The Homebrew case matters because `brew services` starts the agent with
`--config <brew prefix>/etc/pessimal/pessimal.toml`. A config written anywhere else would be
ignored by the service the user is about to start, which is exactly the confusion this removes.

## The API key

It goes in the config file, which is written 0600, and the prompt does not echo it.

Not into the service's environment: `brew services` writes its plist with mode 0644, so a key there
may be readable by other accounts on the machine. `scripts/install-agent-launchd.sh` puts the key in
a 0600 plist instead, which is equally safe and is left as it is; `init` never edits that plist.

`--api-key-stdin` reads one line from stdin. The key is never taken as a command-line argument,
because arguments are visible in `ps` to every user on the machine.

## Starting the service

`init` detects the manager rather than assuming one:

| Detected | Start command |
|---|---|
| Homebrew formula installed, `brew` on `PATH` | `brew services start pessimal-agent` |
| macOS, the LaunchAgent from `scripts/install-agent-launchd.sh` is loaded | already running; `init` says so and offers a restart |
| Linux with `systemctl` and the unit file installed | `sudo systemctl enable --now pessimal-agent` |
| Anything else | prints the command to run the agent in the foreground |

It runs the command only after a "yes", or with `--start`. It never installs a service unit that is
not there: writing one is what `scripts/install-agent-launchd.sh` and the tarball's systemd unit do.

## Design

The rule this codebase already follows: no I/O in the core.

`pessimal_agent_core::onboarding` is pure and holds every decision — answer validation, building an
`AgentConfig` from the answers, rendering commented TOML that parses back into the same config,
resolving the config path from facts, and choosing the service plan from facts. `SystemFacts` is a
plain struct the binary fills in (environment variables, which files exist, which binaries are on
`PATH`, whether the process is root).

`pessimal_agent_host::init` is the shell: prompts on a terminal, reads stdin, writes files, runs the
one-batch export, and runs the start command.

That split is what makes the flow testable: every question's validation, every path decision and
every service plan is a unit test with no terminal and no filesystem.

## Verification

1. `cargo test --workspace`: the core's onboarding tests, including a round trip from answers to
   TOML and back to the same `AgentConfig`, and a rendered file that `AgentConfig::from_toml`
   accepts.
2. Path resolution and service plan tests for macOS with and without Homebrew, Linux as root and as
   a user, and an explicit `--config`.
3. The key never reaches a command line: a test asserts `init` rejects `--api-key` as an unknown
   argument.
4. By hand on this machine, 2026-09-16, against the real SigNoz instance: the flow re-asks after a
   bad URL, writes 0600, and reports "The backend accepted a batch". With a wrong key it reports
   `the backend refused it — Unauthenticated: Invalid or missing key`, which is the exporter's DEBUG
   event captured by a tracing layer — the SDK itself only says `InternalFailure("Failed to flush")`.
   The LaunchAgent on that machine was detected rather than a second service being suggested.

## Out of scope

- Installing a service unit that does not exist yet. Two tools already do that.
- Editing the LaunchAgent plist written by `scripts/install-agent-launchd.sh`.
- Windows service registration.

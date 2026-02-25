# Phylactery

A personal AI agent built the way Unix intended: as a bunch of small programs that talk to each other through text streams, files, and sockets.

No framework. No monolith. No microservices. Just processes.

## What Is This?

Phylactery is an autonomous AI agent that runs as cooperating Unix processes on your machine. It manages its own long-term memory, reflects on its experiences, talks to you over Signal, reacts to webhooks, polls your infrastructure, watches your filesystem, and does whatever you tell it to -- all without a browser tab in sight.

Each piece does one thing. The daemon manages sessions. The session runner runs the agentic loop. Tools are executables that read JSON from stdin and write JSON to stdout. The model adapter translates to whatever LLM backend you use. Bridges connect the agent to the outside world. They compose because they're just programs.

If a tool crashes, the session continues. If the webhook listener crashes, the daemon doesn't notice. If you want to swap the model, you write a new adapter and change one line in a config file. Everything is replaceable because nothing is coupled.

## The Philosophy

Every boundary between components is one of three things:

- **A Unix socket** -- the daemon serves REST here, no TCP, no network exposure
- **stdin/stdout JSON** -- tools and model adapters are child processes that speak JSON
- **Files** -- logs, knowledge base, named pipes for event injection, identity files

There's no custom IPC. No message bus. No gRPC. If you can `curl` a Unix socket and `echo` JSON into a pipe, you can operate the whole system by hand.

Tools are discovered from `$PATH`. Anything named `phyl-tool-*` that responds to `--spec` becomes available to the agent. Write one in Rust, Python, bash -- the agent doesn't care. The contract is the spec.

## What It Can Do

- **Run sessions** -- give it a task, it figures out how to do it using tools, asks you questions when it's stuck, and writes a summary when it's done
- **Remember things** -- a git-backed knowledge base that the agent reads from and writes to, with full history
- **Evolve** -- SOUL.md is written by the agent after every session, reflecting on what it learned. LAW.md (your rules) constrains it. JOB.md (your description) focuses it. SOUL.md (its own words) defines it.
- **Talk to you** -- via Signal Messenger, the terminal, or any bridge you write. It asks questions. You answer. It continues.
- **React to the world** -- webhooks from GitHub, SSE streams from your infrastructure, file watches on a directory, polling any CLI command. All create sessions automatically.
- **Use any MCP server** -- plug in MCP servers and their tools appear in every session, namespaced and ready to use

## Architecture at a Glance

```
You ──► phyl (CLI) ──► phylactd (daemon) ──► phyl-run (session) ──► model adapter
                            │                      │
                            │                      ├──► phyl-tool-bash
                            │                      ├──► phyl-tool-files
                            │                      ├──► phyl-tool-session
                            │                      └──► phyl-tool-mcp ──► MCP servers
                            │
                            ├──► phyl-poll (polls commands for changes)
                            ├──► phyl-listen (webhooks, SSE, file watches)
                            └──► phyl-bridge-signal (Signal Messenger)
```

Twelve crates. One library, eleven binaries. Each is small enough to read in one sitting.

See [docs/](docs/README.md) for the full documentation.

## Quick Start

### Build

```sh
git clone https://github.com/gabrielbauman/phylactery.git
cd phylactery
cargo build --release
```

Binaries land in `target/release/`. Put them on your `$PATH`.

### Initialize

```sh
phyl init
```

This creates the agent's home at `~/.local/share/phylactery/` (Linux) or `~/Library/Application Support/phylactery/` (macOS) with:
- `config.toml` -- configuration with sensible defaults
- `secrets.env` -- secret storage (mode 600, gitignored)
- `LAW.md`, `JOB.md` -- edit these to define your agent's rules and role
- `SOUL.md` -- starts as "I am new." The agent takes it from here.
- `knowledge/` -- the agent's long-term memory
- A git repo to track it all

### Configure

Edit the identity files to define your agent:

```sh
$EDITOR ~/.local/share/phylactery/LAW.md    # Your rules
$EDITOR ~/.local/share/phylactery/JOB.md    # Its role
```

The default model adapter uses the [Claude CLI](https://docs.anthropic.com/en/docs/claude-cli). Make sure `claude` is on your `$PATH`, or set `PHYL_CLAUDE_CLI` to point to it.

### Run

```sh
# Start the daemon
phyl start

# In another terminal, start a session
phyl session "What tools do you have available? List them."

# Or run everything (daemon + configured services) in one shot
phyl start --all
```

### Optional: Service Installation

On **Linux** (systemd):

```sh
phyl setup systemd
```

On **macOS** (launchd):

```sh
phyl setup launchd
```

This generates, installs, and enables service definitions for the daemon and any configured services (poller, listener, bridge). On Linux this creates systemd user units; on macOS this creates launchd user agents in `~/Library/LaunchAgents/`.

### Optional: Add Integrations

```sh
# Add an MCP server
phyl config add mcp filesystem
phyl config edit

# Add a poll rule
phyl config add poll github-notifications
phyl config edit

# Add a webhook
phyl config add hook github
phyl config edit

# Add secrets
phyl config add-secret GITHUB_WEBHOOK_SECRET abc123

# Set up Signal bridge
phyl config add bridge signal
phyl config edit
```

### Check Status

```sh
phyl setup status    # health of all components
phyl ls              # list sessions
phyl watch           # live feed of all session activity
```

## Documentation

Full documentation lives in [docs/](docs/README.md), with a page for every component:

**Architecture**: [Overview](docs/architecture.md) -- [Protocols](docs/protocols.md) -- [Configuration](docs/configuration.md)

**Core**: [phyl](docs/phyl.md) -- [phylactd](docs/phylactd.md) -- [phyl-run](docs/phyl-run.md) -- [phyl-core](docs/phyl-core.md)

**Model Adapters**: [phyl-model-claude](docs/phyl-model-claude.md)

**Tools**: [bash](docs/phyl-tool-bash.md) -- [files](docs/phyl-tool-files.md) -- [session](docs/phyl-tool-session.md) -- [mcp](docs/phyl-tool-mcp.md)

**Event Sources**: [phyl-poll](docs/phyl-poll.md) -- [phyl-listen](docs/phyl-listen.md)

**Bridges**: [phyl-bridge-signal](docs/phyl-bridge-signal.md)

**Concepts**: [Sessions](docs/sessions.md) -- [Knowledge Base](docs/knowledge-base.md) -- [Identity Files](docs/identity-files.md)

## Security

Phylactery runs on your machine with your privileges. A few things to be aware of:

- **No network exposure by default.** The daemon listens on a Unix socket, not TCP. Only local processes with filesystem access to the socket can connect.
- **Socket permissions.** The daemon socket is created with mode `0600` (owner-only). Other users on the system cannot connect.
- **Secrets management.** API keys and webhook secrets live in `secrets.env` (mode `0600`, gitignored). They are loaded into the process environment at startup, never written to logs or config files.
- **Webhook verification.** Inbound webhooks are verified with HMAC-SHA256 signatures (GitHub and GitLab formats supported). Always configure a `secret` for production webhooks.
- **Tool sandboxing.** Tool specs declare sandbox boundaries (allowed paths, network access, resource limits). The file tool validates paths against allowed directories to prevent traversal.
- **The agent runs code.** The bash tool executes shell commands. LAW.md exists to constrain behavior, but treat the agent as having your user's full capabilities within its sandbox.

## Requirements

- Rust (edition 2024) for building
- Linux or macOS (file watching uses inotify on Linux and FSEvents on macOS via the `notify` crate)
- [Claude CLI](https://docs.anthropic.com/en/docs/claude-cli) for the default model adapter (or write your own)
- [signal-cli](https://github.com/AsamK/signal-cli) if using the Signal bridge

## License

See [LICENSE](LICENSE).

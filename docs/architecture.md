# Architecture Overview

Phylactery is built as a set of small cooperating Unix processes. Each does one thing. They communicate via text streams, files, and Unix sockets.

This is not a framework. It's a bunch of programs that talk to each other through the oldest, most boring interfaces available. If that sounds like the Unix philosophy, it's because it is.

## The Big Picture

```
┌─────────────────────────────────────────────────────────┐
│                      User / Scripts                      │
│                                                          │
│  phyl session "..."     curl --unix-socket ...           │
│  phyl status <id>       echo "event" > sessions/x/events │
│  phyl say <id> "..."    cron: phyl session "check mail"  │
└──────────┬──────────────────────────┬───────────────────┘
           │                          │
     ┌─────▼──────┐                   │
     │    phyl     │ (CLI client)     │
     │ talks to    │                  │
     │ Unix socket │                  │
     └─────┬──────┘                   │
           │                          │
     ┌─────▼──────────────────────────▼──┐
     │            phylactd                │ (daemon)
     │                                    │
     │  Manages sessions (spawn/kill)     │
     │  Serves REST API on Unix socket    │
     │  Watches session logs              │
     │  Enforces concurrency limits       │
     └──────┬─────────────────────────────┘
            │ spawns one per session
            │
     ┌──────▼──────────────────────────────────┐
     │            phyl-run                      │ (session runner)
     │                                          │
     │  The agentic loop:                       │
     │  1. Read LAW.md, JOB.md, SOUL.md        │
     │  2. Discover tools (phyl-tool-* --spec)  │
     │  3. Read events from FIFO + initial args │
     │  4. Invoke model adapter                 │
     │  5. Parse tool calls, dispatch to tools  │
     │  6. Loop until done                      │
     └──┬───────────┬──────────────────────────┘
        │           │
        │           │ invokes (stdin/stdout JSON)
        │           │
   ┌────▼────┐  ┌───▼──────────────┐
   │  Tools  │  │  Model Adapters  │
   └─────────┘  └──────────────────┘
```

## Two Repos, Two Concerns

Phylactery uses a deliberate two-repo model:

- **This repo** builds the binaries. Clone it, build it, install, forget about it until you want to hack on the agent itself.
- **`$PHYLACTERY_HOME`** (default `~/.local/share/phylactery` on Linux, `~/Library/Application Support/phylactery` on macOS) is the agent's home. A separate git repo created by `phyl init`. It holds identity files, the knowledge base, session state, config, and secrets. This is the agent's living memory.

## Process Architecture

There is no monolith. Instead, there are 13 crates that build into one library and twelve binaries:

| Binary | Role |
|--------|------|
| `phylactd` | Daemon. Manages sessions, serves API on a Unix socket. |
| `phyl` | CLI client. Thin wrapper over HTTP-to-Unix-socket. |
| `phyl-run` | Session runner. The agentic loop. One per session. |
| `phyl-model-claude` | Model adapter for the Claude CLI. |
| `phyl-model-openai` | Model adapter for OpenAI-compatible APIs (Ollama, llama.cpp, vLLM, etc.). |
| `phyl-tool-bash` | Tool: execute shell commands. |
| `phyl-tool-files` | Tool: read/write/search files. |
| `phyl-tool-session` | Tool (server mode): human interaction + session control. |
| `phyl-tool-mcp` | Tool (server mode): bridge to any MCP server. |
| `phyl-bridge-signal` | Bridge: two-way Signal Messenger interface. |
| `phyl-poll` | Poller: run commands on intervals, sessions on change. |
| `phyl-listen` | Listener: webhooks, SSE subscriptions, file watches. |

## Why So Many Processes?

Each process can crash without taking the others down. Each can be replaced independently. Each can be written in any language as long as it speaks the right protocol. The daemon never touches the network. The webhook listener never touches the model. The tools don't know about each other.

This isn't over-engineering -- it's the minimum number of pieces needed so that each piece is simple enough to reason about in isolation.

## Event Source Trifecta

Three complementary mechanisms bring the outside world to the agent:

| Mechanism | Binary | Direction | Trigger |
|-----------|--------|-----------|---------|
| Polling | `phyl-poll` | Pull (agent asks the world) | Output changed since last check |
| Listening | `phyl-listen` | Push (world tells the agent) | External system sends event |
| Bridges | `phyl-bridge-*` | Bidirectional | Human sends message |

All three create sessions via `POST /sessions` on the daemon API. The daemon doesn't care how a session was born -- it just runs it.

## Communication Patterns

Every inter-process boundary uses one of three patterns:

1. **Unix socket HTTP** -- CLI and support services talk to the daemon via REST on a Unix socket. No TCP, no TLS, no network exposure.
2. **stdin/stdout JSON** -- Tools and model adapters are invoked as child processes. They read JSON from stdin, write JSON to stdout. Stateless (one-shot) or stateful (NDJSON server mode).
3. **Files** -- Session logs (`log.jsonl`), named pipes (FIFOs) for event injection, the knowledge base, identity files. The filesystem is shared state.

See [Protocols](protocols.md) for the exact JSON schemas.

## Service Topology

A typical running system looks like this:

```
systemd (or phyl start --all)
  ├── phylactd              (always running, manages sessions)
  │   ├── phyl-run          (one per active session)
  │   │   ├── phyl-model-*        (model adapter: claude, openai, or custom)
  │   │   ├── phyl-tool-mcp       (server-mode, lifetime of session)
  │   │   └── phyl-tool-session   (server-mode, lifetime of session)
  │   └── phyl-run          (another session...)
  ├── phyl-poll             (long-lived, polls on intervals)
  ├── phyl-listen           (long-lived, receives events)
  └── phyl-bridge-signal    (long-lived, Signal interface)
```

The daemon is the center. Everything else is optional. You can run just the daemon and CLI for a purely manual setup, or wire up polling, listening, and bridges for full autonomy.

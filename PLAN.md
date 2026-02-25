# Phylactery — Implementation Plan

A minimal, opinionated personal AI agent built as a set of small cooperating
programs. Each does one thing. They communicate via text streams, files, and
Unix sockets. Written in Rust where reliability matters; tools and model
adapters can be anything executable.

---

## Core Concepts

### Sessions

A session is a conversation with a goal. Each session is a **separate OS
process** (`phyl-run`). Sessions are created by humans, scripts, webhooks,
cron — anything that can talk to the daemon.

Each session gets:

- A UUID
- A git-tracked working directory: `sessions/<uuid>/`
- An append-only event log: `sessions/<uuid>/log.jsonl`
- An input FIFO: `sessions/<uuid>/events` (named pipe for live injection)
- Access to the shared knowledge base
- Access to tools (discovered from `$PHYLACTERY_TOOLS_PATH`)

Sessions are interactive. Write to the FIFO and the running session picks it up
on its next loop iteration.

A session ends when:

- The model calls the `done` tool
- The daemon kills the process (user cancellation)
- It times out (configurable, default 1 hour)

### The Knowledge Base

Markdown files under `knowledge/`, tracked by git. This is long-term memory.
Any session can read from it. Any session can write files and commit. Structure
is agent-managed but seeded with:

```
knowledge/
  contacts/
  projects/
  preferences/
  journal/
```

The agent updates this regularly (enforced by LAW.md). Commits are automatic
with descriptive messages. Concurrent writes from parallel sessions are handled
by git's normal merge machinery — sessions pull before committing.

### LAW.md

Root-level file. Injected into every session as system prompt. Unconditional
rules. The agent cannot override, ignore, or modify these. This is the policy
layer.

### JOB.md

Root-level file. Describes the agent's role and scope. The agent should refuse
sessions outside its job description. This is the focus layer.

---

## Architecture: A Set of Small Programs

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
     │  1. Read LAW.md, JOB.md                  │
     │  2. Discover tools (phyl-tool-* --spec)  │
     │  3. Read events from FIFO + initial args │
     │  4. Invoke model adapter                 │
     │  5. Parse tool calls, dispatch to tools  │
     │  6. Loop until done                      │
     │  7. Write all events to log.jsonl        │
     └──┬───────────┬──────────────────────────┘
        │           │
        │           │ invokes (stdin/stdout JSON)
        │           │
   ┌────▼────┐  ┌───▼──────────────┐
   │  Tools  │  │  Model Adapters  │
   └─────────┘  └──────────────────┘
```

### The Binaries

| Binary | Role | Language |
|--------|------|----------|
| `phylactd` | Daemon. Manages sessions, serves API. | Rust |
| `phyl` | CLI client. Thin wrapper over HTTP-to-Unix-socket. | Rust |
| `phyl-run` | Session runner. The agentic loop. | Rust |
| `phyl-model-claude` | Model adapter for Claude CLI. | Rust (or shell) |
| `phyl-tool-bash` | Tool: execute shell commands. | Rust (or shell) |
| `phyl-tool-files` | Tool: read/write/search files. | Rust (or shell) |
| `phyl-tool-done` | Tool: end a session. | Shell script |
| `phyl-tool-ask` | Tool: ask the human a question. Blocks until answered. | Rust |
| `phyl-tool-mcp` | Tool: bridge to any MCP server. | Rust |
| `phyl-bridge-signal` | Bridge: two-way Signal Messenger interface. | Rust |

Each tool and model adapter is a standalone executable. They can be written in
any language. The contract is JSON on stdin/stdout.

---

## Interface Contracts

These are the only interfaces that matter. Everything flows through them.

### Model Adapter Protocol

A model adapter is an executable. It reads a request from stdin, writes a
response to stdout.

**Input** (stdin, JSON):

```json
{
  "messages": [
    { "role": "system", "content": "You are..." },
    { "role": "user", "content": "Hello" },
    { "role": "assistant", "content": "Hi", "tool_calls": [...] },
    { "role": "tool", "tool_call_id": "x", "content": "result" }
  ],
  "tools": [
    {
      "name": "bash",
      "description": "Run a shell command",
      "parameters": { "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] }
    }
  ]
}
```

**Output** (stdout, JSON):

```json
{
  "content": "Here's what I found...",
  "tool_calls": [
    {
      "id": "tc_1",
      "name": "bash",
      "arguments": { "command": "ls -la" }
    }
  ]
}
```

That's it. `phyl-model-claude` translates this to/from whatever the `claude`
CLI expects. Want to use Ollama? Write `phyl-model-ollama`. Same contract.
A shell script could do it:

```sh
#!/bin/sh
# phyl-model-echo — a test model that just echoes
jq -r '.messages[-1].content' | jq -R '{content: ., tool_calls: []}'
```

### Tool Protocol

A tool is an executable. It supports two modes:

**Discovery** (`--spec`):

```sh
$ phyl-tool-bash --spec
{
  "name": "bash",
  "description": "Execute a shell command and return its output",
  "parameters": {
    "type": "object",
    "properties": {
      "command": { "type": "string", "description": "The command to run" }
    },
    "required": ["command"]
  }
}
```

One executable can expose multiple tools by returning an array:

```sh
$ phyl-tool-files --spec
[
  { "name": "read_file", "description": "...", "parameters": { ... } },
  { "name": "write_file", "description": "...", "parameters": { ... } },
  { "name": "search_files", "description": "...", "parameters": { ... } }
]
```

**Invocation** (stdin/stdout):

```sh
$ echo '{"name":"bash","arguments":{"command":"ls"}}' | phyl-tool-bash
{"output": "file1.txt\nfile2.txt\n"}
```

On error:

```sh
$ echo '{"name":"bash","arguments":{"command":"false"}}' | phyl-tool-bash
{"error": "Command exited with status 1"}
```

Tools receive environment variables for context:

| Variable | Value |
|----------|-------|
| `PHYLACTERY_SESSION_ID` | UUID of the current session |
| `PHYLACTERY_SESSION_DIR` | Absolute path to session working directory |
| `PHYLACTERY_KNOWLEDGE_DIR` | Absolute path to knowledge base |
| `PHYLACTERY_ROOT` | Absolute path to phylactery repo root |

### Event Log Format

Each session writes to `sessions/<uuid>/log.jsonl`. One JSON object per line:

```jsonl
{"ts":"2026-02-24T12:00:00Z","type":"system","content":"Session started"}
{"ts":"2026-02-24T12:00:00Z","type":"user","content":"Check my email"}
{"ts":"2026-02-24T12:00:01Z","type":"assistant","content":"I'll check...","tool_calls":[...]}
{"ts":"2026-02-24T12:00:02Z","type":"tool_result","tool_call_id":"tc_1","content":"3 new messages"}
{"ts":"2026-02-24T12:00:03Z","type":"assistant","content":"You have 3 new emails..."}
{"ts":"2026-02-24T12:00:04Z","type":"done","summary":"Checked email, reported 3 new messages"}
```

### Session Input FIFO

Each session creates a named pipe at `sessions/<uuid>/events`. Anyone can write
to it to inject events into the running session:

```sh
echo '{"type":"user","content":"Actually, also check Signal"}' > sessions/$ID/events
```

The session runner reads from this FIFO in a non-blocking loop alongside the
model invocation cycle.

---

## Human Interface: Bridges

The agent needs human attention sometimes — to answer questions, approve
actions, or just report results. Rather than build one UI, we build a
**bridge protocol** and let small programs connect the agent to any transport.

### The Attention Feed

Sessions can request human attention. The model does this via an `ask_human`
tool (built-in). When called, the session emits a question event to the log
and blocks, waiting for a response on its FIFO.

Log event:

```jsonl
{"ts":"...","type":"question","id":"q_1","content":"Should I send this email to Bob?","options":["yes","no","edit draft"]}
```

The daemon aggregates attention-worthy events from all session logs into a
single SSE (Server-Sent Events) stream:

```
GET /feed → streams question, notification, done, error events from all sessions
```

When a bridge delivers the human's answer, it posts back:

```
POST /sessions/:id/events  body: {"type":"answer","question_id":"q_1","content":"yes"}
```

The session unblocks and continues.

### Bridge Protocol

A bridge is a standalone program. It:

1. Connects to `GET /feed` on the daemon's Unix socket
2. Presents events to the human (terminal, Signal, Matrix, email, whatever)
3. Collects responses
4. Posts them back via `POST /sessions/:id/events`

That's the entire contract. A bridge doesn't need to know about models, tools,
or the knowledge base. It just reads events and writes responses.

### Built-in: `phyl watch`

The simplest bridge. A CLI command that connects to the feed and displays a
live multiplexed view of all sessions:

```
$ phyl watch
[3a7f] Running: checking email...
[3a7f] QUESTION: Found 3 new emails. Summarize them? [yes/no]
> 3a7f yes
[3a7f] Summarizing...
[91b2] Done: "Updated project notes in knowledge base"
[3a7f] Done: "Summarized 3 emails, updated contacts/bob.md"
```

Line-based. Works over SSH. No TUI framework.

### Signal Bridge: `phyl-bridge-signal`

A separate program (likely Python, to use `signal-cli` or `signal-cli-rest-api`)
that:

1. Connects to `GET /feed` on the daemon's Unix socket
2. For each attention event, sends a Signal message to the configured phone
   number
3. Listens for Signal replies
4. Matches replies to sessions and posts them back via the API

Signal becomes a two-way interface to the agent:

```
Signal message from agent:
  [Session 3a7f] Found 3 new emails. Summarize them?
  Reply: yes / no / edit draft

You reply:
  yes

Agent continues.
```

The bridge can also accept **inbound** Signal messages as new sessions:

```
You send to agent:
  Check if the server is healthy

Bridge calls:
  POST /sessions {"prompt": "Check if the server is healthy"}

Agent creates a session, does the work, reports back via Signal.
```

This makes Signal the primary human interface. The agent messages you when it
needs something. You message the agent to give it tasks. Everything else runs
autonomously.

**Configuration** (in `config.toml`):

```toml
[bridge.signal]
phone = "+1234567890"           # Agent's Signal number
owner = "+0987654321"           # Your Signal number (only accept from this)
signal_cli = "signal-cli"       # Path to signal-cli binary
```

**Security:** The bridge only accepts messages from the configured owner
number. All other messages are ignored.

### Other Bridges (future)

The bridge protocol is simple enough that adding new transports is trivial:

| Bridge | Transport | Effort |
|--------|-----------|--------|
| `phyl watch` | Terminal (built-in CLI) | Built-in |
| `phyl-bridge-signal` | Signal Messenger | ~150 lines Python |
| `phyl-bridge-matrix` | Matrix chat room | ~150 lines Python |
| `phyl-bridge-email` | Email (IMAP/SMTP) | ~200 lines Python |
| `phyl-bridge-telegram` | Telegram bot | ~100 lines Python |
| `phyl-bridge-ntfy` | ntfy.sh push notifications (one-way) | ~30 lines shell |

Bridges are not part of the core Rust workspace. They're scripts in a
`bridges/` directory. They can be installed and run independently.

### The `ask_human` Tool

This is a built-in tool available to every session. When the model needs human
input, it calls this tool:

```json
{
  "name": "ask_human",
  "arguments": {
    "question": "Should I send this email to Bob?",
    "options": ["yes", "no", "edit draft"],
    "context": "Draft email: ..."
  }
}
```

The session runner:

1. Writes a `question` event to `log.jsonl` (with a unique question ID)
2. Blocks, waiting for an `answer` event on the FIFO
3. Returns the human's answer to the model as the tool result

If no answer arrives within a timeout (configurable, default 30 minutes), the
tool returns `"No response from human — timed out"` and the model decides how
to proceed.

---

## Daemon API

`phylactd` serves HTTP on a Unix socket. Default path:
`$XDG_RUNTIME_DIR/phylactery.sock`

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/sessions` | Start a session. Body: `{"prompt":"..."}` |
| `GET` | `/sessions` | List sessions: `[{id, status, created_at, summary}]` |
| `GET` | `/sessions/:id` | Session detail + recent log entries |
| `POST` | `/sessions/:id/events` | Inject event: `{"content":"..."}` |
| `DELETE` | `/sessions/:id` | Kill the session process |
| `GET` | `/feed` | SSE stream of attention events across all sessions |
| `GET` | `/health` | Health check |

No auth. Secure via socket file permissions (0700). If you need remote access,
put it behind an SSH tunnel or a reverse proxy.

---

## CLI

`phyl` is a thin HTTP client. It talks to the daemon's Unix socket.

```
phyl session "do the thing"        # Start session, stream output (foreground)
phyl session -d "do the thing"     # Start session, return ID (detached)
phyl ls                            # List sessions
phyl status <id>                   # Session detail
phyl say <id> "new info"           # Inject event into running session
phyl log <id>                      # Tail session log (like tail -f)
phyl stop <id>                     # Kill session
phyl watch                         # Live feed of all sessions, answer questions
phyl start                         # Start daemon (foreground)
phyl start -d                      # Start daemon (background, daemonize)
```

Foreground session streams `log.jsonl` to the terminal as it's written.
Ctrl-C detaches (session keeps running). `phyl stop` kills.

`phyl watch` is the primary human interface when using the terminal. It
multiplexes all session activity into a single stream and lets you respond to
agent questions inline.

---

## Project Structure

```
phylactery/
├── Cargo.toml                  # Workspace manifest
├── LAW.md                      # Agent rules (user-authored)
├── JOB.md                      # Agent job description (user-authored)
├── config.toml                 # Daemon config
├── crates/
│   ├── phyl-core/              # Shared types: Message, ToolCall, ToolSpec, etc.
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── phylactd/               # Daemon binary
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl/                   # CLI client binary
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-run/               # Session runner binary
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-model-claude/      # Claude model adapter
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-tool-bash/         # Bash tool
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-tool-files/        # File read/write/search tool
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-tool-mcp/          # MCP bridge tool
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-tool-ask/          # Ask-human tool (blocks for human response)
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── phyl-bridge-signal/     # Signal Messenger bridge
│       ├── Cargo.toml
│       └── src/main.rs
├── tools/
│   └── phyl-tool-done          # Shell script: ends a session
├── knowledge/                  # Git-tracked knowledge base
│   └── .gitkeep
└── sessions/                   # Per-session working directories (gitignored)
    └── .gitkeep
```

**Why a Cargo workspace?** The Rust binaries share types via `phyl-core`, but
compile to independent static binaries. `cargo build --release` produces all of
them. They can be installed to `~/.local/bin/` or `/usr/local/bin/`.

---

## Configuration

`config.toml`:

```toml
[daemon]
socket = "$XDG_RUNTIME_DIR/phylactery.sock"

[session]
timeout_minutes = 60
max_concurrent = 4
model = "phyl-model-claude"     # Path or name of model adapter binary

[git]
repo_path = "/home/user/phylactery"
auto_commit = true

# Tool discovery: search these directories for phyl-tool-* executables
tools_path = [
  "./tools",                    # Project-local tools
  "~/.local/lib/phylactery/tools",  # User tools
]

# MCP servers (used by phyl-tool-mcp)
[[mcp]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user"]

[[mcp]]
name = "brave-search"
command = "npx"
args = ["-y", "@anthropic/mcp-server-brave-search"]
env = { BRAVE_API_KEY = "$BRAVE_API_KEY" }
```

---

## The Agentic Loop (phyl-run)

This is the heart of the system. It lives in `phyl-run` and is deliberately
straightforward:

```
phyl-run --session-dir ./sessions/<uuid> --prompt "do the thing"

1. Read config.toml
2. Read LAW.md → prepend to system prompt
3. Read JOB.md → append to system prompt
4. Discover tools:
   for each phyl-tool-* in $tools_path:
     run `phyl-tool-X --spec` → collect tool schemas
5. Open events FIFO for reading (non-blocking)
6. Initialize history = [system_prompt, user_prompt]
7. Loop:
   a. Build model request: { messages: history, tools: tool_schemas }
   b. Pipe request to model adapter: echo $REQ | phyl-model-claude
   c. Parse model response
   d. Append assistant message to history
   e. Write to log.jsonl
   f. If tool_calls present:
      for each tool_call:
        pipe call to appropriate phyl-tool-* binary
        append tool result to history
        write to log.jsonl
      continue loop (goto a)
   g. If no tool_calls:
      check FIFO for new events
      if events: append as user messages, continue loop
      if no events: wait briefly, check again
      if done tool was called: exit
   h. Check timeout → exit if exceeded
8. Write final summary to log
9. Exit
```

Each model invocation and tool call is a **separate process**. No long-lived
connections. No state leaks. If the model adapter crashes, that invocation
fails and the session can retry or report the error. If a tool hangs, it can be
killed independently.

---

## Implementation Order

Each phase produces something you can run and test.

### Phase 1: Core Types + Skeleton

- [ ] `cargo init` workspace with `phyl-core`
- [ ] Define shared types in `phyl-core`: `Message`, `ToolCall`, `ToolSpec`,
      `ModelRequest`, `ModelResponse`, `ToolInput`, `ToolOutput`, `LogEntry`
- [ ] All types derive `Serialize`/`Deserialize`
- [ ] Stub the other crates with `fn main() { todo!() }`
- [ ] Verify: `cargo build` succeeds, produces multiple binaries

### Phase 2: Tool Protocol

Build two tools and verify the protocol works end-to-end from the command line.

- [ ] Implement `phyl-tool-bash`: `--spec` mode and invocation mode
- [ ] Implement `phyl-tool-files`: read_file, write_file, search_files
- [ ] Write `tools/phyl-tool-done` as a shell script
- [ ] Test from command line:
      `echo '{"name":"bash","arguments":{"command":"echo hi"}}' | phyl-tool-bash`

### Phase 3: Model Adapter

- [ ] Implement `phyl-model-claude`:
      - Read `ModelRequest` from stdin
      - Translate to claude CLI invocation (`claude --print --output-format json`)
      - Parse claude's response
      - Write `ModelResponse` to stdout
- [ ] Test from command line:
      `echo '{"messages":[{"role":"user","content":"say hi"}],"tools":[]}' | phyl-model-claude`

### Phase 4: Session Runner

The agentic loop, testable without a daemon.

- [ ] Implement `phyl-run`:
      - Parse args (session dir, prompt)
      - Discover tools from path
      - Build system prompt from LAW.md + JOB.md
      - Run the agentic loop
      - Write to log.jsonl
- [ ] Create FIFO, read events from it
- [ ] Test: `mkdir -p sessions/test && phyl-run --session-dir sessions/test --prompt "what is 2+2"`

### Phase 5: Daemon

- [ ] Implement `phylactd`:
      - Parse config
      - Listen on Unix socket (axum + hyper-unix)
      - Spawn `phyl-run` as child process for each session
      - Track session processes (pid, status)
      - Tail `log.jsonl` for session state
      - Kill sessions on DELETE
      - Reap finished sessions
- [ ] API endpoints: POST/GET/DELETE sessions, POST events, GET health
- [ ] Test: `curl --unix-socket /tmp/phyl.sock http://localhost/health`

### Phase 6: CLI Client

- [ ] Implement `phyl`:
      - `phyl start [-d]` — launch `phylactd`
      - `phyl session [-d] "prompt"` — POST /sessions, stream log
      - `phyl ls` — GET /sessions
      - `phyl status <id>` — GET /sessions/:id
      - `phyl say <id> "msg"` — POST /sessions/:id/events
      - `phyl log <id>` — tail sessions/:id/log.jsonl
      - `phyl stop <id>` — DELETE /sessions/:id
- [ ] Test: full cycle with daemon + CLI

### Phase 7: MCP Bridge

- [ ] Implement `phyl-tool-mcp`:
      - On `--spec`: start configured MCP servers, list their tools, aggregate
      - On invocation: route tool call to the correct MCP server
      - JSON-RPC over stdio to MCP servers
- [ ] Test: configure an MCP server, invoke a tool through the bridge

### Phase 8: Knowledge Base + Git

- [ ] Implement auto-commit in `phyl-tool-files` for writes under `knowledge/`
- [ ] Add `search_files` tool (wraps `grep -r`)
- [ ] Seed `knowledge/` directory structure
- [ ] Add knowledge base summary generation to session startup

### Phase 9: Human Attention System

- [ ] Implement `phyl-tool-ask`:
      - On `--spec`: return ask_human tool schema
      - On invocation: write question event to log, block waiting for answer
        on a response file/FIFO, return answer as tool output
- [ ] Add `GET /feed` SSE endpoint to daemon:
      - Tail all active session logs
      - Filter for attention-worthy events (question, done, error)
      - Stream as SSE to connected clients
- [ ] Implement `phyl watch` CLI command:
      - Connect to `GET /feed`
      - Display events, accept typed responses
      - Route responses to correct session via `POST /sessions/:id/events`

### Phase 10: Signal Bridge

- [ ] Implement `phyl-bridge-signal`:
      - Connect to daemon `GET /feed` via Unix socket
      - Send questions/notifications as Signal messages via `signal-cli`
      - Listen for inbound Signal messages
      - Route replies back to sessions via `POST /sessions/:id/events`
      - Accept new session requests from inbound messages
      - Only accept messages from configured owner number
- [ ] Config: signal phone numbers, signal-cli path
- [ ] Test: end-to-end question/answer cycle over Signal

---

## Design Decisions

### Why a set of binaries instead of one?

- **Testability.** Every component can be tested from the command line with
  `echo` and pipes. No test harness needed.
- **Replaceability.** Don't like the Claude adapter? Replace it. Want a custom
  tool? Write a script. Any language.
- **Isolation.** A crashing tool doesn't take down the session. A crashing
  session doesn't take down the daemon.
- **Simplicity.** Each binary is small. Easy to understand, audit, debug.
- **Composability.** The pieces work together but don't depend on each other's
  internals. You can run `phyl-run` without the daemon. You can invoke a tool
  without a session.

### Why Rust for the core binaries?

Single static binaries. No runtime dependencies. Start in milliseconds.
Run for months without leaking. This agent is infrastructure — it should be
as reliable as `sshd`.

### Why allow any language for tools and model adapters?

The contract is JSON on stdin/stdout. A tool is just an executable. This means:

- Quick prototyping in shell scripts
- Python tools when libraries are needed
- Rust tools when performance matters
- Even `jq` pipelines as tools

### Why shell out to `claude` instead of calling an API?

- No API keys baked into the agent
- No HTTP client code
- Model binary handles auth, retries, token management
- Easy to swap: change one config line from `phyl-model-claude` to
  `phyl-model-ollama`
- The `claude` CLI already exists and works. Don't rebuild it.

### Why Unix socket for the daemon?

- No auth needed — filesystem permissions are the ACL
- Any HTTP client works: `curl --unix-socket`
- Simpler than TCP + TLS + auth tokens
- Standard on Linux

### Why FIFO for session input?

- Standard Unix primitive
- Any process can write to it: `echo "msg" > fifo`
- No protocol, no client library needed
- Works from shell scripts, cron jobs, systemd units

### Why git for memory?

- Version history for free
- Human-readable, editable with `vim`
- Searchable with `grep`
- Diffable
- Syncable to a remote
- The agent can inspect its own history with `git log`
- No database to configure or maintain

### Why not a database?

We store text. Git stores text. Adding a database means adding a dependency,
a schema, a migration strategy, a backup strategy, and a query language. Files
and grep are enough.

### Why JSONL for session logs?

- Append-only (safe for concurrent writes with line buffering)
- Streamable (tail -f works)
- Each line is independently parseable (no framing issues)
- Standard format, tooling everywhere

---

## What This Is Not

- **Not a chat UI.** No web frontend. Use the terminal or Signal.
- **Not multi-user.** One human, one agent, one machine.
- **Not cloud-native.** It's local processes on a Linux box.
- **Not a framework.** You don't import it. You run it.
- **Not sandboxed.** The bash tool runs real commands. LAW.md is policy, not
  a security boundary.
- **Not a daemon that must be running.** You can run `phyl-run` directly to
  test a session without the daemon.

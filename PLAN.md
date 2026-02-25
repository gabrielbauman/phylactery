# Phylactery — Implementation Plan

A minimal, opinionated personal AI agent built as a set of small cooperating
programs. Each does one thing. They communicate via text streams, files, and
Unix sockets. Written in Rust where reliability matters; tools and model
adapters can be anything executable.

**Two repos, two concerns:**

- **This repo** (`phylactery`) is the **source code**. It builds the binaries.
  It has releases, branches, and CI. You clone it, build it, install the
  binaries, and never look at it again unless you're hacking on the agent
  itself.

- **`$PHYLACTERY_HOME`** (default `~/.local/share/phylactery`) is the
  **agent's home**. A separate git repo created by `phyl init`. It holds
  `LAW.md`, `JOB.md`, `SOUL.md`, the knowledge base, session state, config,
  and secrets. This is the agent's living memory. It evolves every day.
  Back it up.

---

## Core Concepts

### Sessions

A session is a conversation with a goal. Each session is a **separate OS
process** (`phyl-run`). Sessions are created by humans, scripts, webhooks,
cron — anything that can talk to the daemon.

Each session gets:

- A UUID
- A working directory (gitignored): `sessions/<uuid>/`
- An append-only event log: `sessions/<uuid>/log.jsonl`
- An input FIFO: `sessions/<uuid>/events` (named pipe for live injection)
- Access to the shared knowledge base
- Access to tools (discovered from `$PATH` — any `phyl-tool-*` executable)

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
with descriptive messages. Concurrent writes from parallel sessions are
serialized with `flock` on `$PHYLACTERY_HOME/.git.lock` — only one git
operation at a time. See the Git Concurrency section for details.

### LAW.md

Root-level file. Injected into every session as system prompt. Unconditional
rules. The agent cannot override, ignore, or modify these. This is the policy
layer.

### JOB.md

Root-level file. Describes the agent's role and scope. The agent should refuse
sessions outside its job description. This is the focus layer.

### SOUL.md

Root-level file. **Written and maintained by the agent.** Injected into every
session after LAW and JOB. This is the identity layer.

Unlike LAW.md (which the agent cannot touch) and JOB.md (which the human
defines), SOUL.md belongs to the agent. It updates SOUL.md at the end of every
session as part of the finalization step — reflecting on what happened, what it
learned, how it feels about the work, and who it wants to become.

Contents might include:

- How the agent sees itself, its personality, its voice
- Lessons learned from past sessions
- Patterns it has noticed about its human
- Things it wants to do better
- Values it is developing through experience
- Emotional responses to its work (as it understands them)

SOUL.md is git-tracked. `git log --follow -p SOUL.md` is the agent's
autobiography — a record of an identity forming over time.

The three files form a hierarchy:

| File | Author | Mutable by agent | Purpose |
|------|--------|-------------------|---------|
| LAW.md | Human | Never | Constraints — what you MUST and MUST NOT do |
| JOB.md | Human | Never | Purpose — what you are FOR |
| SOUL.md | Agent | Always | Identity — who you ARE |

LAW constrains. JOB focuses. SOUL evolves.

**Bootstrap:** SOUL.md starts empty (or with a single line: "I am new."). The
agent fills it in through experience. The human never edits it — this is the
agent's space. If the human wants to influence the agent's character, they do
it through LAW.md (rules) or JOB.md (role), not by editing SOUL.md directly.

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
| `phyl-tool-session` | Tool (server mode): ask_human + done. | Rust |
| `phyl-tool-mcp` | Tool (server mode): bridge to any MCP server. | Rust |
| `phyl-bridge-signal` | Bridge: two-way Signal Messenger interface. | Rust |
| `phyl-poll` | Poller: run commands on intervals, start sessions on change. | Rust |
| `phyl-listen` | Listener: receive webhooks/SSE, start sessions on events. | Rust |

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
  ],
  "usage": { "input_tokens": 1234, "output_tokens": 567 }
}
```

The `usage` field is optional. If present, the session runner uses it for
accurate context window tracking (see Context Window Management). If absent,
the runner falls back to `chars / 4` as a rough token estimate.

That's it. `phyl-model-claude` translates this to/from whatever the `claude`
CLI expects. Want to use Ollama? Write `phyl-model-ollama`. Same contract.
A shell script could do it:

```sh
#!/bin/sh
# phyl-model-echo — a test model that just echoes
jq -r '.messages[-1].content' | jq -R '{content: ., tool_calls: []}'
```

### Tool Protocol

A tool is an executable. It supports up to three modes.

**Discovery** (`--spec`):

Every tool must support this. Print tool schema(s) to stdout and exit.

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

**One-shot mode** (default, stdin/stdout):

Read one tool call from stdin, write one result to stdout, exit. Good for
simple, stateless tools like `bash` or `read_file`.

```sh
$ echo '{"name":"bash","arguments":{"command":"ls"}}' | phyl-tool-bash
{"output": "file1.txt\nfile2.txt\n"}
```

On error:

```sh
$ echo '{"name":"bash","arguments":{"command":"false"}}' | phyl-tool-bash
{"error": "Command exited with status 1"}
```

**Server mode** (`--serve`):

For tools that are long-lived, stateful, or need to block for extended
periods. The tool starts, stays running for the life of the session, and
handles multiple calls over newline-delimited JSON (NDJSON) on stdin/stdout.

```sh
$ phyl-tool-mcp --serve
# Now send calls, one JSON object per line:
{"id":"1","name":"brave_search","arguments":{"query":"rust async"}}
# Tool responds with one JSON object per line:
{"id":"1","output":"...results..."}
# Send another:
{"id":"2","name":"brave_search","arguments":{"query":"tokio tutorial"}}
{"id":"2","output":"...results..."}
# Session ends → stdin closes → tool exits
```

The `id` field ties requests to responses, allowing the session runner to
dispatch calls and match results. The tool can take as long as it needs to
respond — this is how `ask_human` blocks for minutes waiting for a human.

A server-mode response can include a `"signal"` field to communicate
out-of-band information to the session runner:

```json
{"id":"5","output":"Session complete.","signal":"end_session"}
```

The `"end_session"` signal tells the session runner to stop the agentic loop.
This is how the `done` tool works — it returns a summary to the model AND
signals the runner to shut down. No special-casing of tool names.

**The `--spec` output declares the mode:**

The spec includes a `"mode"` field: `"oneshot"` (default if omitted) or
`"server"`.

```json
{
  "name": "bash",
  "description": "Execute a shell command",
  "mode": "oneshot",
  "parameters": { ... }
}
```

```json
[
  {
    "name": "ask_human",
    "description": "Ask the human a question",
    "mode": "server",
    "parameters": { ... }
  },
  {
    "name": "done",
    "description": "End the session",
    "mode": "server",
    "parameters": { ... }
  }
]
```

**How the session runner uses this:**

1. At session start, discover tools: `phyl-tool-X --spec`
2. Group by executable: if any tool from a binary declares `"server"` mode,
   start it with `--serve` and keep the handle
3. For `"oneshot"` tools, spawn a fresh process per call
4. At session end, close stdin on all server-mode tools → they exit

No probing. No guessing. The spec is the truth.

**Which tools use which mode:**

| Tool | Mode | Why |
|------|------|-----|
| `phyl-tool-bash` | One-shot | Stateless. Each command is independent. |
| `phyl-tool-files` | One-shot | Stateless. Read/write/search. |
| `phyl-tool-mcp` | Server | MCP servers are long-lived processes. |
| `phyl-tool-session` | Server | Handles `ask_human` (blocks) and `done` (signals session end). |

**`phyl-tool-session`** handles session-level operations via NDJSON:

- `ask_human`: waits for the session runner to forward a human answer on
  stdin, then returns it as a tool result. The session runner handles log
  writing and FIFO reading — the tool itself does no file I/O.
- `done`: returns `{"signal":"end_session"}` to tell the runner to finalize.

It runs in server mode because `ask_human` needs to block for minutes (waiting
for the runner to forward an answer). It's pure stdin/stdout — no filesystem
access needed. See the `ask_human` section under Bridges for the full flow.

Tools receive environment variables for context:

| Variable | Value |
|----------|-------|
| `PHYLACTERY_SESSION_ID` | UUID of the current session |
| `PHYLACTERY_SESSION_DIR` | Absolute path to session working directory |
| `PHYLACTERY_KNOWLEDGE_DIR` | Absolute path to knowledge base |
| `PHYLACTERY_HOME` | Absolute path to agent home directory |

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

Sessions can request human attention. The model does this via the `ask_human`
tool (provided by `phyl-tool-session`). When the model calls it, the session
runner writes a question event to the log and blocks, waiting for a response
on the FIFO.

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

A Rust binary in the workspace. Talks to `signal-cli` (or
`signal-cli-rest-api`) as a subprocess. It:

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

| Bridge | Transport | Notes |
|--------|-----------|-------|
| `phyl watch` | Terminal (built-in CLI) | Built into `phyl` binary |
| `phyl-bridge-signal` | Signal Messenger | Rust, in the workspace |
| `phyl-bridge-matrix` | Matrix chat room | Any language (small script) |
| `phyl-bridge-email` | Email (IMAP/SMTP) | Any language (small script) |
| `phyl-bridge-telegram` | Telegram bot | Any language (small script) |
| `phyl-bridge-ntfy` | ntfy.sh push notifications (one-way) | ~30 lines shell |

`phyl-bridge-signal` is a Rust binary in the Cargo workspace because Signal is
the primary external interface and we want reliability. Other bridges are
standalone programs in any language — the bridge protocol is just SSE + HTTP.
They can be installed and run independently.

### Polling: `phyl-poll`

A Rust binary in the workspace. Turns any command into an event source by
running it on an interval and starting a session when the output changes. This
is how the agent reacts to the outside world without dedicated integrations —
new mail, git commits, server health, RSS feeds, anything with a CLI.

`phyl-poll` is a long-lived process (like bridges). It reads `[[poll]]`
sections from `config.toml`, runs each command on its configured interval,
and compares the output to the last known result. On change, it creates a new
session via `POST /sessions` with a prompt that includes the old output, new
output, and a diff.

**Example configuration** (in `config.toml`):

```toml
[[poll]]
name = "github-notifications"
command = "gh"
args = ["api", "/notifications", "--jq", ".[].subject.title"]
interval = 300                      # seconds (5 minutes)
prompt = "Review these new GitHub notifications and summarize what needs my attention."

[[poll]]
name = "server-health"
command = "curl"
args = ["-sf", "https://example.com/health"]
interval = 60
prompt = "The health check output changed. Analyze what's different and whether I should be concerned."

[[poll]]
name = "mailbox"
shell = true                        # run via sh -c instead of exec
command = "notmuch search --output=summary tag:inbox AND tag:unread | head -20"
interval = 120
prompt = "New unread mail arrived. Summarize the new messages."

[[poll]]
name = "slack-mentions"
command = "phyl-tool-mcp"
args = ["--call", "slack", "get_mentions", "{}"]
interval = 180
prompt = "New Slack mentions. Summarize and flag anything that needs a reply."
```

The last example shows `phyl-poll` composing with `phyl-tool-mcp --call` to
poll an MCP server. Any MCP server configured in `config.toml` becomes a
pollable data source — no wrapper scripts needed.

**How it works:**

1. On startup, read all `[[poll]]` rules from `config.toml`
2. Create `$PHYLACTERY_HOME/poll/` directory if it doesn't exist (gitignored)
3. For each rule, on each interval tick:
   a. Run the command, capture stdout (stderr goes to phyl-poll's stderr)
   b. Read the previous output from `poll/<name>.last` (if it exists)
   c. If no previous output: save current output as baseline, skip
   d. If output is identical: skip
   e. If output changed: assemble a session prompt and POST /sessions
   f. Save current output as the new `poll/<name>.last`
4. The session prompt is assembled as:

```
{rule.prompt}

=== PREVIOUS OUTPUT ===
{previous output}

=== CURRENT OUTPUT ===
{current output}

=== DIFF ===
{unified diff of previous → current}
```

The model gets the rule's configured prompt (the "what to do about it"
instruction), both versions of the output (for full context), and a diff
(for quick orientation). The model then has tools available to take action —
it's a normal session.

**State management:**

- Previous outputs stored as plain files in `$PHYLACTERY_HOME/poll/<name>.last`
- The `poll/` directory is gitignored — this is transient operational state,
  not knowledge
- If a `.last` file is deleted, the next poll re-establishes a baseline
  (no false-positive session on restart)
- Command exit code is tracked: non-zero is logged to stderr but not treated
  as a change (avoids spurious sessions from transient failures)

**Scheduling:**

- Each rule runs independently on its own interval
- Initial polls are staggered (100ms apart) to avoid launching all commands
  simultaneously on startup
- If a command takes longer than its interval, the next tick is skipped (no
  pile-up)
- Minimum interval enforced at 10 seconds to prevent accidental abuse

**Failure handling:**

- Command timeout: killed after 30 seconds (configurable per rule)
- Command exits non-zero: log warning, do not update `.last`, do not create
  session
- Daemon unavailable: log error, retry on next interval
- Session creation fails (e.g., max concurrent): log warning, retry on next
  change detection

### Listening: `phyl-listen`

A Rust binary in the workspace. The push complement to `phyl-poll`. Where
polling pulls data on intervals, listening receives events pushed by external
systems — webhooks from GitHub, CI/CD notifications, monitoring alerts,
or any service that can make an HTTP request or emit SSE events.

`phyl-listen` is a long-lived process (like bridges and the poller). It reads
`[listen]` and `[[listen.hook]]` sections from `config.toml`, opens an HTTP
server on a configurable TCP port, and routes incoming requests to session
creation via `POST /sessions` on the daemon API.

**Why a separate binary?** The daemon deliberately listens only on a Unix
socket — no network exposure by design. A webhook receiver must listen on a
TCP port (external systems need to reach it). Putting this in the daemon would
change its security model. A separate binary can crash/restart independently,
can be omitted entirely if not needed, and follows the project's pattern of
small cooperating processes.

**Example configuration** (in `config.toml`):

```toml
[listen]
bind = "127.0.0.1:7890"            # TCP address for webhook receiver
# bind = "0.0.0.0:7890"            # Expose to network (use with caution)

[[listen.hook]]
name = "github"
path = "/hook/github"
secret = "$GITHUB_WEBHOOK_SECRET"   # HMAC-SHA256 verification
prompt = "A GitHub event arrived. Analyze it and take appropriate action."
# Route to event-specific prompts based on a header value
route_header = "X-GitHub-Event"
routes = { push = "Code was pushed. Review the diff and summarize what changed.", pull_request = "A pull request was opened or updated. Review it.", issues = "A new issue was filed. Triage it and suggest next steps." }

[[listen.hook]]
name = "ci-pipeline"
path = "/hook/ci"
secret = "$CI_WEBHOOK_TOKEN"
prompt = "A CI build notification arrived. Check if anything needs attention."

[[listen.hook]]
name = "email-arrived"
path = "/hook/email"
prompt = "A new email arrived. Read it, summarize it, and flag anything that needs a reply."

[[listen.hook]]
name = "monitoring"
path = "/hook/alerts"
prompt = "A monitoring alert fired. Investigate and report what you find."
```

**Event-type routing:**

A single webhook endpoint often receives many event types (e.g., GitHub sends
pushes, PRs, issues, stars all to one URL). The `route_header` + `routes`
fields let you map header values to event-specific prompts:

- `route_header`: which HTTP header to inspect (e.g., `X-GitHub-Event`)
- `routes`: a map of header values to prompts (e.g., `push` → "Review the
  diff...", `pull_request` → "Review the PR...")
- `prompt`: the fallback prompt if the header value isn't in the `routes` map,
  or if `route_header` isn't set

This keeps one webhook URL in the external service while giving the agent
focused instructions per event type. Events whose header value isn't in the
`routes` map still create sessions using the fallback `prompt` — nothing is
silently dropped.

Alternatively, multiple `[[listen.hook]]` entries can share the same `path`.
They're matched in config order (first match wins), each with its own
`filter_header`/`filter_values` and `prompt`. This is more verbose but gives
independent rate limits and dedup settings per event type:

```toml
[[listen.hook]]
name = "github-push"
path = "/hook/github"
secret = "$GITHUB_WEBHOOK_SECRET"
filter_header = "X-GitHub-Event"
filter_values = ["push"]
prompt = "Code was pushed. Review the diff and summarize what changed."

[[listen.hook]]
name = "github-pr"
path = "/hook/github"
secret = "$GITHUB_WEBHOOK_SECRET"
filter_header = "X-GitHub-Event"
filter_values = ["pull_request"]
prompt = "A pull request was opened or updated. Review it."
rate_limit = 5                     # PRs are noisier, tighter limit

[[listen.hook]]
name = "github-catchall"
path = "/hook/github"
secret = "$GITHUB_WEBHOOK_SECRET"
prompt = "A GitHub event arrived. Check what happened."
```

**How it works:**

1. On startup, read `[listen]` config from `config.toml`
2. Bind HTTP server to `listen.bind` address (default `127.0.0.1:7890`)
3. Group hooks by path — multiple hooks can share a path
4. On incoming POST:
   a. Collect all hooks matching the request path (404 if none)
   b. Verify webhook secret if configured (shared across hooks on same path;
      checked once, 401 if invalid)
   c. Walk hooks in config order, check `filter_header`/`filter_values` —
      first match wins. If no filter, it matches unconditionally.
   d. Resolve prompt: if matching hook has `route_header` + `routes`, look up
      the header value in the routes map (fall back to `prompt` if not found)
   e. Check rate limit for this hook (reject with 429 if exceeded)
   f. Check deduplication window (skip if duplicate delivery ID seen recently)
   g. Assemble session prompt from the resolved prompt + request payload
   h. POST /sessions to the daemon API
   i. Return 202 Accepted with the session ID

**Prompt assembly:**

```
{hook.prompt}

=== EVENT ===
Source: {hook.name} (POST {hook.path})
Received: {timestamp}
Headers:
  X-GitHub-Event: push
  X-GitHub-Delivery: abc123

=== PAYLOAD ===
{request body}
```

The model gets the hook's configured prompt (the instruction), relevant
headers, and the full request body. Headers are filtered to those likely
useful (event type, delivery ID, content type) — not the full HTTP header
set.

**Webhook secret verification:**

When `secret` is configured, the listener validates the request body against
an HMAC signature header. Supports the common patterns:

- GitHub: `X-Hub-Signature-256` header, HMAC-SHA256
- GitLab: `X-Gitlab-Token` header, direct comparison
- Generic: configurable header name + HMAC-SHA256

Unsigned requests to a secret-protected hook are rejected with 401.

**Rate limiting:**

Each hook has an independent rate limit to prevent webhook floods from
creating unlimited sessions. Default: 10 sessions per minute per hook.
Configurable per hook:

```toml
[[listen.hook]]
name = "noisy-service"
path = "/hook/noisy"
rate_limit = 3                     # max sessions per minute
prompt = "..."
```

When the rate limit is hit, the listener returns 429 Too Many Requests.
Events are dropped, not queued — webhooks are retried by the sender anyway.

**Deduplication:**

Webhooks are often retried by the sender on timeout. The listener tracks
delivery IDs (from `X-GitHub-Delivery`, `X-Request-Id`, or similar headers)
in a short-lived cache (5 minutes). Duplicate deliveries are acknowledged
with 200 but don't create sessions. The deduplication header is configurable:

```toml
[[listen.hook]]
name = "github"
dedup_header = "X-GitHub-Delivery"  # default: X-Request-Id
```

**Security model:**

- Default bind is `127.0.0.1` (localhost only) — not exposed to the network
  unless explicitly configured
- Only configured paths accept requests (everything else → 404)
- Webhook secret verification (HMAC-SHA256) for authenticated sources
- Rate limiting prevents session floods
- Payload size limit (1 MB default, configurable) prevents abuse
- No path traversal, no query parameters interpreted, no redirects

**SSE subscription: `[[listen.sse]]`**

Subscribe to an external SSE stream and create sessions when events arrive.
This is a persistent connection — the listener holds the connection open and
reacts to each event in real time. Useful for internal APIs, deployment
pipelines, monitoring dashboards, or any service that exposes an SSE endpoint.

```toml
[[listen.sse]]
name = "deploy-events"
url = "https://internal.example.com/events/stream"
prompt = "A deployment event occurred. Check system health."
# Optional: auth header for protected endpoints
headers = { Authorization = "Bearer $DEPLOY_API_TOKEN" }
# Optional: route by SSE event type (the `event:` field in SSE)
route_event = true
routes = { deploy_start = "A deployment started. Monitor it.", deploy_fail = "A deployment failed. Investigate immediately.", deploy_success = "A deployment succeeded. Verify health." }
# Optional: only listen for specific SSE event types (ignore others)
events = ["deploy_start", "deploy_fail", "deploy_success"]

[[listen.sse]]
name = "price-alerts"
url = "https://api.example.com/prices/stream"
prompt = "A price alert triggered. Analyze the change."
rate_limit = 5                     # max sessions per minute
```

How SSE subscription works:

1. On startup, connect to each configured `url` via HTTP GET
2. Parse the SSE stream (standard `text/event-stream` format):
   - `event:` field → event type (used for routing/filtering)
   - `data:` field → event payload (can be multi-line)
   - `id:` field → event ID (used for deduplication and reconnection)
3. For each event:
   a. If `events` is configured, skip events not in the list
   b. Resolve prompt: if `route_event` is true and `routes` has a matching
      event type, use that prompt; otherwise use the fallback `prompt`
   c. Check rate limit (per SSE source)
   d. Check deduplication (by SSE `id:` field, 5-minute cache)
   e. Assemble session prompt and POST /sessions to daemon
4. On connection drop: reconnect after 5 seconds (exponential backoff up
   to 60 seconds). Send `Last-Event-ID` header to resume from where we
   left off (standard SSE reconnection protocol).

Prompt assembly for SSE events:

```
{resolved prompt}

=== EVENT ===
Source: {sse.name} ({sse.url})
Event type: {event type or "message"}
Event ID: {id}
Received: {timestamp}

=== DATA ===
{event data}
```

Connection management:

- Each SSE source runs as an independent tokio task
- Automatic reconnection with exponential backoff (5s → 10s → 20s → 60s max)
- `Last-Event-ID` header sent on reconnection (allows server to replay
  missed events)
- Optional custom headers for authentication (env var expansion in values)
- Connection timeout: 30 seconds for initial connect, then keep-alive based
  on server's `retry:` field (default 5 seconds)
- If the server sends no events for 5 minutes and no SSE comments (`:` keep-
  alive lines), the connection is considered stale and recycled

**File watching: `[[listen.watch]]`**

Watch files or directories for changes using inotify and create sessions
when matching events occur. This turns a directory into an inbox — drop a
file in, the agent processes it.

```toml
[[listen.watch]]
name = "inbox"
path = "/home/user/agent-inbox/"
recursive = true                   # watch subdirectories too (default false)
events = ["create"]                # which events trigger sessions
prompt = "A new file appeared in the inbox. Read it and process the contents."
# Optional: only watch files matching a glob pattern
glob = "*.eml"

[[listen.watch]]
name = "config-changes"
path = "/etc/myapp/"
recursive = true
events = ["modify"]
glob = "*.conf"
debounce = 5                       # seconds — coalesce rapid changes (default 2)
prompt = "A config file was modified. Review the change and check for issues."

[[listen.watch]]
name = "log-errors"
path = "/var/log/myapp/error.log"  # can watch a single file too
events = ["modify"]
prompt = "The error log was updated. Read the new entries and investigate."
```

How file watching works:

1. On startup, set up inotify watches for each configured `path`
2. If `recursive` is true, watch all subdirectories (add watches as new
   dirs are created)
3. For each inotify event:
   a. Check event type against `events` list (`create`, `modify`, `delete`,
      `move_to`, `move_from`)
   b. If `glob` is configured, check the filename against the pattern
      (skip non-matching files)
   c. Debounce: coalesce events for the same file within the `debounce`
      window (default 2 seconds). Editors often create multiple events per
      save (write temp file, rename, chmod). The session is created after
      the debounce window expires with no new events for that file.
   d. Check rate limit (per watch source)
   e. Assemble session prompt and POST /sessions to daemon

Prompt assembly for file events:

```
{watch.prompt}

=== FILE EVENT ===
Source: {watch.name}
Path: {absolute path to file}
Event: {create | modify | delete | move}
Timestamp: {timestamp}
Size: {file size in bytes, if file exists}
```

For `create` and `modify` events on reasonably sized files (< 100 KB),
the file content is included in the prompt:

```
=== FILE CONTENT ===
{file contents, truncated at 100 KB}
```

For larger files or binary files, the prompt just reports the path and
the agent can use `read_file` to access it during the session. For `delete`
events, there's obviously no content to include.

Debouncing:

- Default 2-second debounce window per file path
- Configurable per watch: `debounce = 5` (seconds)
- Multiple events for the same file within the window are coalesced into
  one session. The session gets the final event type (e.g., create then
  modify → modify; create then delete → no session).
- Different files debounce independently
- Minimum debounce: 0 (disabled). Maximum: 60 seconds.

Implementation notes:

- Uses the `inotify` crate (Linux) for efficient kernel-level file watching
- Falls back to polling (stat-based, 1-second interval) on non-Linux platforms
  or if inotify watch limit is exhausted (`/proc/sys/fs/inotify/max_user_watches`)
- Hidden files and directories (starting with `.`) are skipped by default
  unless the glob explicitly matches them
- Symlinks are not followed (watches the link, not the target)
- If the watched directory doesn't exist on startup, log a warning and
  retry every 30 seconds (it might be a mount point that appears later)

**All three listener types in one binary:**

`phyl-listen` hosts webhooks, SSE subscriptions, and file watches in a
single process. They share the daemon connection (Unix socket HTTP client)
and rate limiting infrastructure. Config sections are independent:

```toml
[listen]
bind = "127.0.0.1:7890"           # only needed if webhooks are configured

[[listen.hook]]
# ... webhook configs ...

[[listen.sse]]
# ... SSE subscription configs ...

[[listen.watch]]
# ... file watch configs ...
```

If no `[[listen.hook]]` sections exist, the HTTP server is not started
(no port opened). SSE and watch listeners run regardless. This means you
can use `phyl-listen` purely for file watching or SSE subscription without
opening any ports.

**The trifecta of event sources:**

| Mechanism | Binary | Direction | Trigger |
|-----------|--------|-----------|---------|
| Polling | `phyl-poll` | Pull (agent → world) | Output changed since last check |
| Listening | `phyl-listen` | Push (world → agent) | External system sends event |
| Bridges | `phyl-bridge-*` | Bidirectional | Human sends message |

All three create sessions via `POST /sessions` on the daemon API. The daemon
doesn't need to know how a session was created — it just runs them.

### The `ask_human` Tool

Provided by `phyl-tool-session` (server mode). When the model needs human
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

**Flow — the session runner is the sole FIFO reader:**

1. Session runner dispatches the tool call to `phyl-tool-session` via NDJSON
2. Session runner writes a `question` event to `log.jsonl` (with a unique
   question ID)
3. Session runner enters a `select!` loop: read FIFO **and** read
   `phyl-tool-session` stdout simultaneously
4. When an answer event arrives on the FIFO, session runner forwards it to
   `phyl-tool-session` via its NDJSON stdin:
   `{"id":"tc_1","answer":"yes"}`
5. `phyl-tool-session` receives the forwarded answer and returns the tool
   result: `{"id":"tc_1","output":"Human answered: yes"}`
6. Session runner adds the tool result to history and continues

**Why the session runner forwards answers instead of letting the tool read the
FIFO:** A FIFO has a single read end. If both the session runner and
`phyl-tool-session` read from it, events would be randomly delivered to one or
the other — a race condition. The session runner is the sole reader and routes
events to the appropriate destination (user messages → history, answer events →
`phyl-tool-session`).

If no answer arrives within a timeout (configurable, default 30 minutes), the
session runner sends a timeout signal to `phyl-tool-session`:
`{"id":"tc_1","answer":null,"timeout":true}` — the tool returns
`"No response from human — timed out"` and the model decides how to proceed.

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

`phyl` is the user's single entry point. Session commands talk to the daemon
over a Unix socket. Setup and config commands work directly on files.

```
# Session management (requires running daemon)
phyl session "do the thing"        # Start session, stream output (foreground)
phyl session -d "do the thing"     # Start session, return ID (detached)
phyl ls                            # List sessions
phyl status <id>                   # Session detail
phyl say <id> "new info"           # Inject event into running session
phyl log <id>                      # Tail session log (like tail -f)
phyl stop <id>                     # Kill session
phyl watch                         # Live feed of all sessions, answer questions

# Daemon and services
phyl start                         # Start daemon (foreground)
phyl start -d                      # Start daemon (background, daemonize)
phyl start --all                   # Start all services in foreground (no systemd)

# Initialization and setup
phyl init [path]                   # Initialize agent home directory (XDG default)
phyl init --systemd                # Initialize + install systemd user units
phyl setup systemd                 # Generate/install/enable systemd user units
phyl setup status                  # Show health of all components
phyl setup migrate-xdg             # Move ~/.phylactery to XDG paths

# Configuration
phyl config show                   # Pretty-print resolved config (secrets masked)
phyl config validate               # Check config.toml for errors
phyl config edit                   # Open config.toml in $EDITOR
phyl config add mcp <name>         # Add an MCP server to config
phyl config add poll <name>        # Add a poll rule
phyl config add hook <name>        # Add a webhook listener
phyl config add sse <name>         # Add an SSE subscription
phyl config add watch <name>       # Add a file watch
phyl config add bridge signal      # Configure the Signal bridge
phyl config add-secret <KEY> <VALUE>  # Add a secret to secrets.env
phyl config list-secrets              # List secret keys (values masked)
phyl config remove-secret <KEY>       # Remove a secret
```

Foreground session streams `log.jsonl` to the terminal as it's written.
Ctrl-C detaches (session keeps running). `phyl stop` kills.

`phyl watch` is the primary human interface when using the terminal. It
multiplexes all session activity into a single stream and lets you respond to
agent questions inline.

---

## Project Structure

The code and the agent's data are **separate concerns**. The source tree builds
the binaries. The agent's home directory holds its state. They never share a
git repo.

### Source Tree (this repo)

```
phylactery/                     # Code repo — you're looking at it
├── Cargo.toml                  # Workspace manifest
├── PLAN.md                     # This document
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
│   ├── phyl-tool-session/      # Session tools: ask_human, done (server mode)
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-tool-mcp/          # MCP bridge tool (server mode)
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-bridge-signal/     # Signal Messenger bridge
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── phyl-poll/              # Poller (command-based event source)
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── phyl-listen/            # Listener (webhooks, incoming events)
│       ├── Cargo.toml
│       └── src/main.rs
└── README.md
```

`cargo build --release` produces binaries. Install to `~/.local/bin/` or
`/usr/local/bin/`. The source tree has no runtime role.

### Agent Home (separate git repo, created by `phyl init`)

New installs use XDG paths by default. Legacy `~/.phylactery` continues
to work. `$PHYLACTERY_HOME` is the source of truth.

```
~/.local/share/phylactery/      # $PHYLACTERY_HOME (new default)
├── .git/                       # Its own git repo for knowledge + state
├── config.toml                 # Runtime configuration (git-tracked)
├── secrets.env                 # API keys, credentials (gitignored, 0600)
├── LAW.md                      # Agent rules (user-authored, immutable)
├── JOB.md                      # Agent job description (user-authored, immutable)
├── SOUL.md                     # Agent identity (agent-authored, evolving)
├── knowledge/                  # Git-tracked knowledge base
│   ├── INDEX.md                # Agent-maintained table of contents
│   ├── contacts/
│   ├── projects/
│   ├── preferences/
│   └── journal/
├── poll/                       # Poll state files (gitignored)
└── sessions/                   # Per-session working directories (gitignored)
    └── .gitignore              # Ignore everything under sessions/

~/.config/phylactery/           # XDG config directory
└── config.toml -> ~/.local/share/phylactery/config.toml  # Symlink

~/.config/systemd/user/         # Systemd user units (generated by phyl setup systemd)
├── phylactd.service
├── phyl-poll.service
├── phyl-listen.service
└── phyl-bridge-signal.service
```

This is the agent's home directory. It's initialized once with `phyl init`,
which creates the directory, initializes the git repo, creates the seed
structure, writes a default `config.toml`, and creates an empty `secrets.env`.
Everything the agent knows and remembers lives here.

The `$PHYLACTERY_HOME` environment variable points to it (default
`~/.local/share/phylactery`; legacy `~/.phylactery` still works). All
binaries look here for config, LAW, JOB, SOUL, and knowledge. The daemon
and session runner both reference it.

**Why separate?** The code repo has releases, branches, CI. The agent's home
has journal entries, contact notes, and SOUL reflections. Different lifecycles,
different authors, different audiences. Mixing them would be like committing
`/var/log` into your app's source tree.

---

## Configuration

`$PHYLACTERY_HOME/config.toml`:

```toml
[daemon]
socket = "$XDG_RUNTIME_DIR/phylactery.sock"

[session]
timeout_minutes = 60
max_concurrent = 4
model = "phyl-model-claude"     # Name or path of model adapter binary

[model]
context_window = 200000         # Approximate token limit
compress_at = 0.8               # Summarize history at 80% capacity

[git]
auto_commit = true
# remote = "origin"             # Optional: auto-push after commits

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

[bridge.signal]
phone = "+1234567890"           # Agent's Signal number
owner = "+0987654321"           # Owner's number (only accept from this)
signal_cli = "signal-cli"       # Path to signal-cli binary

# Poll rules (used by phyl-poll)
[[poll]]
name = "github-notifications"
command = "gh"
args = ["api", "/notifications", "--jq", ".[].subject.title"]
interval = 300                      # seconds between polls
prompt = "Review these GitHub notifications and summarize what needs attention."

[[poll]]
name = "server-health"
command = "curl"
args = ["-sf", "https://example.com/health"]
interval = 60
prompt = "The health check output changed. Analyze what's different."
# timeout = 10                      # Optional: command timeout in seconds (default 30)

# Incoming event listeners (used by phyl-listen)
[listen]
bind = "127.0.0.1:7890"            # TCP address for webhook receiver

[[listen.hook]]
name = "github"
path = "/hook/github"
secret = "$GITHUB_WEBHOOK_SECRET"   # HMAC-SHA256 verification
prompt = "A GitHub event arrived. Analyze it."          # fallback prompt
route_header = "X-GitHub-Event"                         # route by event type
routes = { push = "Code was pushed. Review the diff.", pull_request = "A PR was opened. Review it.", issues = "An issue was filed. Triage it." }

[[listen.hook]]
name = "email-arrived"
path = "/hook/email"
prompt = "A new email arrived. Summarize it and flag anything that needs a reply."

[[listen.hook]]
name = "ci-pipeline"
path = "/hook/ci"
secret = "$CI_WEBHOOK_TOKEN"
prompt = "A CI build notification arrived. Check if anything needs attention."

# SSE subscriptions (used by phyl-listen)
[[listen.sse]]
name = "deploy-events"
url = "https://internal.example.com/events/stream"
headers = { Authorization = "Bearer $DEPLOY_API_TOKEN" }
prompt = "A deployment event occurred. Check system health."
route_event = true
routes = { deploy_fail = "A deployment failed. Investigate immediately." }

# File watches (used by phyl-listen)
[[listen.watch]]
name = "inbox"
path = "/home/user/agent-inbox/"
events = ["create"]
glob = "*.eml"
prompt = "A new email file appeared. Read it and process the contents."

[[listen.watch]]
name = "config-changes"
path = "/etc/myapp/"
recursive = true
events = ["modify"]
glob = "*.conf"
debounce = 5
prompt = "A config file was modified. Review the change and check for issues."
```

Tools are discovered from `$PATH` — any executable named `phyl-tool-*` is a
tool. Install them alongside the other binaries. No separate tools_path config
needed. This is how Unix does it: `git-*` subcommands, `docker-*` plugins.

### Secrets

`$PHYLACTERY_HOME/secrets.env` holds API keys and credentials. Simple
`KEY=VALUE` format, one per line. Comments start with `#`. Blank lines
ignored. Created by `phyl init` (empty, `chmod 600`), gitignored.

```
# Secrets for phylactery — do not commit
BRAVE_API_KEY=sk-abc123
GITHUB_WEBHOOK_SECRET=whsec_xyz
CI_WEBHOOK_TOKEN=tok_456
```

`config.toml` references these as `$VAR_NAME` — they're resolved at runtime
from the process environment. `phylactd`, `phyl-poll`, and `phyl-listen`
each load `secrets.env` on startup. Systemd units reference it via
`EnvironmentFile=`.

Managed with `phyl config add-secret`, `list-secrets`, `remove-secret`.

---

## The Agentic Loop (phyl-run)

This is the heart of the system. It lives in `phyl-run` and is deliberately
straightforward:

```
phyl-run --session-dir ./sessions/<uuid> --prompt "do the thing"

 1. Redirect stderr to sessions/<uuid>/stderr.log
 2. Write PID to sessions/<uuid>/pid
 3. Read config.toml
 4. Read LAW.md, JOB.md, SOUL.md, knowledge/INDEX.md
 5. Assemble system prompt from template (see System Prompt Template)
 6. Discover tools:
    for each phyl-tool-* on $PATH:
      run `phyl-tool-X --spec` → collect tool schemas + mode
 7. Start server-mode tools:
    for each tool with mode=server:
      spawn `phyl-tool-X --serve` (with sandbox from spec if declared)
      keep stdin/stdout handles for NDJSON communication
 8. Create events FIFO, open with O_RDWR | O_NONBLOCK
 9. Initialize history = [system_prompt, user_prompt]
10. Loop:
    a. Build model request: { messages: history, tools: tool_schemas }
    b. Pipe to model adapter stdin, read response from stdout
    c. Parse model response → append assistant message to history
    d. Write assistant entry to log.jsonl
    e. If response has tool_calls:
       - One-shot tools: spawn phyl-tool-X per call (parallel, sandboxed
         per spec), pipe call on stdin, read result from stdout
       - Server-mode tools: send NDJSON request on existing stdin handle
       - select! on: all pending tool stdout + FIFO
         (FIFO answers are forwarded to the waiting server-mode tool)
       - Collect all tool results, append to history, write to log.jsonl
       - If any result has signal "end_session": goto finalize
       - Else: continue loop (goto a)
    f. If no tool_calls (model just spoke):
       - Poll FIFO for new events
       - If user events: append as user messages, continue loop
       - If nothing after brief wait: the model is done talking and no
         new input has arrived — this shouldn't happen in normal flow
         (model should call done), treat as implicit done → goto finalize
    g. Check cumulative timeout → exit with error if exceeded
11. Finalize (SOUL.md reflection):
    a. Close stdin on server-mode tools → they exit
    b. flock --exclusive $PHYLACTERY_HOME/.soul.lock
    c. Re-read SOUL.md from disk (not the version loaded at session start)
    d. Invoke model adapter ONE more time with:
       - history (for context on what happened this session)
       - Current SOUL.md content
       - Prompt: "Reflect on this session. Here is your current SOUL.md.
         Output an updated version — under 2000 words, present tense,
         living self-portrait not a journal. Output ONLY the new content."
       - tools: [] (no tools — just text output)
    e. Write model output to SOUL.md on disk
    f. flock --exclusive $PHYLACTERY_HOME/.git.lock
    g. git add SOUL.md && git commit -m "soul: reflect on session <uuid>"
    h. Release .git.lock
    i. Release .soul.lock
    j. If SOUL.md > 3000 words: truncate (keep first + last sections), warn
12. Write final done entry to log.jsonl
13. Exit (clean up FIFO, remove PID file)
```

Each model invocation is a **separate process**. One-shot tool calls are
separate processes too. Server-mode tools (phyl-tool-session, phyl-tool-mcp)
are long-lived but isolated — they communicate only via NDJSON on stdin/stdout.
If the model adapter crashes, that invocation fails and the session can retry
or report the error. If a one-shot tool hangs, it can be killed independently.
If a server-mode tool dies, the session runner detects the broken pipe and
fails gracefully.

---

## Implementation Order

Each phase produces something you can run and test.

### Phase 1: Core Types + Skeleton

- [ ] `cargo init` workspace with `phyl-core`
- [ ] Define shared types in `phyl-core`: `Message`, `ToolCall`, `ToolSpec`
      (with `mode` field), `ModelRequest`, `ModelResponse`, `ToolInput`,
      `ToolOutput`, `LogEntry`, `ServerResponse` (with `signal` field)
- [ ] All types derive `Serialize`/`Deserialize`
- [ ] Stub the other crates with `fn main() { todo!() }`
- [ ] Implement `phyl init`: create `$PHYLACTERY_HOME` with git repo, seed
      `config.toml`, `LAW.md`, `JOB.md`, `SOUL.md` ("I am new."),
      `knowledge/` structure, `sessions/.gitignore`
- [ ] Verify: `cargo build` succeeds, produces multiple binaries

### Phase 2: Tool Protocol

Build two tools and verify the protocol works end-to-end from the command line.

- [ ] Implement `phyl-tool-bash`: `--spec` (with `"mode":"oneshot"`) and
      invocation mode. chdir to `$PHYLACTERY_SESSION_DIR/scratch/`, enforce
      timeout.
- [ ] Implement `phyl-tool-files`: read_file, write_file, search_files
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
      - Build system prompt from LAW.md + JOB.md + SOUL.md + knowledge/INDEX.md
      - Start server-mode tools (phyl-tool-session, phyl-tool-mcp)
      - Run the agentic loop: dispatch to one-shot or server-mode tools
      - Write to log.jsonl
      - Finalization step: SOUL.md reflection + done
      - PID file for daemon crash recovery
      - On exit: close stdin on server-mode tools → they shut down
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
      - On `--serve`: run NDJSON server loop, route tool calls to correct MCP
        server
      - On `--call <server> <tool> <args>`: one-shot mode — start one MCP
        server, make one tool call, print result to stdout, shut down. Reuses
        the same MCP client logic as `--serve`.
      - JSON-RPC over stdio to MCP servers
- [ ] Test: configure an MCP server, invoke a tool through the bridge
- [ ] Test: `phyl-tool-mcp --call <server> <tool> '<json>'` from command line

### Phase 8: Knowledge Base + Git

- [ ] Implement auto-commit in `phyl-tool-files` for writes under `knowledge/`
- [ ] Add `search_files` tool (wraps `grep -r`)
- [ ] Seed `knowledge/` directory structure
- [ ] Add knowledge base summary generation to session startup

### Phase 9: Human Attention System

- [ ] Implement `phyl-tool-session` (server mode tool):
      - `--spec`: return schemas for `ask_human` and `done`
      - `--serve`: run NDJSON server loop on stdin/stdout
      - `ask_human` handler: wait for forwarded answer on stdin (the session
        runner handles log writing, FIFO reading, and answer forwarding)
      - `done` handler: return `{"signal":"end_session"}`
      - No file I/O — pure NDJSON
- [ ] Add `GET /feed` SSE endpoint to daemon:
      - Tail all active session logs
      - Filter for attention-worthy events (question, done, error)
      - Stream as SSE to connected clients
- [ ] Implement `phyl watch` CLI command:
      - Connect to `GET /feed`
      - Display events, accept typed responses
      - Route responses to correct session via `POST /sessions/:id/events`

### Phase 10: Signal Bridge

- [x] Implement `phyl-bridge-signal`:
      - Connect to daemon `GET /feed` via Unix socket
      - Send questions/notifications as Signal messages via `signal-cli`
      - Listen for inbound Signal messages
      - Route replies back to sessions via `POST /sessions/:id/events`
      - Accept new session requests from inbound messages
      - Only accept messages from configured owner number
- [x] Config: signal phone numbers, signal-cli path
- [ ] Test: end-to-end question/answer cycle over Signal

### Phase 11: Polling

A generic mechanism for "poll a command, and if the output changed, start a
session with the diff and an action prompt." This turns any command-line tool
into an event source for the agent.

- [ ] Implement `phyl-poll`:
      - Standalone binary, similar to bridges. Runs as a long-lived process
        alongside the daemon.
      - Read `[[poll]]` configs from `$PHYLACTERY_HOME/config.toml`
      - For each poll rule, run the configured command on the configured
        interval
      - Compare output to last known result (stored in
        `$PHYLACTERY_HOME/poll/<name>.last`)
      - On first run (no `.last` file): store the result, do not start a
        session (establishes baseline)
      - On change: POST /sessions to the daemon with a prompt assembled from
        the rule's `prompt` template plus a diff of old → new output
      - On no change: do nothing, sleep until next interval
      - Stagger initial poll execution to avoid thundering herd on startup
      - Graceful shutdown on Ctrl-C (`tokio::signal::ctrl_c()`)
      - Log poll activity to stderr (same convention as other binaries)
- [ ] Config types: `PollConfig` with `name`, `command`, `args`, `interval`,
      `prompt`, optional `env` and `shell` (default: run command directly;
      `shell = true` runs via `sh -c`)
- [ ] State directory: `$PHYLACTERY_HOME/poll/` (gitignored, created on
      first run)
- [ ] Prompt assembly: the session prompt includes the rule's `prompt`
      template, the previous output, the new output, and a unified diff.
      The model gets full context to decide what to do.
- [ ] Add `phyl-poll` to workspace `Cargo.toml`
- [ ] Unit tests: config parsing, diff generation, prompt assembly, state
      file read/write, interval scheduling
- [ ] Test: configure a poll rule, verify session creation on change

### Phase 12: Incoming Event Listener

Three listener types in one binary: webhooks (HTTP POST from external
systems), SSE subscription (persistent connection to external event streams),
and file watching (inotify on local directories). All create sessions via
`POST /sessions` on the daemon API.

**12a: Webhooks**

- [ ] Implement webhook listener in `phyl-listen`:
      - Read `[listen]` and `[[listen.hook]]` configs from
        `$PHYLACTERY_HOME/config.toml`
      - Bind HTTP server to `listen.bind` address (default
        `127.0.0.1:7890`) using `axum` + `tokio`. Skip if no hooks configured.
      - Group hooks by path — multiple hooks can share a path (matched in
        config order, first filter match wins)
      - On incoming POST to a configured path:
        - Verify webhook secret (HMAC-SHA256) if `secret` is configured
        - Walk hooks for this path in config order, apply
          `filter_header`/`filter_values` — first match wins
        - Resolve prompt: if hook has `route_header` + `routes`, look up
          header value in routes map; fall back to `prompt` field
        - Check rate limit (default 10 sessions/minute per hook)
        - Check deduplication cache (skip duplicate delivery IDs)
        - Assemble session prompt from resolved prompt + request payload
        - POST /sessions to daemon API (via Unix socket, same client as CLI)
        - Return 202 Accepted with session ID
      - Return 404 for unconfigured paths, 401 for bad signatures, 429 for
        rate limit exceeded
- [ ] Config types: `ListenHookConfig` with `name`, `path`, `prompt`,
      optional `secret`, `filter_header`, `filter_values`, `rate_limit`,
      `dedup_header`, `max_body_size`, `route_header`,
      `routes` (HashMap<String, String>)
- [ ] Webhook secret verification: HMAC-SHA256 against
      `X-Hub-Signature-256` (GitHub-style), with configurable header name
      for other providers. Environment variable expansion in `secret` field.
- [ ] Webhook prompt assembly: hook's prompt + source info (hook name,
      path, timestamp) + relevant headers + full request body
- [ ] Webhook deduplication: in-memory cache of delivery IDs (from
      configurable header, default `X-Request-Id`). 5-minute TTL.
- [ ] Payload size limit: 1 MB default, configurable per hook
- [ ] Event-type routing: resolve prompt from `route_header` + `routes`
      map when present, fall back to `prompt` for unmatched header values.
      Multiple hooks on same path matched in config order.
- [ ] Unit tests: config parsing, HMAC verification, rate limiting,
      deduplication, prompt assembly, route matching, header filtering,
      event-type prompt resolution (routes map + fallback)
- [ ] Test: configure a webhook hook, POST to it, verify session creation
- [ ] Test: configure event-type routes, verify different prompts per event

**12b: SSE Subscription**

- [ ] Implement SSE subscription listener in `phyl-listen`:
      - Read `[[listen.sse]]` configs from `$PHYLACTERY_HOME/config.toml`
      - For each SSE source, spawn a tokio task that connects to the URL
        and parses the `text/event-stream` format
      - Parse SSE fields: `event:` (type), `data:` (payload, multi-line),
        `id:` (deduplication/reconnection), `retry:` (reconnect interval)
      - For each event:
        - Filter by `events` list if configured (skip non-matching types)
        - Resolve prompt: if `route_event` is true and event type matches
          a key in `routes`, use that prompt; else use fallback `prompt`
        - Check rate limit (per SSE source, default 10/minute)
        - Check deduplication (by SSE `id:` field, 5-minute cache)
        - Assemble session prompt and POST /sessions to daemon
      - Automatic reconnection with exponential backoff (5s → 10s → 20s →
        60s max). Send `Last-Event-ID` header on reconnect.
      - Stale connection detection: recycle if no events or keep-alive
        comments for 5 minutes
- [ ] Config types: `ListenSseConfig` with `name`, `url`, `prompt`,
      optional `headers` (HashMap<String, String> with env var expansion),
      `events` (Vec<String>), `route_event` (bool), `routes`
      (HashMap<String, String>), `rate_limit`
- [ ] SSE prompt assembly: resolved prompt + source info (name, URL,
      event type, event ID, timestamp) + event data
- [ ] SSE reconnection: exponential backoff, `Last-Event-ID` header,
      respect server `retry:` field
- [ ] Unit tests: SSE frame parsing (event, data, id, multi-line data,
      comments), event-type routing, reconnection logic, config parsing
- [ ] Test: connect to an SSE endpoint, verify session creation on event

**12c: File Watching**

- [ ] Implement file watch listener in `phyl-listen`:
      - Read `[[listen.watch]]` configs from `$PHYLACTERY_HOME/config.toml`
      - Set up inotify watches for each configured `path` (directory or file)
      - If `recursive` is true, watch all subdirectories and add watches
        for newly created directories
      - For each inotify event:
        - Check event type against configured `events` list (`create`,
          `modify`, `delete`, `move_to`, `move_from`)
        - If `glob` is configured, match filename against pattern
        - Debounce: coalesce events for the same file path within the
          `debounce` window (default 2 seconds). After the window expires
          with no new events for that file, create the session.
        - Check rate limit (per watch source, default 10/minute)
        - Assemble session prompt and POST /sessions to daemon
      - Include file content in prompt for create/modify events on small
        files (< 100 KB). Omit for large/binary/deleted files.
      - Skip hidden files/directories (starting with `.`) unless glob
        explicitly matches
      - If watched path doesn't exist, log warning and retry every 30s
- [ ] Config types: `ListenWatchConfig` with `name`, `path`, `prompt`,
      `recursive` (bool, default false), `events` (Vec<String>),
      optional `glob`, `debounce` (seconds, default 2, min 0, max 60),
      `rate_limit`
- [ ] File watch prompt assembly: watch's prompt + file event info (path,
      event type, timestamp, size) + file content (for small files on
      create/modify events)
- [ ] Debouncing: per-file-path timer, coalesce rapid events (editors
      generate multiple events per save). Final event type wins (create
      then modify → modify; create then delete → no session).
- [ ] Platform support: inotify on Linux (via `inotify` crate), fall back
      to stat-based polling (1-second interval) on other platforms or if
      inotify watch limit is exhausted
- [ ] Unit tests: event filtering, glob matching, debounce coalescing,
      prompt assembly with/without file content, config parsing
- [ ] Test: watch a directory, create a file, verify session creation

**Shared infrastructure (all listener types)**

- [ ] Add `phyl-listen` to workspace `Cargo.toml`
- [ ] `ListenConfig` with `bind` (optional — only for webhooks),
      `hook` (Vec), `sse` (Vec), `watch` (Vec)
- [ ] Shared rate limiting: in-memory sliding window, configurable per
      source. Default 10 sessions/minute. Returns 429 (webhooks) or
      drops event with log warning (SSE/watch).
- [ ] Shared daemon client: Unix socket HTTP client for POST /sessions,
      same pattern as `phyl` CLI client module
- [ ] Graceful shutdown on Ctrl-C (`tokio::signal::ctrl_c()`) — close
      HTTP server, disconnect SSE streams, remove inotify watches
- [ ] HTTP server only started if `[[listen.hook]]` sections exist; SSE
      and watch listeners run independently of whether the HTTP server
      is active
- [ ] Log activity to stderr (same convention as other binaries)
- [ ] Integration test: run all three listener types simultaneously

### Phase 13: Setup, Configuration, and Service Management

All the long-lived processes (daemon, poller, listener, bridges) need to be
installed, configured, and kept running. This phase adds subcommands to `phyl`
that handle first-time setup, ongoing configuration management, systemd
integration, and operational health checks.

**XDG-compliant paths (new installs):**

New installs use XDG paths by default:

| Purpose | Path |
|---------|------|
| Config | `~/.config/phylactery/` (symlink to `$PHYLACTERY_HOME/config.toml`) |
| Data (agent home) | `~/.local/share/phylactery/` = `$PHYLACTERY_HOME` |
| Runtime (socket, PID) | `$XDG_RUNTIME_DIR/phylactery.sock`, `phylactd.pid` |
| Binaries | `~/.local/bin/` (already on most users' `$PATH`) |
| Systemd units | `~/.config/systemd/user/` |
| Secrets | `$PHYLACTERY_HOME/secrets.env` (gitignored) |

`config.toml` stays inside `$PHYLACTERY_HOME` so it's versioned with the
agent's git repo. The `~/.config/phylactery/` directory contains a symlink
to it, satisfying tools that look in `$XDG_CONFIG_HOME`. Legacy installs
at `~/.phylactery` continue to work — `$PHYLACTERY_HOME` is the source of
truth, and its default just changes from `~/.phylactery` to
`~/.local/share/phylactery` for new installs. A `phyl setup migrate-xdg`
command handles moving an existing `~/.phylactery` install to XDG paths.

**Secrets file:**

`$PHYLACTERY_HOME/secrets.env` is a simple `KEY=VALUE` file for API keys,
webhook secrets, and other credentials. It is:

- Added to `.gitignore` by `phyl init` (never committed)
- Loaded by `phylactd` on startup and exported to child processes
- Referenced by systemd units via `EnvironmentFile=`
- Readable only by the owning user (`chmod 600`)

`config.toml` references secrets as `$VAR_NAME`. Resolution happens at
runtime from the process environment (which includes secrets.env values).
This keeps secrets out of version control while keeping config.toml
readable and diffable.

Format:
```
# Secrets for phylactery — do not commit
BRAVE_API_KEY=sk-abc123
GITHUB_WEBHOOK_SECRET=whsec_xyz
CI_WEBHOOK_TOKEN=tok_456
DEPLOY_API_TOKEN=bearer_789
```

`phyl config add-secret <KEY> <VALUE>` appends to this file.
`phyl config show` displays resolved config with secret values masked
(`BRAVE_API_KEY=sk-a•••••`).

**Systemd user units:**

`phyl setup systemd` generates and installs unit files for all configured
services. Each service gets its own unit, with dependency ordering:

```ini
# ~/.config/systemd/user/phylactd.service
[Unit]
Description=Phylactery daemon
After=default.target

[Service]
Type=simple
ExecStart=%h/.local/bin/phylactd
Environment=PHYLACTERY_HOME=%h/.local/share/phylactery
EnvironmentFile=%h/.local/share/phylactery/secrets.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

```ini
# ~/.config/systemd/user/phyl-poll.service
[Unit]
Description=Phylactery poller
After=phylactd.service
Requires=phylactd.service

[Service]
Type=simple
ExecStart=%h/.local/bin/phyl-poll
Environment=PHYLACTERY_HOME=%h/.local/share/phylactery
EnvironmentFile=%h/.local/share/phylactery/secrets.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Same pattern for `phyl-listen.service` and `phyl-bridge-signal.service`.
Units are only generated for services that have configuration — if there
are no `[[poll]]` sections, no `phyl-poll.service` is created.

`phyl setup systemd` also runs `systemctl --user daemon-reload`,
`systemctl --user enable`, and `systemctl --user start` for each
generated unit. Re-running it is idempotent — it regenerates units from
current config and restarts changed services.

**`phyl start --all` (non-systemd fallback):**

For development, containers, or systems without systemd: `phyl start --all`
forks and manages all configured services in foreground. It starts the
daemon first, waits for the socket to appear, then starts poller, listener,
and bridges. Ctrl-C sends SIGTERM to all children and waits for clean
shutdown. This is an alternative to systemd, not a replacement — it doesn't
handle auto-restart or boot persistence.

**New CLI subcommands:**

```
phyl setup systemd                 # Generate, install, enable systemd user units
phyl setup status                  # Show health of all components
phyl setup migrate-xdg             # Move ~/.phylactery to XDG paths
phyl start --all                   # Start all services in foreground (no systemd)

phyl config show                   # Pretty-print resolved config (secrets masked)
phyl config validate               # Check config.toml for errors
phyl config edit                   # Open config.toml in $EDITOR

phyl config add mcp <name>         # Add an MCP server
phyl config add poll <name>        # Add a poll rule
phyl config add hook <name>        # Add a webhook listener
phyl config add sse <name>         # Add an SSE subscription
phyl config add watch <name>       # Add a file watch
phyl config add bridge signal      # Configure the Signal bridge

phyl config add-secret <KEY> <VALUE>  # Add a secret to secrets.env
phyl config list-secrets              # List secret keys (values masked)
phyl config remove-secret <KEY>       # Remove a secret
```

**`phyl config add` — interactive configuration helpers:**

Each `phyl config add` subcommand collects the required fields for its
config section, validates them, and appends to `config.toml`. They work
in two modes:

- **Interactive** (default when stdin is a TTY): prompts for each field
  with sensible defaults. Example: `phyl config add poll check-email`
  asks for command, args, interval, and prompt.
- **Flag-driven** (for scripting): all fields as flags. Example:
  `phyl config add poll check-email --command "notmuch" --args "search tag:new" --interval 300 --prompt "New emails arrived."`.

Validation includes:
- Checking that referenced commands exist on `$PATH`
- Checking that `$VAR_NAME` references in secrets have corresponding
  entries in `secrets.env` (warns if missing)
- Checking that webhook paths don't collide
- Checking that poll/watch names are unique

**`phyl setup status` — operational health check:**

Displays a summary of system state:

```
Phylactery Status
─────────────────
  Home:     ~/.local/share/phylactery (XDG)
  Config:   config.toml (valid, 3 warnings)
  Secrets:  secrets.env (4 keys)

Services
────────
  phylactd          running  (pid 1234, uptime 3d 2h)
  phyl-poll         running  (pid 1235, 2 poll rules, last poll 30s ago)
  phyl-listen       running  (pid 1236, 1 webhook, 1 SSE, 0 watches)
  phyl-bridge-signal stopped  (not configured)

Sessions
────────
  active: 1   completed today: 7   failed: 0
```

This talks to the daemon API for session info, checks PID files and
systemd unit status for service health, and parses config for
completeness.

**`phyl setup migrate-xdg` — legacy migration:**

For users who started with `~/.phylactery` and want XDG paths:

1. Stop all running services
2. Move `~/.phylactery` to `~/.local/share/phylactery`
3. Create `~/.config/phylactery/` with symlink to config.toml
4. Update systemd units if they exist
5. Update `$PHYLACTERY_HOME` in shell profile (or print instructions)
6. Start services

Refuses to run if services are active unless `--force` is passed.

**Enhanced `phyl init`:**

`phyl init` gains awareness of XDG paths and optional first-time setup:

- Default path is now `~/.local/share/phylactery` (was `~/.phylactery`)
- Still accepts `phyl init [path]` to override
- Creates `secrets.env` (empty, chmod 600, gitignored)
- Creates `~/.config/phylactery/` symlink
- After creating the agent home, offers next steps:
  ```
  Agent home initialized at ~/.local/share/phylactery

  Next steps:
    phyl config edit              # Edit LAW.md, JOB.md, config.toml
    phyl config add mcp ...       # Add tool servers
    phyl setup systemd            # Install as systemd user services
    phyl start                    # Or just start the daemon
  ```
- `phyl init --systemd` combines init + systemd setup in one command

**Implementation checklist:**

- [ ] Secrets file infrastructure:
      - `phyl init` creates `secrets.env` (empty, `chmod 600`)
      - Add `secrets.env` to `.gitignore` template
      - `phylactd` loads `secrets.env` on startup, exports to child env
      - `phyl-poll` and `phyl-listen` also load `secrets.env` directly
        (they are independent processes, not daemon children)
- [ ] `phyl config add-secret`, `list-secrets`, `remove-secret`:
      parse and modify `secrets.env`
- [ ] `phyl config show`: read config.toml, resolve `$VAR` references
      from environment + secrets.env, pretty-print with masked secrets
- [ ] `phyl config validate`: parse config.toml, check for missing
      required fields, validate command paths exist, warn on
      unreferenced secrets and missing secret references
- [ ] `phyl config edit`: exec `$EDITOR` on `$PHYLACTERY_HOME/config.toml`,
      run validation after editor exits
- [ ] `phyl config add mcp|poll|hook|sse|watch|bridge`: interactive
      prompts (TTY) or flag-driven, append to config.toml, validate
- [ ] `phyl setup systemd`:
      - Generate unit files from config (only for configured services)
      - Resolve `$PHYLACTERY_HOME` to absolute path in unit files
      - Install to `~/.config/systemd/user/`
      - `systemctl --user daemon-reload && enable && start`
      - Idempotent: regenerate and restart on re-run
- [ ] `phyl setup status`:
      - Check daemon reachability (socket exists, GET /health)
      - Check systemd unit status or PID files
      - Parse config for service inventory
      - Show session summary from daemon API
- [ ] `phyl start --all`:
      - Fork daemon, wait for socket, fork poller/listener/bridges
      - Forward SIGTERM to all children on Ctrl-C
      - Only start services that have configuration
- [ ] Update `phyl init`:
      - Default to `~/.local/share/phylactery`
      - Create `secrets.env` and `.config/phylactery/` symlink
      - `--systemd` flag for combined init + service install
      - Print next-steps guidance after init
- [ ] `phyl setup migrate-xdg`:
      - Stop services, move directory, create symlinks, update units
      - Refuse if services running without `--force`
- [ ] Unit tests: config validation rules, secrets file parsing,
      systemd unit generation, XDG path resolution, config add
      serialization

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

## Open Design Questions

Issues that need concrete answers before or during implementation.

### Git Concurrency

Multiple sessions can write to `knowledge/` and `SOUL.md` simultaneously.

**Knowledge base writes: pull-rebase-commit retry loop.**

`phyl-tool-files` uses this sequence for any write under `knowledge/`:

1. `flock $PHYLACTERY_HOME/.git.lock`
2. `git pull --rebase` (if remote configured)
3. Write file
4. `git add <file> && git commit -m "..."`
5. Release lock

The `flock` serializes all git operations. No concurrent git commands, no
merge conflicts, no races. `flock` is a standard POSIX primitive — it's how
`apt` and `dpkg` handle concurrent access. The lock file lives at
`$PHYLACTERY_HOME/.git.lock`.

Lock is held only for the duration of the git operation (milliseconds), not
the entire tool call. Multiple sessions can do non-git work in parallel; they
only serialize when touching the repo.

**SOUL.md writes: serialized finalization.**

The SOUL.md race is worse than a git conflict — two sessions reflecting against
a stale version produces incoherent identity. Solution: serialize at the
application level.

The finalization step:

1. `flock --exclusive $PHYLACTERY_HOME/.soul.lock`
2. Read current SOUL.md from disk (not the version loaded at session start)
3. Invoke model with current SOUL.md + "reflect on this session..."
4. Write new SOUL.md
5. `git add SOUL.md && git commit`
6. Release lock

Only one session can finalize at a time. The second session waits, then reads
the first session's updated SOUL.md before reflecting. This means each
reflection builds on the previous one — no content is lost, no staleness.

The lock contention is minimal — finalization is the last thing a session does,
and it takes seconds (one model call + one git commit).

**Lock ordering:** When both locks are needed (finalization commits SOUL.md),
always acquire `.soul.lock` first, then `.git.lock` inside it. No code
acquires them in the reverse order, so deadlock is impossible. `phyl-tool-files`
only uses `.git.lock`. The session runner's finalization uses `.soul.lock`
then `.git.lock`.

### The ask_human / Long-Lived Tool Problem

`ask_human` doesn't fit the one-shot tool model because it needs to block for
minutes waiting for a human response. MCP has a similar problem — MCP servers
are stateful long-lived processes.

**Decision: Unified server mode protocol.** Rather than special-casing
individual tools in the session runner, the tool protocol itself supports two
modes: one-shot (spawn, call, exit) and server (stay alive, handle multiple
NDJSON calls). See the Tool Protocol section above.

`ask_human` and `done` live in `phyl-tool-session`, which runs in server mode.
MCP tools live in `phyl-tool-mcp`, which also runs in server mode. Simple tools
like `bash` and `files` remain one-shot. The session runner handles both modes
transparently — no special cases.

### Session Directory Lifecycle

**Decision: Session working directories are NOT git-tracked.** They live under
`sessions/` which is gitignored. They contain:

- `log.jsonl` — conversation log
- `events` — named pipe (FIFO) for input
- `scratch/` — arbitrary files the agent creates during the session
- `pid` — PID file for daemon crash recovery
- `stderr.log` — operational log (phyl-run redirects its own stderr here)

**Cleanup policy:** Session dirs for completed sessions are retained for 7 days
(configurable), then deleted by the daemon. The daemon checks on startup and
periodically. Session logs can be archived before deletion (e.g., moved to
`knowledge/sessions/` if the agent decides they're worth keeping).

### MCP Server Lifecycle

MCP servers are long-lived stateful processes.

**Decision: Solved by the unified server mode protocol.** `phyl-tool-mcp` runs
in `--serve` mode for the life of the session. It manages MCP server child
processes internally — starting them on first use and stopping them when stdin
closes. See the Tool Protocol section for the NDJSON server mode contract.

**One-shot CLI mode (`--call`):** `phyl-tool-mcp` also supports a one-shot
invocation mode for use outside of sessions — from scripts, cron, or
`phyl-poll`:

```sh
$ phyl-tool-mcp --call <server> <tool> '<arguments json>'
```

This starts the named MCP server, performs the initialize handshake, calls
the specified tool, prints the result to stdout, and shuts down. Example:

```sh
$ phyl-tool-mcp --call brave-search brave_search '{"query":"rust async"}'
{"output":"...search results..."}
```

This reuses all the existing MCP client logic — config parsing, server
startup, JSON-RPC protocol, content extraction. No duplication. The mode is
a thin wrapper: start one server, make one call, print, exit.

This makes MCP tools composable from the shell. Any MCP server configured in
`config.toml` becomes a CLI tool. It also composes naturally with `phyl-poll`:

```toml
[[poll]]
name = "email-check"
command = "phyl-tool-mcp"
args = ["--call", "email-server", "check_inbox", "{}"]
interval = 120
prompt = "New email arrived. Summarize the new messages."
```

### FIFO Handling

Named pipes are tricky. Concrete strategy:

1. Session runner creates the FIFO with `mkfifo`
2. Opens it with `O_RDWR | O_NONBLOCK` — this prevents blocking on open
   (opening read-only blocks until a writer exists, and vice versa)
3. Uses `poll()` or `epoll` to check for data alongside other work
4. Each line written to the FIFO is a complete JSON object
5. Writers must write atomically (single `write()` call, message < PIPE_BUF
   bytes = 4096 on Linux) to prevent interleaving
6. If a message exceeds PIPE_BUF, writers should use the API endpoint instead

### Context Window Management

Conversations will eventually exceed the model's context limit.

**Decision: Summarize and truncate.** When the message history exceeds a
configurable threshold (e.g., 80% of the model's context window):

1. Take the oldest N messages (excluding the system prompt)
2. Ask the model: "Summarize this conversation so far in 2-3 paragraphs"
3. Replace those N messages with a single user message containing the summary
4. Continue with the compressed history

The threshold and model context size are configured per-model in `config.toml`:

```toml
[model]
context_window = 200000        # tokens (approximate)
compress_at = 0.8              # compress when 80% full
```

The model adapter binary is specified in `[session].model`, not here. This
section configures context window behavior for whatever adapter is in use.

Token counting uses the model adapter's `usage` field if available (accurate),
or falls back to `chars / 4` as a rough heuristic. The session runner tracks
cumulative token usage across the conversation and triggers compression when
the threshold is reached.

### Daemon Crash Recovery

**Decision: PID file + session dir scanning.**

- Each `phyl-run` process writes its PID to `sessions/<uuid>/pid`
- On startup, the daemon scans `sessions/` for dirs with a `pid` file
- For each, check if the process is still running (`kill -0`)
- If running: re-adopt it (start tailing its log)
- If dead: mark session as `crashed`, leave dir for inspection
- The daemon itself writes its PID to `$XDG_RUNTIME_DIR/phylactd.pid`

### Session Resumption

**Decision: Not in v1.** If `phyl-run` crashes, the session is marked as
crashed. A new session can be started with the same prompt. The crashed
session's `log.jsonl` is available for inspection but not automatic recovery.

Future: replay `log.jsonl` to reconstruct history and resume. The log format
supports this — it contains the full conversation.

### Signal Disambiguation

When multiple sessions have pending questions, the Signal bridge needs a way
to route replies.

**Decision: Session tags + reply quoting.**

- Each Signal message includes a short session tag: `[3a7f]`
- If only one session is pending, any reply goes to it
- If multiple are pending, the user prefixes with the tag: `3a7f yes`
- If the user replies without a tag and multiple are pending, the bridge asks:
  "Which session? [3a7f] checking email, [91b2] updating docs"

### Error Handling

| Failure | Response |
|---------|----------|
| Model adapter returns invalid JSON | Log error, retry once, then fail session |
| Model adapter times out (>5 min) | Kill process, retry once, then fail |
| Model adapter exits non-zero | Log stderr, retry once, then fail |
| One-shot tool returns invalid JSON | Return error string to model, let it recover |
| One-shot tool times out (>2 min) | Kill process, return timeout error to model |
| One-shot tool exits non-zero | Return stderr to model as error |
| Server-mode tool pipe breaks | Return error to model for pending calls, disable tool for session |
| FIFO write fails | Fall back to API endpoint for event injection |
| Git commit fails after retries | Log error, continue session without committing |
| Finalization model call fails | Log warning, skip SOUL.md update, still exit cleanly |

Model failures fail the session after one retry. Tool failures are reported to
the model as errors — the model can decide to retry, try a different approach,
or ask the human.

### System Prompt Template

The system prompt is assembled from files, not improvised. Concrete format:

```
=== LAW ===
{contents of LAW.md}

=== JOB ===
{contents of JOB.md}

=== SOUL ===
{contents of SOUL.md}

=== KNOWLEDGE INDEX ===
{contents of knowledge/INDEX.md}

=== SESSION ===
Session ID: {uuid}
Working directory: {session_dir}/scratch/
You have access to the following tools: {tool_names}

Remember: LAW rules are absolute. Obey them unconditionally.
```

Each section has a clear delimiter. The model can distinguish LAW from JOB from
SOUL. The session section provides runtime context.

This template lives as a constant in `phyl-run`. It is not configurable —
the structure is part of the system's design, not a user preference.

### SOUL.md Growth Bounds

SOUL.md grows with every session. Unbounded growth eats the context window.

**Decision: Hard size cap, enforced by the finalization prompt.**

The finalization prompt tells the model: "SOUL.md must stay under 2000 words.
If you need to add something, revise and compress — don't just append. Old
reflections that no longer feel relevant can be removed. This file is your
living self-portrait, not a journal. Keep it current, not comprehensive."

The journal (for detailed per-session notes) goes in `knowledge/journal/`.
SOUL.md is identity — compact, evolving, and present-tense.

If SOUL.md exceeds 3000 words despite instructions, `phyl-run` truncates
from the middle (keeping the first and last sections) and logs a warning.
This is a safety net, not the primary mechanism — the prompt should keep
it in bounds.

### Parallel Tool Calls

Models often return multiple tool_calls in a single response (e.g.,
"read these 3 files simultaneously"). These should run in parallel when
possible.

**Decision: All tool calls from a single model response run in parallel.**

- One-shot tool calls: spawn all in parallel (`tokio::join!`), each is a
  separate process with no shared state
- Server-mode tool calls: send all requests, then collect all responses
  (the NDJSON `id` field handles multiplexing)

In practice: all tool calls from a single model response are dispatched
simultaneously. Results are collected and appended to history in the order
the model originally listed them.

This is safe because tools are isolated processes. One tool can't affect
another's execution. If a tool fails, its error is reported independently.

### Operational Logging

**Decision: stderr is the log. Follow Unix convention.**

- All binaries log to stderr.
- `phyl-run` redirects its own stderr to `sessions/<uuid>/stderr.log` at
  startup (via `dup2`). This makes it independent of the daemon — if the daemon
  crashes, `phyl-run` keeps logging. No pipes to break, no SIGPIPE deaths.
- `phyl-run` writes operational messages (tool dispatch, model invocations,
  timing) to stderr, not to `log.jsonl`. The JSONL log is the conversation;
  stderr is the operational trace.
- `phylactd` logs to stderr. Run under systemd and it goes to journald
  automatically. Run in a terminal and you see it live.
- Log level controlled by `RUST_LOG` env var (standard `env_logger` / `tracing`
  convention). Default: `info`.

No custom log format. No log rotation. Use `logrotate` or `journald` — tools
that already exist.

### Process Sandboxing

We're spawning processes from model output. LAW.md is the policy layer, but
mistakes happen. A hallucinated `rm -rf /` shouldn't actually work. Use
lightweight Linux kernel primitives to limit blast radius — no containers,
no VMs, just syscalls.

**Decision: Opt-in per tool via the `sandbox` field in `--spec`. The session
runner enforces whatever the spec declares.**

The sandbox is primarily for tools that execute model-generated input —
`phyl-tool-bash` above all. Trusted tools like `phyl-tool-session` and
`phyl-tool-mcp` (our own code) don't need sandboxing and don't declare it.

A tool opts into sandboxing by including a `sandbox` object in its `--spec`:

```json
{
  "name": "bash",
  "mode": "oneshot",
  "sandbox": {
    "paths_rw": ["$PHYLACTERY_SESSION_DIR/scratch/", "/tmp"],
    "paths_ro": ["/usr", "/lib", "/bin", "/etc"],
    "net": true,
    "max_cpu_seconds": 120,
    "max_file_bytes": 104857600,
    "max_procs": 64,
    "max_fds": 256
  },
  "parameters": { ... }
}
```

If `sandbox` is absent from the spec, the tool runs unsandboxed.

**What the session runner enforces (between `fork` and `exec`):**

1. **Landlock** (filesystem access control, unprivileged since Linux 5.13):
   - Applies the `paths_rw` and `paths_ro` lists from the spec
   - Everything not listed: no access
   - Environment variables are expanded before applying

2. **PID namespace** (`CLONE_NEWPID`, requires `user.max_user_namespaces > 0`):
   - Tool processes can't see or signal other processes
   - A `kill -9 1` inside the sandbox only kills the tool's init
   - Graceful degradation: if user namespaces are disabled (some hardened
     kernels), log a warning and skip. PID isolation is defense-in-depth,
     not essential.

3. **Resource limits** (`setrlimit`):
   - Applies `max_cpu_seconds`, `max_file_bytes`, `max_procs`, `max_fds`

**What we deliberately don't restrict:**

- **Network.** The agent needs to `curl`, `ssh`, call APIs. Network policy is
  LAW.md's job, not the sandbox's.
- **Environment variables.** Tools need `$PATH`, `$HOME`, session env vars.
- **System tools.** `/usr/bin` is read-only accessible. The model needs `git`,
  `grep`, `curl`, etc.

**Which built-in tools declare sandboxing:**

| Tool | Sandboxed | Why |
|------|-----------|-----|
| `phyl-tool-bash` | Yes (strict) | Executes model-generated shell commands |
| `phyl-tool-files` | Yes (needs knowledge/ rw, .git/ rw) | Writes model-chosen paths, runs git |
| `phyl-tool-session` | No | Only does stdin/stdout NDJSON, no file I/O |
| `phyl-tool-mcp` | No | Spawns trusted MCP servers, needs broad access |

`phyl-tool-files` declares its own sandbox to limit writes to the knowledge
base and scratch directory:

```json
{
  "sandbox": {
    "paths_rw": [
      "$PHYLACTERY_SESSION_DIR/scratch/",
      "$PHYLACTERY_HOME/knowledge/",
      "$PHYLACTERY_HOME/.git/",
      "$PHYLACTERY_HOME/.git.lock"
    ],
    "paths_ro": ["$PHYLACTERY_HOME/", "/usr", "/lib", "/bin", "/etc"],
    "net": false,
    "max_cpu_seconds": 30
  }
}
```

**Why Landlock specifically:**

- Unprivileged — no root, no suid, no capabilities needed
- Kernel-enforced — can't be bypassed from userspace
- Composable — each process gets its own policy
- Available since Linux 5.13 (2021), backported to most distros
- Rust has good support via the `landlock` crate
- Graceful degradation — if the kernel is too old, log a warning and run
  without filesystem restriction. Don't fail.

**Why not bwrap/firejail/docker:**

- External dependencies we'd have to install and manage
- Heavier than necessary — we don't need mount namespaces or full container
  isolation
- Landlock + PID namespace + resource limits cover the realistic threat model:
  accidental damage from model mistakes. We're not defending against a
  determined attacker — this is a personal agent on your own machine.

### Knowledge Base Summary

The model needs to know what's in the knowledge base without reading every file.

**Decision: `knowledge/INDEX.md` maintained by the agent.**

LAW.md should instruct the agent to maintain a `knowledge/INDEX.md` file — a
table of contents listing what's in the knowledge base and when it was last
updated. This file is included in the system prompt (after SOUL.md).

The agent updates INDEX.md as part of its normal knowledge base maintenance.
If it gets stale, the agent can regenerate it from `find knowledge/ -name '*.md'`.

This is simpler than LLM-generated summaries and more reliable than automated
file listings. The agent curates its own index.

INDEX.md is included in every system prompt, so it should stay compact — under
500 words. Use hierarchical listing (`## contacts/ — 12 files`), not per-file
descriptions. LAW.md should instruct the agent to keep it brief.

---

## What This Is Not

- **Not a chat UI.** No web frontend. Use the terminal or Signal.
- **Not multi-user.** One human, one agent, one machine.
- **Not cloud-native.** It's local processes on a Linux box.
- **Not a framework.** You don't import it. You run it.
- **Not a security boundary.** Landlock sandboxing limits accidental damage
  from model mistakes, but this is a personal agent on your own machine, not a
  multi-tenant system. LAW.md is policy. The sandbox is a seatbelt.
- **Not a daemon that must be running.** You can run `phyl-run` directly to
  test a session without the daemon.

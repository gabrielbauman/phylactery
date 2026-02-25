# Phylactery — Implementation Progress

Tracking implementation status against the [plan](PLAN.md).

## Phase 1: Core Types + Skeleton — **Complete**

- [x] `cargo init` workspace with `phyl-core`
- [x] Define shared types in `phyl-core`: `Message`, `ToolCall`, `ToolSpec`
      (with `mode` field), `ModelRequest`, `ModelResponse`, `ToolInput`,
      `ToolOutput`, `LogEntry`, `ServerResponse` (with `signal` field)
- [x] All types derive `Serialize`/`Deserialize`
- [x] Stub the other crates with `fn main() { todo!() }`
- [x] Implement `phyl init`: create `$PHYLACTERY_HOME` with git repo, seed
      `config.toml`, `LAW.md`, `JOB.md`, `SOUL.md` ("I am new."),
      `knowledge/` structure, `sessions/.gitignore`
- [x] Verify: `cargo build` succeeds, produces multiple binaries

## Phase 2: Tool Protocol — **Complete**

- [x] Implement `phyl-tool-bash`: `--spec` (with `"mode":"oneshot"`) and
      invocation mode. chdir to `$PHYLACTERY_SESSION_DIR/scratch/`, enforce
      timeout.
- [x] Implement `phyl-tool-files`: read_file, write_file, search_files
- [x] Test from command line:
      `echo '{"name":"bash","arguments":{"command":"echo hi"}}' | phyl-tool-bash`

## Phase 3: Model Adapter — **Complete**

- [x] Implement `phyl-model-claude`:
      - Read `ModelRequest` from stdin
      - Build system prompt from system messages + tool definitions (with
        `<tool_call>` XML format instructions)
      - Build user prompt from conversation history (multi-turn support with
        `<conversation_history>` formatting)
      - Invoke `claude --print --output-format json --tools "" --no-session-persistence`
      - Parse claude CLI JSON response (`result`, `is_error` fields)
      - Extract `<tool_call>` blocks from response text into structured `ToolCall` objects
      - Write `ModelResponse` to stdout
- [x] Environment variable support: `PHYL_CLAUDE_CLI` (binary path),
      `PHYL_CLAUDE_MODEL` (model override)
- [x] Unit tests (20 tests): tool call extraction, system prompt building,
      user prompt formatting, response parsing
- [x] Test from command line:
      `echo '{"messages":[{"role":"user","content":"say hi"}],"tools":[]}' | phyl-model-claude`

## Phase 4: Session Runner — **Complete**

- [x] Implement `phyl-run`:
      - Parse args (`--session-dir`, `--prompt`)
      - Redirect stderr to `sessions/<uuid>/stderr.log` (via `dup2`)
      - Write PID file to `sessions/<uuid>/pid`
      - Read `config.toml` (with defaults if missing)
      - Read LAW.md, JOB.md, SOUL.md, knowledge/INDEX.md
      - Assemble system prompt from template (=== LAW/JOB/SOUL/SESSION sections)
      - Discover tools from `$PATH` (any `phyl-tool-*` executable, `--spec`)
      - Parse tool specs (single or array), detect oneshot vs server mode
      - Start server-mode tools (`--serve`, keep stdin/stdout handles for NDJSON)
      - Build tool dispatch map (name → executable + mode)
      - Set environment variables (`PHYLACTERY_SESSION_ID`, `_SESSION_DIR`,
        `_HOME`, `_KNOWLEDGE_DIR`)
      - Run the agentic loop:
        - Invoke model adapter (configurable binary, default `phyl-model-claude`)
        - Model retry with configurable max retries on failure
        - Append assistant messages to history, write to log.jsonl
        - Dispatch tool calls: oneshot tools in parallel (threads), server-mode
          via NDJSON
        - Collect results, detect `end_session` signal from server-mode tools
        - Implicit done: if model responds without tool calls and no FIFO
          events after brief wait, finalize
        - Context window management: track cumulative tokens (from `usage`
          field or chars/4 heuristic), compress history at configurable
          threshold by summarizing oldest messages via model adapter
      - Finalization step:
        - Close stdin on server-mode tools → they exit
        - `flock` on `.soul.lock` (exclusive, serializes SOUL.md updates)
        - Re-read SOUL.md from disk (not stale session-start version)
        - Invoke model for reflection (with session summary + current SOUL.md)
        - Write updated SOUL.md
        - `flock` on `.git.lock`, `git add SOUL.md && git commit`
        - Release locks (soul first, then git — correct lock ordering)
        - Truncate SOUL.md if >3000 words (keep first + last thirds)
      - Write final `done` entry to log.jsonl
      - Cleanup: remove FIFO, remove PID file
- [x] Create FIFO (`mkfifo`, open with `O_RDWR | O_NONBLOCK`), poll for
      events with `poll()`, parse JSON or plain text events
- [x] Implement `phyl-tool-session` (server mode, NDJSON):
      - `--spec`: returns array with `ask_human` and `done` tool specs
        (both `mode: "server"`)
      - `--serve`: NDJSON server loop on stdin/stdout
      - `done` handler: returns summary with `"signal":"end_session"`
      - `ask_human` handler: blocks waiting for forwarded answer from runner,
        handles timeout and session-end cancellation
- [x] Unit tests (7 for phyl-run, 2 for phyl-tool-session): system prompt
      building, session summarization, SOUL.md truncation, tool spec
      serialization
- [x] Test: `phyl-run --session-dir sessions/test --prompt "what is 2+2"`
      (requires `phyl init` and model adapter on PATH)

## Phase 5: Daemon — **Complete**

- [x] Implement `phylactd`:
      - Parse `config.toml` from `$PHYLACTERY_HOME` (with defaults)
      - Listen on Unix socket (axum + tokio `UnixListener`)
      - Socket path from `config.daemon.socket` (default
        `$XDG_RUNTIME_DIR/phylactery.sock` or `/tmp/phylactery.sock`)
      - Set socket permissions to 0700 (owner-only access)
      - Write daemon PID file to `$XDG_RUNTIME_DIR/phylactd.pid`
      - Verify `$PHYLACTERY_HOME` exists (fail with message to run `phyl init`)
      - Spawn `phyl-run` as child process for each session (auto-discovers
        binary via `$PATH` or same directory as `phylactd`)
      - In-memory session tracking: id, status, pid, child handle, prompt,
        summary, created_at
      - Enforce max concurrent sessions (`config.session.max_concurrent`)
      - Background reaper task: poll every 5s, detect finished/crashed processes
        via `try_wait()` or `kill(0)`, update status, extract summary from
        `log.jsonl` done entries
      - Crash recovery on startup: scan `sessions/` for directories with `pid`
        files, re-adopt running processes (`kill -0` check), mark dead ones as
        crashed, extract timestamps and prompts from `log.jsonl`
      - Clean socket removal on startup (stale) and shutdown
- [x] API endpoints:
      - `GET /health` — returns status, active/total session counts
      - `POST /sessions` — start session with `{"prompt":"..."}`, returns
        `{id, status}` (201 Created)
      - `GET /sessions` — list all sessions as `SessionInfo` array (sorted
        newest first)
      - `GET /sessions/:id` — session detail with prompt + last 50 log entries
      - `DELETE /sessions/:id` — kill running session (SIGTERM → SIGKILL),
        returns 204
      - `POST /sessions/:id/events` — inject event into session FIFO
        (supports user messages, answer events with `question_id`), returns 202
      - `GET /feed` — SSE stream of attention events (question, done, error)
        across all sessions, tails `log.jsonl` files with byte offset tracking
- [x] Unit tests (13 tests): request/response serialization, FIFO event
      building, log reading (empty, with entries, summary, prompt extraction),
      config defaults, router creation

## Phase 6: CLI Client

- [ ] Implement remaining `phyl` subcommands (session, ls, status, say, log, stop, watch, start)
- [ ] Test: full cycle with daemon + CLI

## Phase 7: MCP Bridge

- [ ] Implement `phyl-tool-mcp`

## Phase 8: Knowledge Base + Git

- [ ] Auto-commit in `phyl-tool-files` for writes under `knowledge/`
- [ ] `search_files` tool
- [ ] Knowledge base summary generation at session startup

## Phase 9: Human Attention System

- [x] Implement `phyl-tool-session` (server mode) — done in Phase 4
- [x] `GET /feed` SSE endpoint in daemon — done in Phase 5
- [ ] `phyl watch` CLI command

## Phase 10: Signal Bridge

- [ ] Implement `phyl-bridge-signal`

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

## Phase 6: CLI Client — **Complete**

- [x] Implement `phyl` subcommands:
      - `phyl start [-d]` — launch `phylactd` (foreground via exec, or `-d`
        for background with PID output). Auto-discovers `phylactd` binary
        via `$PATH` or same directory as `phyl`.
      - `phyl session [-d] "prompt"` — POST /sessions to create a session.
        Foreground mode tails `log.jsonl` with formatted output until session
        ends. Detached mode (`-d`) prints session UUID and returns.
      - `phyl ls` — GET /sessions, displays table with ID, status, created
        timestamp, and truncated summary.
      - `phyl status <id>` — GET /sessions/:id, displays session detail with
        prompt, status, timestamps, and recent log entries (formatted).
      - `phyl say <id> "msg"` — POST /sessions/:id/events, injects a user
        message into a running session's FIFO.
      - `phyl log <id>` — Tails session's `log.jsonl`. For finished sessions,
        dumps the full log. For running sessions, follows with polling until
        the session completes.
      - `phyl stop <id>` — DELETE /sessions/:id, kills a running session.
      - `phyl watch` — GET /feed (SSE), streams attention-worthy events
        (questions, done, errors) across all sessions. Handles inline
        question answering: prompts for input when a question event arrives,
        sends the answer via POST /sessions/:id/events.
- [x] Unix socket HTTP client module (`client.rs`): uses hyper 1.x low-level
      `http1::handshake` over tokio `UnixStream`. Supports GET, POST, DELETE,
      and streaming GET for SSE. Socket path resolved from `config.toml`.
- [x] Log entry display formatting (`format.rs`): type-aware formatting for
      all `LogEntryType` variants — user, assistant (with tool calls), tool
      results (truncated to 200 chars), questions (with options), answers,
      done (summary), errors, system messages.
- [x] Unit tests (15 tests): client error display/construction (5 tests),
      log entry formatting for all entry types (10 tests)

## Phase 7: MCP Bridge — **Complete**

- [x] Implement `phyl-tool-mcp`:
      - Read MCP server configs from `$PHYLACTERY_HOME/config.toml`
        (`[[mcp]]` sections with `name`, `command`, `args`, `env`)
      - MCP JSON-RPC 2.0 client: `initialize` handshake with
        `protocolVersion: "2024-11-05"`, `notifications/initialized`,
        `tools/list`, `tools/call`
      - On `--spec`: start each configured MCP server, perform init
        handshake, query `tools/list`, aggregate into `Vec<ToolSpec>` with
        server-name-prefixed tool names (e.g., `filesystem_read_file`),
        all in `mode: "server"`, shut down MCP servers, print JSON
      - On `--serve`: start all configured MCP servers, build routing
        table (prefixed name → server index + original name), run NDJSON
        server loop reading `ServerRequest` from stdin, route to correct
        MCP server via `tools/call`, convert MCP response content items
        to `ServerResponse` on stdout, shut down servers on stdin EOF
      - Environment variable expansion in MCP server env config
        (`$VAR` references resolved from process environment)
      - Graceful shutdown: drop stdin to signal EOF, try_wait then kill
      - Handles notifications and unexpected messages from MCP servers
        (skips with log to stderr)
- [x] Unit tests (17 tests): JSON-RPC request/response serialization,
      MCP tool definition parsing, content extraction from MCP responses,
      tool name prefixing, routing table lookup, ToolSpec output format,
      config loading with missing home, env var expansion, initialize
      params structure

## Phase 8: Knowledge Base + Git — **Complete**

- [x] Implement auto-commit in `phyl-tool-files` for writes under `knowledge/`:
      - Detects when `write_file` target is under `$PHYLACTERY_HOME/knowledge/`
      - Reads `config.toml` to check `git.auto_commit` setting
      - Acquires exclusive `flock` on `$PHYLACTERY_HOME/.git.lock`
        (serializes with SOUL.md commits and other sessions)
      - Runs `git add <file>` + `git commit -m "knowledge: update <path>"`
      - Reports commit status in tool output message
      - Gracefully handles "nothing to commit" (no-op)
- [x] `search_files` tool (implemented in Phase 2, enhanced):
      - Recursive substring search with file path + line number output
      - Skips hidden files, `node_modules/`, `target/` directories
      - Caps at 200 matches, binary files silently skipped
      - Updated description to clarify knowledge base searchability
      - Path parameter supports `$PHYLACTERY_HOME/knowledge/` via env
        var expansion (`$VAR` and `${VAR}` syntax)
- [x] Environment variable expansion in `phyl-tool-files` path resolution:
      - `resolve_path()` expands `$VAR` and `${VAR}` references from
        process environment before resolving relative paths
      - Allows model to reference `$PHYLACTERY_HOME/knowledge/...` in
        tool arguments for both reads and searches
- [x] Knowledge base file tree generation at session startup:
      - `phyl-run` recursively enumerates files under `knowledge/`
      - Generates compact file tree listing (paths only, no content)
      - Included in system prompt as `=== KNOWLEDGE SUMMARY ===` section
      - INDEX.md excluded from listing (already shown separately)
      - Skips hidden files/directories
      - Agent uses `read_file` and `search_files` to access content
        on demand, keeping context window usage minimal
- [x] Unit tests: 18 tests for `phyl-tool-files` (env var expansion,
      path resolution, read/write/search operations, tool specs,
      sandbox config, config loading), 5 new tests for `phyl-run`
      (knowledge summary generation, file collection, hidden file
      skipping, system prompt integration)

## Phase 9: Human Attention System — **Complete**

- [x] Implement `phyl-tool-session` (server mode) — done in Phase 4
- [x] `GET /feed` SSE endpoint in daemon — done in Phase 5
- [x] `phyl watch` CLI command — done in Phase 6

## Phase 10: Signal Bridge — **Complete**

- [x] Implement `phyl-bridge-signal`:
      - Read `[bridge.signal]` config from `$PHYLACTERY_HOME/config.toml`
        (`phone`, `owner`, `signal_cli` fields)
      - Verify `signal-cli` availability on startup (version check)
      - Connect to daemon `GET /feed` SSE endpoint via Unix socket
        (hyper HTTP/1.1 client, same pattern as `phyl` CLI client)
      - Parse SSE frames, extract `LogEntry` from event data JSON
      - Forward attention events as Signal messages to configured owner:
        - Question events: include question text and numbered options,
          track pending questions in FIFO queue for reply matching
        - Done events: include session summary
        - Error events: include error message
      - Poll for inbound Signal messages via
        `signal-cli -a <phone> --output=json receive --timeout 2`
      - Parse `signal-cli` JSON envelope format
        (`envelope.sourceNumber`, `envelope.dataMessage.message`)
      - Security: only accept messages from configured `owner` number,
        ignore all others
      - Route inbound replies to pending questions via
        `POST /sessions/:id/events` with `question_id`
      - Numeric reply matching: "1", "2", etc. resolve to question
        option text
      - Accept new session requests from inbound messages when no
        questions are pending (`POST /sessions` with message as prompt),
        confirm session start via Signal reply
      - Automatic reconnection to daemon feed on connection loss (5s delay)
      - Pending question queue capped at 50 to prevent unbounded growth
      - Graceful shutdown on Ctrl-C via `tokio::signal::ctrl_c()`
- [x] Config types added to `phyl-core/src/lib.rs`:
      `BridgeConfig`, `SignalBridgeConfig` with `phone`, `owner`,
      `signal_cli` (defaults to `"signal-cli"`)
- [x] Config field added to `Config` struct:
      `bridge: Option<BridgeConfig>` (optional, backward-compatible)
- [x] `phyl init` config template updated with commented-out
      `[bridge.signal]` section
- [x] Unit tests (14 tests): answer resolution (numeric options, out of
      range, text, empty options), signal-cli JSON envelope parsing
      (normal, no message, empty message, null envelope), pending
      question state management (tracking, FIFO order, cap at 50),
      config deserialization (full, defaults, missing bridge)

## Phase 11: Polling — **Complete**

- [x] Implement `phyl-poll`:
      - Standalone long-lived binary, runs alongside daemon
      - Read `[[poll]]` configs from `$PHYLACTERY_HOME/config.toml`
      - Load `secrets.env` on startup (env var expansion for config values)
      - For each poll rule, spawn tokio task with configured interval
      - Run configured command (direct or `shell = true` via `sh -c`)
      - Capture stdout, compare to previous output stored in
        `$PHYLACTERY_HOME/poll/<name>.last`
      - On first run: establish baseline (store output, no session)
      - On change: assemble prompt with previous/current output + diff,
        POST /sessions to daemon via Unix socket
      - On no change: sleep until next interval
      - Staggered initial execution (100ms per rule) to avoid thundering herd
      - Command timeout enforcement (configurable, default 30s)
      - Graceful shutdown on Ctrl-C (`tokio::signal::ctrl_c()`)
- [x] Config types in `phyl-core`: `PollConfig` with `name`, `command`,
      `args`, `interval` (default 300s), `prompt`, `env` (HashMap),
      `shell` (bool), `timeout` (default 30s)
- [x] State directory: `$PHYLACTERY_HOME/poll/` (gitignored, created
      on first run)
- [x] Prompt assembly: rule's `prompt` + previous output + current
      output + simple line-by-line diff
- [x] Daemon client: hyper HTTP/1.1 over Unix socket (same pattern as
      CLI client)
- [x] Unit tests (14 tests): diff generation (identical, changes,
      additions, deletions), prompt assembly, env var expansion
      ($VAR and ${VAR} syntax, missing vars, mixed), config
      deserialization (full, defaults, shell mode, with env/timeout),
      secrets.env loading
- [x] Added to workspace `Cargo.toml`

## Phase 12: Incoming Event Listener — **Complete**

### Phase 12a: Webhooks

- [x] Implement webhook listener in `phyl-listen`:
      - HTTP server using axum + tokio on configurable bind address
        (default `127.0.0.1:7890`)
      - Route incoming POSTs to configured hooks by path matching
      - HMAC-SHA256 signature verification (`X-Hub-Signature-256`
        for GitHub, `X-Gitlab-Token` for GitLab)
      - Event-type routing: resolve prompt from `route_header` +
        `routes` map, fall back to `prompt` field
      - Header-based filtering: `filter_header` + `filter_values`
        for matching (first match wins among hooks on same path)
      - Rate limiting: sliding window, default 10/minute per hook
      - Deduplication: 5-minute in-memory cache by delivery ID header
        (default `X-Request-Id`)
      - Payload size limit: default 1 MB per hook
      - Returns 202/404/401/429 as appropriate
      - Prompt assembly: hook prompt + source info + headers + payload
- [x] Config types in `phyl-core`: `ListenHookConfig` with `name`,
      `path`, `prompt`, `secret`, `filter_header`, `filter_values`,
      `rate_limit`, `dedup_header`, `max_body_size`, `route_header`,
      `routes` (HashMap)
- [x] Unit tests: HMAC verification (GitHub valid/invalid, GitLab
      valid/invalid, no header), prompt resolution (with route,
      fallback, no route header), config deserialization

### Phase 12b: SSE Subscription

- [x] Implement SSE subscription listener in `phyl-listen`:
      - For each `[[listen.sse]]` config, spawn persistent connection
      - SSE frame parsing: `event:`, `data:` (multi-line), `id:`,
        comments (`:` prefix for keep-alive)
      - Event-type filtering (`events` list) and routing
        (`route_event` + `routes` map)
      - Automatic reconnection with exponential backoff (5s → 60s max)
      - `Last-Event-ID` header on reconnect
      - Stale connection detection (5-minute no-activity timeout)
      - Rate limiting and deduplication per source
      - Custom headers with env var expansion
      - Prompt assembly: resolved prompt + source info + event data
- [x] Config types in `phyl-core`: `ListenSseConfig` with `name`,
      `url`, `prompt`, `headers` (HashMap with env expansion),
      `events`, `route_event`, `routes`, `rate_limit`
- [x] Unit tests: config deserialization (full, defaults)

### Phase 12c: File Watching

- [x] Implement file watch listener in `phyl-listen`:
      - inotify-based file system monitoring
      - Watch configured paths for create/modify/delete/move events
      - Recursive directory watching (optional, with auto-add for
        new subdirectories)
      - Event type filtering against configured `events` list
      - Glob-based filename filtering
      - Per-file debouncing (configurable window, default 2s)
      - Include file content for small files (<100KB) on create/modify
      - Skip hidden files/directories unless glob explicitly matches
      - Rate limiting per watch source
      - Prompt assembly: watch prompt + file event info + file content
- [x] Config types in `phyl-core`: `ListenWatchConfig` with `name`,
      `path`, `prompt`, `recursive`, `events`, `glob`, `debounce`,
      `rate_limit`
- [x] Unit tests: glob matching, watch mask building, event type
      mapping, prompt assembly, config deserialization (full, defaults)

### Shared Infrastructure

- [x] `ListenConfig` in `phyl-core` with `bind`, `hook` (Vec),
      `sse` (Vec), `watch` (Vec)
- [x] Shared rate limiting module: in-memory sliding window per source,
      configurable max per minute
- [x] Shared deduplication cache: in-memory with 5-minute TTL
- [x] Shared daemon client: Unix socket HTTP POST /sessions
- [x] Graceful shutdown on Ctrl-C
- [x] Unit tests: rate limiter (under/over limit, independent sources),
      dedup cache (duplicate/different IDs)
- [x] Added to workspace `Cargo.toml`

## Phase 13: Setup, Configuration, and Service Management — **Complete**

### Secrets File Infrastructure

- [x] `phyl init` creates `secrets.env` (empty, `chmod 600`)
- [x] Added `secrets.env` to `.gitignore` template (root `.gitignore`)
- [x] `phyl-poll` and `phyl-listen` load `secrets.env` on startup

### Configuration Subcommands

- [x] `phyl config show`: read config.toml, pretty-print with
      secret values masked (first 3 chars + bullets)
- [x] `phyl config validate`: parse config.toml, check model adapter
      path, poll command paths, duplicate names, secret references
      ($VAR) against secrets.env and environment
- [x] `phyl config edit`: exec `$EDITOR` on config.toml, run
      validation after editor exits
- [x] `phyl config add <type> <name>`: append config section template
      for mcp/poll/hook/sse/watch/bridge signal
- [x] `phyl config add-secret <KEY> <VALUE>`: append to secrets.env,
      check for duplicates
- [x] `phyl config list-secrets`: display keys with masked values
- [x] `phyl config remove-secret <KEY>`: remove from secrets.env

### Setup Subcommands

- [x] `phyl setup systemd`: generate systemd user unit files from
      current config (only for configured services), install to
      `~/.config/systemd/user/`, run `daemon-reload` + `enable` +
      `start`. Idempotent — regenerates on re-run. Units include
      `EnvironmentFile` for secrets, `Restart=on-failure`,
      dependency ordering (poller/listener/bridge after daemon).
- [x] `phyl setup status`: show operational health — home directory
      (XDG or legacy), config validity, secrets count, service
      status (daemon reachability via GET /health, systemd unit
      status for others), session summary from daemon API
- [x] `phyl setup migrate-xdg`: move `~/.phylactery` to
      `~/.local/share/phylactery`, create `~/.config/phylactery/`
      symlink, safety check (requires `--force`)

### Enhanced Init

- [x] Default path remains `~/.phylactery` via `$PHYLACTERY_HOME`
      (XDG default when `$PHYLACTERY_HOME` not set in `phyl-core`)
- [x] Creates `secrets.env` (empty, chmod 600, gitignored)
- [x] Creates `~/.config/phylactery/` symlink to config.toml
      (when using XDG paths)
- [x] Creates `poll/` directory with `.gitignore`
- [x] `--systemd` flag for combined init + service install
- [x] Prints next-steps guidance after init
- [x] Updated config template with commented-out `[[poll]]` and
      `[listen]` examples

### Start --all (Non-systemd Fallback)

- [x] `phyl start --all`: starts daemon first, waits for socket,
      then starts poller/listener/bridge based on config.
      Ctrl-C sends SIGTERM to all children.
      Only starts services that have configuration.

### Unit Tests

- [x] Config validation rules (secret refs, comments), secrets file
      parsing (empty, with entries), systemd unit generation (daemon,
      with dependency), secret counting (8 tests total)

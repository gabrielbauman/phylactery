# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
cargo build                          # Build all crates (debug)
cargo build --release                # Release build (binaries in target/release/)
cargo build -p phyl-core             # Build just the core types library
cargo build -p phyl                  # Build just the CLI binary
cargo test                           # Run all tests
cargo test -p phyl-run               # Run tests for a single crate
cargo test -p phyl-run -- test_name  # Run a single test by name
cargo clippy --workspace --all-targets  # Lint (CI runs this with -Dwarnings)
cargo fmt --all -- --check           # Check formatting
```

## CI

GitHub Actions (`.github/workflows/ci.yml`) runs on every push/PR to `main`:

- **check** and **test** on both `ubuntu-latest` and `macos-latest`
- **clippy** and **fmt** on `ubuntu-latest`
- `RUSTFLAGS=-Dwarnings` — all warnings are errors. Fix clippy lints before pushing.

Requires Rust stable 1.83+ (edition 2024).

## What This Project Is

A personal AI agent built as cooperating Unix processes. See `docs/` for detailed documentation of each component.

**Two-repo model:** This repo builds the binaries. The agent's runtime state lives in a separate git repo at `$PHYLACTERY_HOME` (default `~/.local/share/phylactery` on Linux, `~/Library/Application Support/phylactery` on macOS; legacy `~/.phylactery` also supported), created by `phyl init`.

## Architecture

17 crates in a Cargo workspace under `crates/`. One shared library, sixteen binaries:

- **phyl-core** — Shared types library. All protocol types live here. Every other crate depends on it.
- **phyl** — CLI client. Session subcommands (`session`, `ls`, `status`, `say`, `log`, `stop`, `watch`) talk to the daemon over a Unix socket. Setup subcommands (`init`, `setup systemd`, `setup status`, `setup migrate-xdg`, `config show/validate/edit/add`) manage configuration, secrets, and systemd user units directly. `start --all` runs all services in foreground without systemd.
- **phylactd** — Daemon. Manages sessions and serves a REST API on a Unix socket (`axum` + `tokio`). Spawns `phyl-run` per session, tracks processes, reaps finished sessions. API: `GET /health`, `POST /sessions`, `GET /sessions`, `GET /sessions/:id`, `DELETE /sessions/:id`, `POST /sessions/:id/events`, `GET /feed` (SSE).
- **phyl-run** — Session runner. The agentic loop: discover tools, invoke model adapter, dispatch tool calls (oneshot in parallel, server-mode via NDJSON), manage FIFO events, write `log.jsonl`, finalize SOUL.md with reflection. Invoked as `phyl-run --session-dir <path> --prompt <text>`.
- **phyl-model-claude** — Model adapter. Translates between phylactery's JSON format and the `claude` CLI. Reads `ModelRequest` from stdin, writes `ModelResponse` to stdout.
- **phyl-model-openai** — Model adapter for OpenAI-compatible APIs. Works with Ollama, llama.cpp, vLLM, LM Studio, or any server implementing `/v1/chat/completions`. Supports native tool calling and XML-based fallback. Configured via environment variables (`PHYL_OPENAI_URL`, `PHYL_OPENAI_MODEL`, etc.).
- **phyl-model-anthropic** — Model adapter for the native Anthropic Messages API. Uses structured `tool_use` content blocks for reliable tool calling. Requires `PHYL_ANTHROPIC_API_KEY`. Configured via environment variables (`PHYL_ANTHROPIC_URL`, `PHYL_ANTHROPIC_MODEL`, etc.).
- **phyl-tool-bash** — One-shot bash tool. Executes shell commands in `$PHYLACTERY_SESSION_DIR/scratch/` with timeout enforcement. Supports `--spec` for discovery.
- **phyl-tool-files** — One-shot file tool. Provides `read_file`, `write_file`, and `search_files` operations. Returns an array of `ToolSpec` from `--spec`. Supports `--spec` for discovery.
- **phyl-tool-session** — Server-mode tool. Provides `ask_human` (blocks for human response) and `done` (signals `end_session`). NDJSON on stdin/stdout. Supports `--spec` and `--serve`.
- **phyl-tool-mcp** — MCP bridge tool. Server-mode, NDJSON on stdin/stdout. Bridges to external MCP servers configured in `config.toml` (`[[mcp]]` sections). Implements MCP JSON-RPC 2.0 client protocol (initialize, tools/list, tools/call). Prefixes tool names with server name (e.g., `filesystem_read_file`). Supports `--spec`, `--serve`, and `--call <server> <tool> <args>` (one-shot CLI mode for use outside sessions, e.g. from `phyl-poll`).
- **phyl-tool-web** — One-shot web tools. Provides `http_fetch` (raw GET), `http_post`, `http_put`, `web_read` (fetch and convert to clean markdown — preferred for reading page content), `web_fetch` (headless browser for JS-rendered pages), and `web_search` (DuckDuckGo). Supports `--spec` for discovery.
- **phyl-tool-self** — Server-mode self-direction tool. Provides `spawn_session` (create session immediately or scheduled), `sleep_until` (end session and schedule wake-up), `list_scheduled` (view pending entries), and `cancel_scheduled` (cancel by ID). Schedule entries are JSON files in `$PHYLACTERY_HOME/schedule/`. Supports `--spec` and `--serve`.
- **phyl-sched** — Scheduler service. Scans `$PHYLACTERY_HOME/schedule/` every 5 seconds and fires due entries by creating sessions via the daemon API. Renames corrupt files to `.bad`, retries failed entries. Started unconditionally by `phyl start --all`.
- **phyl-bridge-signal** — Signal Messenger bridge. Connects to daemon's `GET /feed` SSE stream, forwards questions/done/errors as Signal messages to the owner. Listens for inbound Signal messages from the owner and routes replies to pending questions or creates new sessions. Uses `signal-cli` for Signal protocol. Configured via `[bridge.signal]` in `config.toml`.
- **phyl-poll** — Poller. Runs commands on configurable intervals, compares output to previous results, and starts sessions via the daemon API when changes are detected. Configured via `[[poll]]` sections in `config.toml`. State files stored in `$PHYLACTERY_HOME/poll/`. Turns any CLI tool into an event source for the agent.
- **phyl-listen** — Incoming event listener. Three listener types: webhooks (`[[listen.hook]]` — HTTP POST on a TCP port, HMAC-SHA256 verification, event-type routing), SSE subscriptions (`[[listen.sse]]` — persistent connections to event streams, reconnection with `Last-Event-ID`), and file watches (`[[listen.watch]]` — inotify-based, glob filtering, debouncing). All create sessions via the daemon API. Supports rate limiting and deduplication.

## Key Protocols

All inter-process communication is JSON on stdin/stdout. The types in `phyl-core/src/lib.rs` define the contracts:

- **Model adapter**: reads `ModelRequest`, writes `ModelResponse`
- **One-shot tools**: read `ToolInput`, write `ToolOutput`
- **Server-mode tools**: NDJSON `ServerRequest`/`ServerResponse` with `id` field for multiplexing and optional `signal` field for out-of-band messages (e.g. `"end_session"`)
- **Tool discovery**: `phyl-tool-X --spec` prints `ToolSpec` (or array of them) with a `mode` field (`"oneshot"` or `"server"`)
- **Bridge protocol**: Bridges are standalone processes that connect to the daemon's `GET /feed` SSE endpoint and post back via `POST /sessions/:id/events`. They are not session-specific and not discovered by `phyl-run`.

## Git Workflow

When implementing features or fixes, follow this workflow:

1. **Branch first.** Create a branch from `main` at the start of the work, prefixed with the current user's name (e.g. `gbauman/add-widget`). Include issue IDs in the branch name if applicable (e.g. `gbauman/42-add-widget`). Determine the username from `git config user.name` or the system username.
2. **Commit regularly.** Make small, logical commits as the implementation develops — don't save everything for one big commit at the end.
3. **Verify before each commit.** Before every commit, run `cargo fmt --all`, `cargo clippy --workspace --all-targets`, and `cargo test`. Fix any issues before committing.
4. **Ask before creating a PR.** When the implementation is complete, ask the user before creating the pull request.
5. **Watch CI after PR creation.** After creating the PR, watch CI checks (`gh pr checks <number> --watch`). If any check fails, diagnose and fix the issue, push the fix, and watch again until all checks pass.
6. **Ask before merging.** Once all CI checks pass, ask the user for permission to merge. Only merge after they agree.
7. **Clean up.** After merging, switch back to `main`, pull, and delete the local branch if needed.

## Conventions

- Rust edition 2024. Common dependencies (`serde`, `serde_json`, `chrono`, `uuid`, `toml`, `libc`, `tokio`, `axum`, `hyper`, `hyper-util`, `http-body-util`, `bytes`, `tower`, `anyhow`) are declared as workspace dependencies in the root `Cargo.toml`.
- All serializable types derive `Serialize`/`Deserialize`. Enums use `#[serde(rename_all = "snake_case")]`. Optional fields use `skip_serializing_if`.
- The `phyl` CLI uses `anyhow::Result<()>` for error handling. Other binaries still use `Result<(), String>` in places.
- Binaries log to stderr. Session conversation logs go to `log.jsonl` files.
- The `phyl init` git repo disables commit signing and sets a local `phylactery` identity to avoid depending on the user's global git config.

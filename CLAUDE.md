# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
cargo build                          # Build all crates
cargo build -p phyl-core             # Build just the core types library
cargo build -p phyl                  # Build just the CLI binary
cargo test                           # Run all tests (when tests exist)
cargo test -p phyl-core              # Run tests for a single crate
```

No linter or formatter is configured yet. Use `cargo clippy` and `cargo fmt` if needed.

## What This Project Is

A personal AI agent built as cooperating Unix processes. Read `PLAN.md` for the full architecture. Read `PROGRESS.md` for what's implemented.

**Two-repo model:** This repo builds the binaries. The agent's runtime state lives in a separate git repo at `$PHYLACTERY_HOME` (default `~/.phylactery`), created by `phyl init`.

## Architecture

10 crates in a Cargo workspace under `crates/`. One shared library, nine binaries:

- **phyl-core** — Shared types library. All protocol types live here. Every other crate depends on it.
- **phyl** — CLI client. Subcommands: `init`, `start [-d]`, `session [-d] "prompt"`, `ls`, `status <id>`, `say <id> "msg"`, `log <id>`, `stop <id>`, `watch`. Talks to the daemon over a Unix socket using hyper HTTP/1.1 client.
- **phylactd** — Daemon. Manages sessions and serves a REST API on a Unix socket (`axum` + `tokio`). Spawns `phyl-run` per session, tracks processes, reaps finished sessions. API: `GET /health`, `POST /sessions`, `GET /sessions`, `GET /sessions/:id`, `DELETE /sessions/:id`, `POST /sessions/:id/events`, `GET /feed` (SSE).
- **phyl-run** — Session runner. The agentic loop: discover tools, invoke model adapter, dispatch tool calls (oneshot in parallel, server-mode via NDJSON), manage FIFO events, write `log.jsonl`, finalize SOUL.md with reflection. Invoked as `phyl-run --session-dir <path> --prompt <text>`.
- **phyl-model-claude** — Model adapter. Translates between phylactery's JSON format and the `claude` CLI. Reads `ModelRequest` from stdin, writes `ModelResponse` to stdout.
- **phyl-tool-bash** — One-shot bash tool. Executes shell commands in `$PHYLACTERY_SESSION_DIR/scratch/` with timeout enforcement. Supports `--spec` for discovery.
- **phyl-tool-files** — One-shot file tool. Provides `read_file`, `write_file`, and `search_files` operations. Returns an array of `ToolSpec` from `--spec`. Supports `--spec` for discovery.
- **phyl-tool-session** — Server-mode tool. Provides `ask_human` (blocks for human response) and `done` (signals `end_session`). NDJSON on stdin/stdout. Supports `--spec` and `--serve`.
- **phyl-tool-mcp** — MCP bridge tool. Server-mode, NDJSON on stdin/stdout. Bridges to external MCP servers configured in `config.toml` (`[[mcp]]` sections). Implements MCP JSON-RPC 2.0 client protocol (initialize, tools/list, tools/call). Prefixes tool names with server name (e.g., `filesystem_read_file`). Supports `--spec` and `--serve`.
- **phyl-bridge-signal** — Signal Messenger bridge (stub).

## Key Protocols

All inter-process communication is JSON on stdin/stdout. The types in `phyl-core/src/lib.rs` define the contracts:

- **Model adapter**: reads `ModelRequest`, writes `ModelResponse`
- **One-shot tools**: read `ToolInput`, write `ToolOutput`
- **Server-mode tools**: NDJSON `ServerRequest`/`ServerResponse` with `id` field for multiplexing and optional `signal` field for out-of-band messages (e.g. `"end_session"`)
- **Tool discovery**: `phyl-tool-X --spec` prints `ToolSpec` (or array of them) with a `mode` field (`"oneshot"` or `"server"`)

## Conventions

- Rust edition 2024. Common dependencies (`serde`, `serde_json`, `chrono`, `uuid`, `toml`, `libc`, `tokio`, `axum`, `hyper`, `hyper-util`, `http-body-util`, `bytes`, `tower`) are declared as workspace dependencies in the root `Cargo.toml`.
- All serializable types derive `Serialize`/`Deserialize`. Enums use `#[serde(rename_all = "snake_case")]`. Optional fields use `skip_serializing_if`.
- Errors are returned as `Result<(), String>` in the current simple code. This will likely evolve to proper error types.
- Binaries log to stderr. Session conversation logs go to `log.jsonl` files.
- The `phyl init` git repo disables commit signing and sets a local `phylactery` identity to avoid depending on the user's global git config.

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

## Phase 2: Tool Protocol

- [ ] Implement `phyl-tool-bash`: `--spec` (with `"mode":"oneshot"`) and
      invocation mode. chdir to `$PHYLACTERY_SESSION_DIR/scratch/`, enforce
      timeout.
- [ ] Implement `phyl-tool-files`: read_file, write_file, search_files
- [ ] Test from command line:
      `echo '{"name":"bash","arguments":{"command":"echo hi"}}' | phyl-tool-bash`

## Phase 3: Model Adapter

- [ ] Implement `phyl-model-claude`:
      - Read `ModelRequest` from stdin
      - Translate to claude CLI invocation (`claude --print --output-format json`)
      - Parse claude's response
      - Write `ModelResponse` to stdout
- [ ] Test from command line:
      `echo '{"messages":[{"role":"user","content":"say hi"}],"tools":[]}' | phyl-model-claude`

## Phase 4: Session Runner

- [ ] Implement `phyl-run`:
      - Parse args (session dir, prompt)
      - Discover tools from path
      - Build system prompt from LAW.md + JOB.md + SOUL.md + knowledge/INDEX.md
      - Start server-mode tools
      - Run the agentic loop
      - Write to log.jsonl
      - Finalization step: SOUL.md reflection + done
      - PID file for daemon crash recovery
- [ ] Create FIFO, read events from it
- [ ] Test: `mkdir -p sessions/test && phyl-run --session-dir sessions/test --prompt "what is 2+2"`

## Phase 5: Daemon

- [ ] Implement `phylactd`
- [ ] API endpoints: POST/GET/DELETE sessions, POST events, GET health

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

- [ ] Implement `phyl-tool-session` (server mode)
- [ ] `GET /feed` SSE endpoint in daemon
- [ ] `phyl watch` CLI command

## Phase 10: Signal Bridge

- [ ] Implement `phyl-bridge-signal`

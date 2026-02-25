# phyl-run -- Session Runner

The agentic loop. Each session is one `phyl-run` process. The daemon spawns it, it runs until the task is done (or times out), then it exits.

## Invocation

```sh
phyl-run --session-dir <path> --prompt <text>
```

You don't call this directly -- the daemon does. But it's a standalone binary, so you can test it in isolation.

## The Loop

1. **Setup** -- Redirect stderr to `stderr.log`, write PID file, create scratch directory, create FIFO
2. **Load context** -- Read `LAW.md`, `JOB.md`, `SOUL.md`, `knowledge/INDEX.md`, enumerate knowledge file tree
3. **Discover tools** -- Scan `$PATH` for `phyl-tool-*` executables, call `--spec` on each, build dispatch table
4. **Start server-mode tools** -- Launch long-lived tools (`--serve`), keep stdin/stdout handles
5. **Agentic loop**:
   - Assemble `ModelRequest` with full message history and tool definitions
   - Invoke model adapter (configurable, default `phyl-model-claude`)
   - Retry on failure (configurable max retries)
   - Parse tool calls from response
   - Dispatch: one-shot tools in parallel (threads), server-mode tools via NDJSON
   - Collect results, check for `end_session` signal
   - Check FIFO for injected events (user messages, answers to questions)
   - If model responds without tool calls and no FIFO events after a brief wait: implicit done
   - Track cumulative token usage, compress history at configurable threshold
6. **Finalize** -- Close server-mode tools, update SOUL.md with reflection, git commit, clean up

## System Prompt

The system prompt is assembled from a template with sections:

```
=== LAW ===
{contents of LAW.md}

=== JOB ===
{contents of JOB.md}

=== SOUL ===
{contents of SOUL.md}

=== KNOWLEDGE SUMMARY ===
{file tree listing of knowledge/ directory}

=== SESSION ===
Session ID: {uuid}
Session directory: {path}
```

## Tool Discovery

Any executable on `$PATH` matching `phyl-tool-*` is a potential tool. The runner calls each with `--spec` and parses the JSON response. Tools declare themselves as `oneshot` or `server` mode.

Server-mode tools are started once at session begin and kept alive for the duration. One-shot tools are spawned fresh per invocation.

## Context Window Management

The runner tracks cumulative token usage (from the model's `usage` field, or `chars / 4` as a fallback). When the history exceeds a configurable threshold (default: 80% of 200k tokens), it compresses by summarizing the oldest messages via the model adapter.

## FIFO Events

The session creates a named pipe at `sessions/<uuid>/events`. Events can be injected from outside:

```sh
echo '{"type":"user","content":"New instructions"}' > sessions/$ID/events
```

The runner polls this FIFO alongside the model loop. User messages go into history. Answer events are routed to pending `ask_human` calls.

## Finalization

At session end:

1. Close stdin on all server-mode tools (they exit)
2. Acquire exclusive lock on `.soul.lock`
3. Re-read `SOUL.md` from disk (not the stale version from session start)
4. Invoke the model for reflection (session summary + current SOUL.md)
5. Write updated `SOUL.md`
6. Acquire `.git.lock`, commit SOUL.md
7. Release locks (soul first, then git)
8. Truncate SOUL.md if over 3000 words (keep first + last thirds)
9. Write `done` entry to `log.jsonl`
10. Remove FIFO and PID file

## Session Artifacts

Each session directory contains:

| File | Purpose |
|------|---------|
| `log.jsonl` | Append-only event log |
| `stderr.log` | Redirected stderr from the runner |
| `pid` | Process ID (removed on clean exit) |
| `events` | Named FIFO for event injection (removed on exit) |
| `scratch/` | Working directory for tools |

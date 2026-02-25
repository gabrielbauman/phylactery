# phylactd -- Daemon

The central process. Manages sessions, serves a REST API on a Unix socket, and provides the SSE attention feed that bridges connect to.

## What It Does

- Spawns `phyl-run` as a child process for each session
- Tracks session state in memory (id, status, pid, prompt, summary)
- Enforces max concurrent sessions
- Reaps finished and crashed sessions (polls every 5 seconds)
- Recovers state on restart by scanning `sessions/` for orphaned processes
- Serves the REST API that everything else talks to

## API

All endpoints are served on a Unix socket. No TCP, no network exposure.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Returns status, active/total session counts |
| `POST` | `/sessions` | Start session (`{"prompt":"..."}`) -- returns 201 |
| `GET` | `/sessions` | List all sessions (newest first) |
| `GET` | `/sessions/:id` | Session detail with prompt + last 50 log entries |
| `DELETE` | `/sessions/:id` | Kill running session (SIGTERM then SIGKILL) -- returns 204 |
| `POST` | `/sessions/:id/events` | Inject event into session FIFO -- returns 202 |
| `GET` | `/feed` | SSE stream of attention events across all sessions |

### The Attention Feed

`GET /feed` returns a Server-Sent Events stream. It tails all active session `log.jsonl` files and emits events that need human attention:

- **question** -- the agent is asking a human something
- **done** -- a session completed (with summary)
- **error** -- something went wrong

Bridges connect to this endpoint. So does `phyl watch`.

## Socket

Default path: `$XDG_RUNTIME_DIR/phylactery.sock` (or `/tmp/phylactery.sock`). Configurable via `config.toml`:

```toml
[daemon]
socket = "/run/user/1000/phylactery.sock"
```

Permissions are set to 0700 (owner-only). The socket is cleaned up on shutdown and on startup (stale socket removal).

## Startup

1. Read `config.toml` from `$PHYLACTERY_HOME`
2. Verify `$PHYLACTERY_HOME` exists (fail with message to run `phyl init` if not)
3. Remove stale socket if present
4. Bind Unix socket, set permissions
5. Write PID file to `$XDG_RUNTIME_DIR/phylactd.pid`
6. Scan `sessions/` for crash recovery (re-adopt running processes, mark dead ones)
7. Start the reaper background task
8. Serve the API

## Crash Recovery

On startup, the daemon scans `sessions/` for directories with `pid` files. For each:

- If the process is still running (`kill -0`): re-adopt it into the session table
- If the process is dead: mark the session as crashed, extract timestamps and prompts from `log.jsonl`

This means you can restart the daemon without losing track of running sessions.

## Configuration

```toml
[daemon]
socket = "..."              # Unix socket path

[session]
max_concurrent = 4          # max simultaneous sessions
timeout = 3600              # session timeout in seconds
```

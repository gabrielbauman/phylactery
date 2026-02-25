# Sessions

A session is a conversation with a goal. Each session is a separate OS process (`phyl-run`). Sessions are created by humans, scripts, webhooks, pollers, file watchers -- anything that can talk to the daemon.

## Lifecycle

1. **Created**: something calls `POST /sessions` on the daemon (or `phyl session "..."`)
2. **Running**: the daemon spawns `phyl-run`, which runs the agentic loop
3. **Done**: the model calls the `done` tool, or the session times out, or the daemon kills it
4. **Finalized**: SOUL.md is updated with a reflection, artifacts are written

## What Each Session Gets

- A UUID
- A working directory: `sessions/<uuid>/`
- An append-only event log: `sessions/<uuid>/log.jsonl`
- An input FIFO: `sessions/<uuid>/events` (named pipe for live injection)
- A scratch directory: `sessions/<uuid>/scratch/` (tool working directory)
- Access to the shared knowledge base
- Access to all discovered tools

## Interacting with Running Sessions

Sessions are interactive. You can inject events while they're running:

```sh
# Via the CLI
phyl say <id> "Actually, also check Signal messages"

# Via the daemon API
curl --unix-socket /tmp/phylactery.sock \
  -X POST http://localhost/sessions/<id>/events \
  -d '{"content":"New instructions"}'

# Via the FIFO directly
echo '{"type":"user","content":"Hey"}' > sessions/<uuid>/events
```

## Session Endings

A session ends when:

- The model calls the `done` tool (clean exit with summary)
- The daemon kills the process (user cancellation via `phyl stop`)
- It times out (configurable, default 1 hour)
- The process crashes (marked as crashed, no finalization)

## Concurrency

Multiple sessions can run simultaneously (up to `max_concurrent`, default 4). They share:

- The knowledge base (writes serialized via `flock`)
- SOUL.md (updates serialized via `.soul.lock`)
- The daemon API
- Tool binaries (each session spawns its own instances)

## Session Status

| Status | Meaning |
|--------|---------|
| `running` | Session process is alive |
| `done` | Completed normally (summary available) |
| `crashed` | Process exited unexpectedly |
| `timed_out` | Exceeded configured timeout |

## Event Log

Every session writes a structured log to `log.jsonl`:

```jsonl
{"ts":"...","type":"system","content":"Session started"}
{"ts":"...","type":"user","content":"Check my email"}
{"ts":"...","type":"assistant","content":"I'll check...","tool_calls":[...]}
{"ts":"...","type":"tool_result","tool_call_id":"tc_1","content":"3 new messages"}
{"ts":"...","type":"question","id":"q_1","content":"Summarize them?","options":["yes","no"]}
{"ts":"...","type":"answer","question_id":"q_1","content":"yes"}
{"ts":"...","type":"done","summary":"Checked email, reported 3 new messages"}
```

This log is the source of truth. The daemon tails it for the SSE feed. The CLI reads it for `phyl log` and `phyl status`.

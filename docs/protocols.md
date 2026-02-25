# Protocols

All inter-process communication in Phylactery is JSON on stdin/stdout. The types in `phyl-core/src/lib.rs` are the canonical definitions. This document describes them in human terms.

## Model Adapter Protocol

A model adapter is an executable. It reads one JSON object from stdin, writes one JSON object to stdout, then exits.

### Request (stdin)

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
      "parameters": {
        "type": "object",
        "properties": {
          "command": { "type": "string" }
        },
        "required": ["command"]
      }
    }
  ]
}
```

### Response (stdout)

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

The `usage` field is optional. If present, the session runner uses it for context window tracking. If absent, the runner falls back to a `chars / 4` heuristic.

Want to use a different model provider? Write a new adapter. Same contract. A shell script could do it:

```sh
#!/bin/sh
# phyl-model-echo -- a test adapter that echoes the last message
jq -r '.messages[-1].content' | jq -R '{content: ., tool_calls: []}'
```

## Tool Protocol

Tools support up to three modes: discovery, one-shot, and server.

### Discovery (`--spec`)

Every tool must support this. Print tool schema(s) to stdout and exit.

```sh
$ phyl-tool-bash --spec
{
  "name": "bash",
  "description": "Execute a shell command and return its output",
  "mode": "oneshot",
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
  { "name": "read_file", "mode": "oneshot", ... },
  { "name": "write_file", "mode": "oneshot", ... },
  { "name": "search_files", "mode": "oneshot", ... }
]
```

The `mode` field declares how the tool is invoked: `"oneshot"` (default if omitted) or `"server"`.

### One-Shot Mode (default)

Read one tool call from stdin, write one result to stdout, exit. Good for stateless operations.

**Input (stdin):**
```json
{ "name": "bash", "arguments": { "command": "ls" } }
```

**Output (stdout):**
```json
{ "output": "file1.txt\nfile2.txt\n" }
```

**Error:**
```json
{ "error": "Command exited with status 1" }
```

### Server Mode (`--serve`)

For long-lived, stateful tools. The tool starts, stays running for the life of the session, and handles multiple calls over newline-delimited JSON (NDJSON).

```
→ {"id":"1","name":"brave_search","arguments":{"query":"rust async"}}
← {"id":"1","output":"...results..."}
→ {"id":"2","name":"brave_search","arguments":{"query":"tokio tutorial"}}
← {"id":"2","output":"...results..."}
```

The `id` field ties requests to responses. A server-mode response can include a `signal` field:

```json
{"id":"5","output":"Session complete.","signal":"end_session"}
```

The `end_session` signal tells the session runner to finalize. This is how the `done` tool works -- it returns a summary AND signals shutdown. No special-casing of tool names.

### Tool Environment Variables

Tools receive context via environment variables:

| Variable | Value |
|----------|-------|
| `PHYLACTERY_SESSION_ID` | UUID of the current session |
| `PHYLACTERY_SESSION_DIR` | Absolute path to session working directory |
| `PHYLACTERY_KNOWLEDGE_DIR` | Absolute path to knowledge base |
| `PHYLACTERY_HOME` | Absolute path to agent home directory |

### Which Tools Use Which Mode

| Tool | Mode | Reason |
|------|------|--------|
| `phyl-tool-bash` | One-shot | Stateless. Each command is independent. |
| `phyl-tool-files` | One-shot | Stateless. Read/write/search. |
| `phyl-tool-mcp` | Server | MCP servers are long-lived processes. |
| `phyl-tool-session` | Server | `ask_human` blocks for minutes; `done` signals shutdown. |

## Event Log Format

Each session writes to `sessions/<uuid>/log.jsonl`. One JSON object per line:

```jsonl
{"ts":"2026-02-24T12:00:00Z","type":"system","content":"Session started"}
{"ts":"2026-02-24T12:00:00Z","type":"user","content":"Check my email"}
{"ts":"2026-02-24T12:00:01Z","type":"assistant","content":"I'll check...","tool_calls":[...]}
{"ts":"2026-02-24T12:00:02Z","type":"tool_result","tool_call_id":"tc_1","content":"3 new messages"}
{"ts":"2026-02-24T12:00:03Z","type":"assistant","content":"You have 3 new emails..."}
{"ts":"2026-02-24T12:00:04Z","type":"done","summary":"Checked email, reported 3 new messages"}
```

## Daemon API

The daemon serves a REST API on a Unix socket.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Status, active/total session counts |
| `POST` | `/sessions` | Start a session (`{"prompt":"..."}`) |
| `GET` | `/sessions` | List all sessions |
| `GET` | `/sessions/:id` | Session detail with recent log entries |
| `DELETE` | `/sessions/:id` | Kill a running session |
| `POST` | `/sessions/:id/events` | Inject event into session FIFO |
| `GET` | `/feed` | SSE stream of attention events across all sessions |

## Bridge Protocol

A bridge is any program that:

1. Connects to `GET /feed` on the daemon's Unix socket
2. Presents events to a human (terminal, Signal, Matrix, email, anything)
3. Collects responses
4. Posts them back via `POST /sessions/:id/events`

That's the entire contract. A bridge doesn't need to know about models, tools, or the knowledge base.

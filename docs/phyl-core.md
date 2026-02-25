# phyl-core -- Shared Types Library

The single source of truth for all protocols. Every other crate depends on this one. It defines the JSON contracts that hold the system together.

## What It Contains

All serializable types used across process boundaries:

### Message Protocol

- `Message` -- role + content + optional tool calls/tool_call_id
- `Role` -- System, User, Assistant, Tool
- `ToolCall` -- id + name + arguments (as serde_json::Value)

### Model Adapter Types

- `ModelRequest` -- messages + tools (what the session runner sends to model adapters)
- `ModelResponse` -- content + tool_calls + optional usage
- `Usage` -- input_tokens + output_tokens

### Tool Types

- `ToolSpec` -- name + description + mode + parameters + optional sandbox
- `ToolMode` -- Oneshot or Server
- `ToolInput` -- name + arguments (for one-shot tools)
- `ToolOutput` -- output or error (one-shot response)
- `ServerRequest` -- id + name + arguments (for NDJSON server-mode tools)
- `ServerResponse` -- id + output/error + optional signal

### Logging

- `LogEntry` -- timestamp + type + content + metadata
- `LogEntryType` -- System, User, Assistant, ToolResult, Question, Answer, Done, Error

### Session

- `SessionStatus` -- Running, Done, Crashed, TimedOut
- `SessionInfo` -- id + status + prompt + summary + timestamps

### Configuration

- `Config` -- top-level config.toml structure
- `DaemonConfig` -- socket path
- `SessionConfig` -- max_concurrent, timeout, model adapter, retries, context window
- `ModelConfig` -- adapter binary name
- `GitConfig` -- auto_commit flag
- `McpServerConfig` -- MCP server definitions
- `PollConfig` -- poll rule definitions
- `ListenConfig` -- listener settings (hooks, SSE, watches)
- `BridgeConfig` / `SignalBridgeConfig` -- Signal bridge settings

### Home Directory

- `phylactery_home()` -- resolves `$PHYLACTERY_HOME` with XDG fallback

## Conventions

- All types derive `Serialize` and `Deserialize`
- Enums use `#[serde(rename_all = "snake_case")]`
- Optional fields use `#[serde(skip_serializing_if = "Option::is_none")]`
- Default values provided via `Default` implementations

## Defaults

| Setting | Default |
|---------|---------|
| Context window | 200,000 tokens |
| Compress at | 80% of context window |
| Session timeout | 3,600 seconds (1 hour) |
| Max concurrent sessions | 4 |
| Model adapter | `phyl-model-claude` |
| Socket path | `$XDG_RUNTIME_DIR/phylactery.sock` |

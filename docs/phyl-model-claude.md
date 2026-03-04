# phyl-model-claude -- Model Adapter for Claude

Translates between Phylactery's JSON protocol and the `claude` CLI. Reads a `ModelRequest` from stdin, invokes the Claude CLI, parses the response, and writes a `ModelResponse` to stdout.

## Protocol

- **Input**: `ModelRequest` on stdin (messages array + tools array)
- **Output**: `ModelResponse` on stdout (content + tool_calls + optional usage)

See [Protocols](protocols.md) for the full JSON schemas.

## How It Works

1. Read `ModelRequest` from stdin
2. Build a system prompt from system messages + formatted tool definitions (using `<tool_call>` XML format instructions)
3. Build a user prompt from conversation history (multi-turn support with `<conversation_history>` formatting)
4. Invoke the Claude CLI:
   ```sh
   claude --print --output-format json --no-session-persistence \
     --tools "" --system-prompt "..." --model <optional>
   ```
5. Parse the JSON response (`result`, `is_error` fields)
6. Extract `<tool_call>` blocks from response text into structured `ToolCall` objects
7. Write `ModelResponse` to stdout

## Tool Call Format

The adapter instructs the model to express tool calls using XML tags:

```xml
<tool_call>
{"name": "bash", "arguments": {"command": "ls -la"}}
</tool_call>
```

These are extracted from the response text and converted into structured `ToolCall` objects in the `ModelResponse`.

## Environment Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `PHYL_CLAUDE_CLI` | `claude` | Path to the Claude CLI binary |
| `PHYL_CLAUDE_MODEL` | *(none)* | Model name override (e.g., `claude-sonnet-4-20250514`) |

## Limitations

The Claude adapter uses the `claude` CLI in `--print` mode, which runs through Claude Code rather than the Anthropic API directly. Tool calling uses a prompt-based XML format (`<tool_call>` tags) rather than the API's native `tool_use` protocol. This is inherently less reliable than native tool calling — the model may occasionally:

- Describe tool actions in prose instead of calling them
- Use an incorrect format for tool calls
- Attempt to call tools that don't exist

For the most reliable tool calling, use `phyl-model-openai` with a provider that supports native tool_use (set `PHYL_OPENAI_TOOL_MODE=native`), or the forthcoming `phyl-model-anthropic` adapter which will use the Anthropic Messages API directly.

The Claude CLI adapter exists primarily to let users with Claude Pro/Max subscriptions use phylactery without a separate API account.

## Writing Your Own Adapter

The model adapter protocol is intentionally simple. Want to use Ollama, OpenAI, or a local model? Write a new adapter binary. The contract is:

1. Read `ModelRequest` JSON from stdin
2. Do whatever you need to get a response from a model
3. Write `ModelResponse` JSON to stdout
4. Exit

A shell script works fine for testing:

```sh
#!/bin/sh
# phyl-model-echo -- echoes the last message
jq -r '.messages[-1].content' | jq -R '{content: ., tool_calls: []}'
```

Configure which adapter to use in `config.toml`:

```toml
[session]
model_adapter = "phyl-model-ollama"
```

# phyl-model-anthropic -- Native Anthropic Messages API Adapter

Connects Phylactery directly to the Anthropic Messages API using native structured tool calling (`tool_use` content blocks). This gives the most reliable tool calling experience for users with Anthropic API keys -- no prompt engineering hacks, no XML parsing, no CLI subprocess.

## Protocol

- **Input**: `ModelRequest` on stdin (messages array + tools array)
- **Output**: `ModelResponse` on stdout (content + tool_calls + optional usage)

See [Protocols](protocols.md) for the full JSON schemas.

## How It Works

1. Read `ModelRequest` from stdin
2. Extract system messages into the top-level `system` field
3. Convert remaining messages to Anthropic format with strict role alternation (merging adjacent same-role messages)
4. Map tool definitions to Anthropic's `tools` parameter (using `input_schema` instead of `parameters`)
5. POST to `{base_url}/v1/messages` with `x-api-key` and `anthropic-version` headers
6. Parse response: text blocks become content, `tool_use` blocks become tool calls
7. Write `ModelResponse` to stdout

## Environment Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `PHYL_ANTHROPIC_API_KEY` | *(required)* | Anthropic API key |
| `PHYL_ANTHROPIC_MODEL` | `claude-sonnet-4-6` | Model to use |
| `PHYL_ANTHROPIC_MAX_TOKENS` | `8192` | Maximum tokens in response |
| `PHYL_ANTHROPIC_URL` | `https://api.anthropic.com` | Base URL (for proxies or compatible endpoints) |
| `PHYL_ANTHROPIC_TIMEOUT` | `300` | Request timeout in seconds |

## Quick Start

```sh
# Set your API key
export PHYL_ANTHROPIC_API_KEY=sk-ant-...

# Configure phylactery to use this adapter
# In $PHYLACTERY_HOME/config.toml:
#   [session]
#   model = "phyl-model-anthropic"

# Start a session
phyl session "Hello from the Anthropic API"
```

### Manual test

```sh
export PHYL_ANTHROPIC_API_KEY=sk-ant-...
echo '{"messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"What is 2+2?"}],"tools":[]}' \
  | cargo run -p phyl-model-anthropic
```

## Comparison with phyl-model-claude

| | phyl-model-claude | phyl-model-anthropic |
|---|---|---|
| **Requires** | `claude` CLI installed | API key (`PHYL_ANTHROPIC_API_KEY`) |
| **Tool calling** | XML-based via prompt engineering | Native `tool_use` content blocks |
| **Reliability** | Depends on CLI behavior and XML parsing | Structured API contract |
| **Token tracking** | Not available | Full usage reporting |
| **Billing** | Via Claude CLI subscription | Via Anthropic API usage |

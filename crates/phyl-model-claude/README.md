# phyl-model-claude

Model adapter that translates between phylactery's JSON protocol and the
[Claude Code CLI](https://code.claude.com/docs/en/cli-reference).

## How it works

`phyl-model-claude` is a one-shot executable. It reads a `ModelRequest` from
stdin, invokes the `claude` CLI in print mode, parses the response, and writes
a `ModelResponse` to stdout.

```
ModelRequest (stdin)
  → build system prompt (system messages + tool definitions)
  → build user prompt (conversation history)
  → claude --print --output-format json --tools "" --system-prompt "..."
  → parse JSON response
  → extract <tool_call> blocks from result text
ModelResponse (stdout)
```

### Tool calling

The claude CLI's built-in tools are disabled (`--tools ""`). Instead,
phylactery's tool definitions are included in the system prompt with
instructions for the model to express tool calls using XML tags:

```
<tool_call>
{"name": "bash", "arguments": {"command": "ls -la"}}
</tool_call>
```

The adapter parses these tags from the response text, extracts the JSON, and
returns them as structured `ToolCall` objects in the `ModelResponse`. Each tool
call receives a unique UUID-based ID (e.g. `tc_550e8400-...`).

Text outside `<tool_call>` blocks becomes the `content` field of the response.

### Conversation history

For multi-turn conversations, the adapter formats the message history:

- **System messages** → passed via `--system-prompt`
- **User/assistant/tool messages** → formatted as text in the user prompt,
  wrapped in `<conversation_history>` tags, with the final message as the
  current turn

## Usage

```sh
# Basic invocation
echo '{"messages":[{"role":"user","content":"say hi"}],"tools":[]}' | phyl-model-claude

# With tools
echo '{"messages":[{"role":"user","content":"list files"}],"tools":[{"name":"bash","description":"Run a command","mode":"oneshot","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}]}' | phyl-model-claude
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PHYL_CLAUDE_CLI` | `claude` | Path to the claude CLI binary |
| `PHYL_CLAUDE_MODEL` | *(CLI default)* | Model to use (e.g. `sonnet`, `opus`, `claude-sonnet-4-6`) |

## Claude CLI JSON output format

The adapter expects the claude CLI's `--output-format json` response:

```json
{
  "result": "The model's text response...",
  "is_error": false,
  "session_id": "abc123",
  "num_turns": 1,
  "cost_usd": 0.01,
  "duration_ms": 2345,
  "duration_api_ms": 1234
}
```

Key fields:

- `result` — the model's text output (may contain `<tool_call>` blocks)
- `is_error` — `true` if the CLI encountered an error
- `session_id`, `num_turns`, `cost_usd`, `duration_ms`, `duration_api_ms` —
  metadata (captured but not currently used in the response)

Token usage is not exposed by the CLI's JSON output, so `ModelResponse.usage`
is always `None`. The session runner falls back to character-based estimation
per the plan.

## CLI flags used

| Flag | Purpose |
|------|---------|
| `--print` | Non-interactive single-shot mode |
| `--output-format json` | Structured JSON output |
| `--tools ""` | Disable all built-in tools |
| `--no-session-persistence` | Don't save CLI sessions to disk |
| `--system-prompt` | Replace the default system prompt |
| `--model` | Override model (when `PHYL_CLAUDE_MODEL` is set) |

The `CLAUDECODE` environment variable is unset before spawning the CLI to
avoid the nested-session guard when running inside a Claude Code session.

## Design decisions

**Why disable built-in tools?** Phylactery's architecture has the session
runner (`phyl-run`) dispatch tool calls, not the model adapter. The adapter
returns tool calls as data; the runner executes them. Disabling the CLI's tools
prevents it from executing tools internally.

**Why XML tags for tool calls?** The claude CLI in `--print` mode returns a
single `result` string. Native API tool_use blocks aren't exposed through
this interface. XML tags provide a reliable, parseable format that Claude
follows consistently.

**Why not use the Anthropic API directly?** The plan specifies the claude CLI
as the model backend. This keeps authentication, model selection, and API
details managed by the CLI. Swapping model backends is done by replacing the
adapter binary — the session runner doesn't change.

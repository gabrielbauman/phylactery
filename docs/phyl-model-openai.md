# phyl-model-openai -- Model Adapter for OpenAI-Compatible APIs

Connects Phylactery to any model server that implements the OpenAI `/v1/chat/completions` endpoint. This covers most local inference servers: Ollama, llama.cpp (with `--api`), vLLM, LM Studio, LocalAI, and cloud providers that use the same format (Together, Groq, OpenRouter, etc.).

## Protocol

- **Input**: `ModelRequest` on stdin (messages array + tools array)
- **Output**: `ModelResponse` on stdout (content + tool_calls + optional usage)

See [Protocols](protocols.md) for the full JSON schemas.

## How It Works

1. Read `ModelRequest` from stdin
2. Translate messages to OpenAI chat format (system/user/assistant/tool roles)
3. Include tool definitions either as native `tools` parameter or embedded in system prompt (XML mode)
4. POST to `{base_url}/chat/completions`
5. Parse the response, extracting tool calls (native or XML `<tool_call>` tags)
6. Write `ModelResponse` to stdout

## Tool Calling Modes

### XML Mode (default)

Tool definitions are embedded in the system prompt with instructions to use `<tool_call>` XML tags. This works with **any** model, even those without native function calling support. Tool results from prior turns are sent as user messages.

This is the default because most small local models don't reliably support native tool calling.

### Native Mode (`PHYL_OPENAI_TOOL_MODE=native`)

Uses the OpenAI `tools` parameter and structured `tool_calls` in the response. This is more reliable when the model supports it (e.g., Qwen, Llama 3.1+, Mistral with function calling, models served by vLLM with guided decoding).

## Environment Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `PHYL_OPENAI_URL` | `http://localhost:11434/v1` | Base URL of the API (no trailing slash) |
| `PHYL_OPENAI_MODEL` | `gemma3n` | Model name to request |
| `PHYL_OPENAI_API_KEY` | *(none)* | Bearer token for authentication |
| `PHYL_OPENAI_TOOL_MODE` | `xml` | `xml` or `native` |
| `PHYL_OPENAI_TIMEOUT` | `300` | Request timeout in seconds |

## Quick Start

### With Ollama

```sh
# Pull a model
ollama pull gemma3:4b

# Configure phylactery
# In $PHYLACTERY_HOME/config.toml:
#   [session]
#   model = "phyl-model-openai"

# Set environment (Ollama serves OpenAI-compatible API on port 11434)
export PHYL_OPENAI_URL=http://localhost:11434/v1
export PHYL_OPENAI_MODEL=gemma3:4b

# Start a session
phyl session "Hello from a local model"
```

### With llama.cpp

```sh
# Start llama.cpp server
llama-server -m gemma-3n-E4B-it-Q4_K_M.gguf --port 8080

# Point the adapter at it
export PHYL_OPENAI_URL=http://localhost:8080/v1
export PHYL_OPENAI_MODEL=gemma-3n
```

### With vLLM

```sh
# Start vLLM (supports native tool calling with guided decoding)
vllm serve google/gemma-3n-E4B-it --port 8000

export PHYL_OPENAI_URL=http://localhost:8000/v1
export PHYL_OPENAI_MODEL=google/gemma-3n-E4B-it
export PHYL_OPENAI_TOOL_MODE=native
```

### With a Cloud Provider

```sh
# Example: Together AI
export PHYL_OPENAI_URL=https://api.together.xyz/v1
export PHYL_OPENAI_MODEL=meta-llama/Llama-3.1-8B-Instruct
export PHYL_OPENAI_API_KEY=your-api-key
```

## Model Recommendations

For agentic use (tool calling in a loop), larger models perform better. Some options:

| Model | Size | Notes |
|-------|------|-------|
| Gemma 3n E4B | ~4B effective | Lightweight, good for simple tasks |
| Llama 3.1 8B | 8B | Solid native tool calling support |
| Qwen 2.5 7B | 7B | Strong instruction following |
| Mistral 7B v0.3 | 7B | Function calling fine-tuned variant available |

Smaller models may struggle with complex multi-step tool use. Start with XML mode and upgrade to native mode if the model handles it reliably.

## Context Window

When using a local model, adjust the context window setting in `config.toml` to match the model's actual capacity:

```toml
[model]
context_window = 8192    # Match your model's context length
compress_at = 0.8        # Compress at 80% of context window
```

The default (200k) is sized for Claude. Local models typically have 4k-32k context windows.

# Phylactery Documentation

Detailed documentation for every component in the Phylactery system.

## Table of Contents

### Architecture

- [Architecture Overview](architecture.md) -- How the pieces fit together and why there are so many of them
- [Protocols](protocols.md) -- The JSON contracts that hold everything together
- [Configuration](configuration.md) -- `config.toml`, `secrets.env`, and how to wire it all up

### Core Components

- [phyl](phyl.md) -- The CLI client you'll actually type commands into
- [phylactd](phylactd.md) -- The daemon that manages sessions and serves the API
- [phyl-run](phyl-run.md) -- The session runner: where the agentic loop lives
- [phyl-core](phyl-core.md) -- Shared types library, the source of truth for all protocols

### Model Adapters

- [phyl-model-claude](phyl-model-claude.md) -- Model adapter for the Claude CLI
- [phyl-model-openai](phyl-model-openai.md) -- Model adapter for OpenAI-compatible APIs (Ollama, llama.cpp, vLLM, LM Studio, etc.)

### Tools

- [phyl-tool-bash](phyl-tool-bash.md) -- Shell command execution
- [phyl-tool-files](phyl-tool-files.md) -- File read/write/search operations
- [phyl-tool-session](phyl-tool-session.md) -- Human interaction and session control
- [phyl-tool-mcp](phyl-tool-mcp.md) -- Bridge to any MCP server
- [phyl-tool-self](phyl-tool-self.md) -- Agent self-direction: spawn sessions, schedule future work, sleep

### Services

- [phyl-sched](phyl-sched.md) -- Scheduler: fires due scheduled entries

### Event Sources

- [phyl-poll](phyl-poll.md) -- Turn any CLI command into an event source by polling for changes
- [phyl-listen](phyl-listen.md) -- Receive webhooks, subscribe to SSE streams, watch files

### Bridges

- [phyl-bridge-signal](phyl-bridge-signal.md) -- Two-way Signal Messenger interface

### Concepts

- [Sessions](sessions.md) -- What a session is and how it works
- [Knowledge Base](knowledge-base.md) -- The agent's long-term memory
- [Identity Files](identity-files.md) -- LAW.md, JOB.md, SOUL.md: the three-layer identity system

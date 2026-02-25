# phyl-tool-mcp -- MCP Bridge

A server-mode tool that bridges to external [Model Context Protocol](https://modelcontextprotocol.io/) servers. Any MCP server becomes available as a tool in your agent's sessions.

## Modes

### Discovery (`--spec`)

Starts each configured MCP server, performs the initialize handshake, queries `tools/list`, aggregates all tools into a single `Vec<ToolSpec>` with server-name-prefixed tool names, then shuts down the MCP servers.

```sh
phyl-tool-mcp --spec
# → [{"name":"filesystem_read_file","mode":"server",...}, ...]
```

Tool names are prefixed with the server name and an underscore: a tool `read_file` from MCP server `filesystem` becomes `filesystem_read_file`.

### Server mode (`--serve`)

Long-running NDJSON bridge. Starts all configured MCP servers, builds a routing table, and dispatches incoming tool calls to the correct server.

```
→ {"id":"1","name":"brave_search","arguments":{"query":"rust async"}}
← {"id":"1","output":"...search results..."}
```

On stdin EOF (session ending), shuts down all MCP servers gracefully.

### One-shot CLI (`--call`)

For use outside sessions (e.g., from `phyl-poll`):

```sh
phyl-tool-mcp --call slack get_mentions '{}'
```

This starts the named MCP server, calls the tool, prints the result, and exits.

## MCP Protocol Implementation

Implements the MCP JSON-RPC 2.0 client protocol:

1. `initialize` handshake (`protocolVersion: "2024-11-05"`)
2. `notifications/initialized`
3. `tools/list` -- discover available tools
4. `tools/call` -- invoke a tool

Handles notifications and unexpected messages from MCP servers gracefully (logged to stderr, not fatal).

## Configuration

In `config.toml`:

```toml
[[mcp]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user"]

[[mcp]]
name = "brave"
command = "npx"
args = ["-y", "@anthropic/mcp-brave-search"]
env = { BRAVE_API_KEY = "$BRAVE_API_KEY" }
```

Environment variables in `env` values are expanded from the process environment (including `secrets.env` values loaded by the parent process).

## Composing with phyl-poll

Since `phyl-tool-mcp` has a `--call` mode, any MCP server becomes a pollable data source:

```toml
[[poll]]
name = "slack-mentions"
command = "phyl-tool-mcp"
args = ["--call", "slack", "get_mentions", "{}"]
interval = 180
prompt = "New Slack mentions. Summarize and flag anything needing a reply."
```

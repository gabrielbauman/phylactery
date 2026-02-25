# Configuration

All configuration lives in `$PHYLACTERY_HOME/config.toml`. Secrets live in `$PHYLACTERY_HOME/secrets.env`.

## config.toml

Created by `phyl init` with sensible defaults. Sections are added as you configure features.

### Daemon Settings

```toml
[daemon]
socket = "/run/user/1000/phylactery.sock"  # default: $XDG_RUNTIME_DIR/phylactery.sock
```

### Session Settings

```toml
[session]
max_concurrent = 4         # max simultaneous sessions
timeout_minutes = 60       # session timeout in minutes (default: 60)
model = "phyl-model-claude"  # model adapter binary
```

To use a local model instead of Claude, switch to the OpenAI-compatible adapter:

```toml
[session]
model = "phyl-model-openai"

[model]
context_window = 8192      # match your local model's context length
```

Then set environment variables for the adapter (e.g., in `secrets.env` or your shell):

```
PHYL_OPENAI_URL=http://localhost:11434/v1
PHYL_OPENAI_MODEL=gemma3:4b
```

See [phyl-model-openai](phyl-model-openai.md) for full details.

### Git Settings

```toml
[git]
auto_commit = true         # auto-commit knowledge base changes
```

### MCP Servers

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

Environment variables in `env` values are expanded from the process environment and `secrets.env`.

### Poll Rules

```toml
[[poll]]
name = "github-notifications"
command = "gh"
args = ["api", "/notifications", "--jq", ".[].subject.title"]
interval = 300              # seconds
prompt = "Review these new GitHub notifications."
timeout = 30                # command timeout in seconds

[[poll]]
name = "mailbox"
shell = true                # run via sh -c
command = "notmuch search --output=summary tag:inbox AND tag:unread | head -20"
interval = 120
prompt = "New unread mail arrived. Summarize the new messages."
```

### Listener Settings

```toml
[listen]
bind = "127.0.0.1:7890"    # TCP address for webhook receiver
```

#### Webhooks

```toml
[[listen.hook]]
name = "github"
path = "/hook/github"
secret = "$GITHUB_WEBHOOK_SECRET"
prompt = "A GitHub event arrived."
route_header = "X-GitHub-Event"
routes = { push = "Code was pushed.", pull_request = "A PR was opened." }
rate_limit = 10             # max sessions per minute
dedup_header = "X-GitHub-Delivery"
max_body_size = 1048576     # bytes (default: 1 MB)
```

#### SSE Subscriptions

```toml
[[listen.sse]]
name = "deploy-events"
url = "https://internal.example.com/events/stream"
prompt = "A deployment event occurred."
headers = { Authorization = "Bearer $DEPLOY_API_TOKEN" }
events = ["deploy_start", "deploy_fail"]
route_event = true
routes = { deploy_fail = "A deployment failed. Investigate." }
rate_limit = 5
```

#### File Watches

```toml
[[listen.watch]]
name = "inbox"
path = "/home/user/agent-inbox/"
recursive = true
events = ["create"]
glob = "*.eml"
debounce = 2                # seconds (default: 2)
prompt = "A new file appeared in the inbox."
```

### Signal Bridge

```toml
[bridge.signal]
phone = "+1234567890"       # Agent's Signal number
owner = "+0987654321"       # Your Signal number (only accept from this)
signal_cli = "signal-cli"   # Path to signal-cli binary
```

## secrets.env

A simple `KEY=VALUE` file at `$PHYLACTERY_HOME/secrets.env`. Created by `phyl init` with mode 600 (owner-only). Gitignored.

```
GITHUB_WEBHOOK_SECRET=abc123
BRAVE_API_KEY=sk-...
DEPLOY_API_TOKEN=ghp_...
```

Manage secrets with:

```sh
phyl config add-secret BRAVE_API_KEY sk-xxx
phyl config list-secrets
phyl config remove-secret OLD_KEY
```

## Configuration Management

```sh
phyl config show              # Pretty-print config with secrets masked
phyl config validate          # Check for errors and missing references
phyl config edit              # Open in $EDITOR, validate on save
phyl config add mcp myserver  # Append a template section
phyl config add poll mycheck  # Append a poll template
phyl config add hook mywebhook  # Append a webhook template
```

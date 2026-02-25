# phyl-poll -- Command Poller

Turns any CLI command into an event source. Runs commands on configurable intervals, compares output to previous results, and starts sessions when changes are detected. This is how the agent reacts to the outside world without dedicated integrations.

## How It Works

1. Read all `[[poll]]` rules from `config.toml`
2. Create `$PHYLACTERY_HOME/poll/` directory if needed (gitignored)
3. For each rule, on each interval:
   - Run the command, capture stdout
   - Read previous output from `poll/<name>.last`
   - If no previous output: save as baseline, skip (no false-positive on first run)
   - If output identical: skip
   - If output changed: assemble prompt with diff context, `POST /sessions` to daemon
   - Save current output as new baseline

## Prompt Assembly

When a change is detected, the session gets full context:

```
{rule.prompt}

=== PREVIOUS OUTPUT ===
{previous output}

=== CURRENT OUTPUT ===
{current output}

=== DIFF ===
{line-by-line diff}
```

## Configuration

```toml
[[poll]]
name = "github-notifications"
command = "gh"
args = ["api", "/notifications", "--jq", ".[].subject.title"]
interval = 300              # seconds (default: 300, minimum: 10)
prompt = "Review these new GitHub notifications."

[[poll]]
name = "mailbox"
shell = true                # run via sh -c instead of exec
command = "notmuch search --output=summary tag:inbox AND tag:unread | head -20"
interval = 120
prompt = "New unread mail arrived. Summarize the new messages."
env = { NOTMUCH_CONFIG = "/home/user/.notmuch-config" }
timeout = 30                # command timeout in seconds (default: 30)
```

## Scheduling

- Each rule runs independently on its own interval
- Initial polls are staggered (100ms apart) to avoid thundering herd on startup
- If a command takes longer than its interval, the next tick is skipped (no pile-up)
- Minimum interval: 10 seconds

## Failure Handling

- **Command timeout**: killed after configured timeout (default 30s)
- **Non-zero exit**: logged to stderr, baseline not updated, no session created
- **Daemon unavailable**: logged, retry on next interval
- **Session creation fails** (e.g., max concurrent): logged, retry on next change

## State

Previous outputs are stored as plain files in `$PHYLACTERY_HOME/poll/<name>.last`. This directory is gitignored -- it's transient operational state, not knowledge.

Deleting a `.last` file re-establishes a baseline on the next poll (no false-positive session).

## Composing with MCP

Any MCP server configured in `config.toml` becomes a pollable data source via `phyl-tool-mcp --call`:

```toml
[[poll]]
name = "slack-mentions"
command = "phyl-tool-mcp"
args = ["--call", "slack", "get_mentions", "{}"]
interval = 180
prompt = "New Slack mentions."
```

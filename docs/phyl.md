# phyl -- CLI Client

The command-line interface for interacting with your agent. Thin wrapper over HTTP to the daemon's Unix socket, plus local setup commands that don't need the daemon running.

## Session Commands

These require a running daemon (`phylactd`).

### Start a session

```sh
phyl session "Check my email and summarize anything important"
```

In foreground mode (default), this tails the session log with formatted output until the session ends. Use `-d` for detached mode, which prints the session UUID and returns immediately.

```sh
phyl session -d "Run the weekly report"
# → Session a3f7b2c1-...
```

### List sessions

```sh
phyl ls
```

Displays a table with session ID, status, creation time, and truncated summary.

### Session details

```sh
phyl status <id>
```

Shows prompt, status, timestamps, and recent log entries (formatted by type).

### Talk to a running session

```sh
phyl say <id> "Actually, also check Signal messages"
```

Injects a user message into the session's FIFO. The running session picks it up on its next loop iteration.

### Tail a session log

```sh
phyl log <id>
```

For finished sessions, dumps the full log. For running sessions, follows with polling until the session completes.

### Kill a session

```sh
phyl stop <id>
```

Sends SIGTERM, then SIGKILL if needed.

### Watch all sessions

```sh
phyl watch
```

Connects to the daemon's SSE feed and streams attention-worthy events (questions, completions, errors) across all sessions. When a question arrives, prompts for input inline and sends the answer back.

```
[3a7f] QUESTION: Found 3 new emails. Summarize them? [yes/no]
> 3a7f yes
[3a7f] Summarizing...
[91b2] Done: "Updated project notes in knowledge base"
```

Works over SSH. No TUI framework needed.

## Daemon & Service Commands

### Start the daemon

```sh
phyl start          # foreground (exec)
phyl start -d       # background (prints PID)
phyl start --all    # all services in foreground (no systemd needed)
```

`--all` starts the daemon first, waits for the socket, then starts the poller, listener, and bridge based on what's configured. Ctrl-C sends SIGTERM to all children.

## Setup Commands

### Initialize

```sh
phyl init                    # default: ~/.local/share/phylactery (Linux)
                             #          ~/Library/Application Support/phylactery (macOS)
phyl init /path/to/home      # custom location
```

Creates the agent's home directory with:
- `config.toml` (with sensible defaults and commented examples)
- `secrets.env` (empty, mode 600, gitignored)
- `LAW.md`, `JOB.md`, `SOUL.md` (identity files)
- `knowledge/` (with INDEX.md and subdirectories)
- `sessions/` (gitignored)
- `poll/` (gitignored)
- A git repo with signing disabled and a local `phylactery` identity

### Service setup

On **Linux** (systemd):

```sh
phyl setup systemd           # generate, install, enable, start units
```

Systemd units include dependency ordering, restart policies, and `EnvironmentFile` for secrets.

On **macOS** (launchd):

```sh
phyl setup launchd           # generate, install, load launch agents
```

Launchd plists are written to `~/Library/LaunchAgents/` with `KeepAlive` and environment variables from `secrets.env`.

On either platform:

```sh
phyl setup services          # auto-detect platform, install appropriate service defs
phyl setup status            # check health of everything
phyl setup migrate-xdg       # move ~/.phylactery to platform-appropriate data dir
```

## Configuration Commands

```sh
phyl config show                        # pretty-print (secrets masked)
phyl config validate                    # check for errors
phyl config edit                        # open in $EDITOR, validate on save
phyl config add mcp my-server           # append template section
phyl config add poll my-check           # append poll rule template
phyl config add hook my-webhook         # append webhook template
phyl config add-secret API_KEY sk-xxx   # add to secrets.env
phyl config list-secrets                # show keys (values masked)
phyl config remove-secret OLD_KEY       # remove from secrets.env
```

## Socket Resolution

The CLI resolves the daemon socket path from `config.toml` in `$PHYLACTERY_HOME`. Default resolution order:

1. `$XDG_RUNTIME_DIR/phylactery.sock` (Linux, if set)
2. `$TMPDIR/phylactery.sock` (macOS, per-user temp directory)
3. `/tmp/phylactery.sock` (fallback)

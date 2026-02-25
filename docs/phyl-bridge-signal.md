# phyl-bridge-signal -- Signal Messenger Bridge

Two-way Signal Messenger interface for your agent. The agent messages you when it needs something. You message the agent to give it tasks. Everything else runs autonomously.

## What It Does

**Outbound** (agent to human):
- Connects to the daemon's SSE feed (`GET /feed`)
- Forwards questions, completions, and errors as Signal messages to the configured owner

**Inbound** (human to agent):
- Polls for incoming Signal messages
- Routes replies to pending questions
- Creates new sessions from free-form messages

## Example Interaction

```
Agent sends:
  [Session 3a7f] Found 3 new emails. Summarize them?
  Reply: 1) yes  2) no  3) edit draft

You reply:
  1

Agent continues, summarizes emails, reports back:
  [Session 3a7f] Done: Summarized 3 emails, updated contacts/bob.md
```

Start a new task by just sending a message:

```
You send:
  Check if the server is healthy

Agent replies:
  Started session a1b2c3: "Check if the server is healthy"

Later:
  [Session a1b2] Done: Server is healthy. All endpoints responding < 200ms.
```

## Configuration

```toml
[bridge.signal]
phone = "+1234567890"       # Agent's registered Signal number
owner = "+0987654321"       # Your number (only accept messages from this)
signal_cli = "signal-cli"   # Path to signal-cli binary (default: "signal-cli")
```

## Requirements

- [signal-cli](https://github.com/AsamK/signal-cli) installed and registered with the agent's phone number
- The agent's Signal number must be able to send/receive messages

## Security

The bridge only accepts messages from the configured `owner` number. All other messages are silently ignored.

## How It Works

1. **Startup**: verify `signal-cli` is available (version check)
2. **Feed connection**: connect to daemon's `GET /feed` SSE endpoint via Unix socket
3. **Outbound loop**: parse SSE frames, extract `LogEntry` events, send Signal messages for questions/done/errors
4. **Inbound loop**: poll `signal-cli receive` every 2 seconds, parse JSON envelope format
5. **Reply routing**: match numeric replies to pending question options, forward via `POST /sessions/:id/events`
6. **New sessions**: when no questions are pending, treat inbound messages as new session prompts

## State Management

- Pending questions tracked in a FIFO queue (capped at 50)
- Questions include session ID and option list for numeric reply matching
- Automatic reconnection to daemon feed on connection loss (5-second delay)
- Graceful shutdown on Ctrl-C

## Writing Other Bridges

The bridge protocol is simple enough that adding new transports is straightforward. A bridge just needs to:

1. Connect to `GET /feed` (SSE)
2. Present events to a human
3. Collect responses
4. Post them back via `POST /sessions/:id/events`

A Matrix bridge, Telegram bot, or email bridge could each be a small script in any language.

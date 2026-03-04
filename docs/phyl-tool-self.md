# phyl-tool-self

Server-mode tool that gives the agent self-direction capabilities. The agent can create new sessions, schedule future work, and end the current session with a planned wake-up.

## Tools

### spawn_session

Create a new session immediately or schedule one for a future time.

**Parameters:**
- `prompt` (required) -- task description for the new session
- `at` (optional) -- when to start: ISO 8601 datetime (e.g. `2026-03-04T10:00:00Z`) or relative interval (`30s`, `5m`, `2h`, `1d`, `1w`). Omit for immediate.

Without `at`, the session is created immediately via the daemon API. With `at`, a schedule entry is written to `$PHYLACTERY_HOME/schedule/` for `phyl-sched` to fire later.

### sleep_until

End the current session and schedule a new one for later. Useful for deferring work (e.g., "check back in an hour").

**Parameters:**
- `prompt` (required) -- task description for the wake-up session
- `at` (required) -- when to wake up: ISO 8601 datetime or relative interval

Writes a schedule entry, then signals `end_session` to the session runner. The session ends normally (with SOUL.md reflection), and `phyl-sched` fires the wake-up later.

### list_scheduled

List all pending scheduled entries sorted by time.

**Parameters:** none

Returns a JSON array of schedule entries.

### cancel_scheduled

Cancel a pending scheduled entry by its UUID.

**Parameters:**
- `id` (required) -- UUID of the entry to cancel

## Schedule Entries

Entries are JSON files in `$PHYLACTERY_HOME/schedule/`, named `{uuid}.json`:

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "prompt": "Check the deployment status",
  "at": "2026-03-04T10:00:00Z",
  "created_by": "session-uuid",
  "created_at": "2026-03-04T09:55:00Z"
}
```

Writes are atomic (`.tmp` then rename) to prevent partial reads by `phyl-sched`.

## Protocol

Server mode: NDJSON on stdin/stdout. Follows the standard `ServerRequest`/`ServerResponse` protocol. See [Protocols](protocols.md).

## Discovery

```bash
phyl-tool-self --spec    # prints 4 ToolSpec entries
phyl-tool-self --serve   # starts NDJSON server mode
```

## Environment

- `PHYLACTERY_HOME` or default home path -- used to locate `config.toml` (for daemon socket) and `schedule/`
- `PHYLACTERY_SESSION_ID` -- optional, recorded as `created_by` in schedule entries

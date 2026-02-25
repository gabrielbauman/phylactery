# phyl-tool-session -- Human Interaction & Session Control

A server-mode tool that provides two capabilities: asking humans questions and ending sessions. It runs for the lifetime of a session, communicating via NDJSON on stdin/stdout.

## Tool Specs

### ask_human

Ask the human operator a question and wait for their response.

- **Parameters**:
  - `question` (string, required) -- the question to ask
  - `options` (array of strings, optional) -- suggested answers
  - `context` (string, optional) -- additional context for the human
- **Returns**: the human's answer
- **Behavior**: blocks until a human responds (via bridge, CLI, or FIFO injection)

### done

Signal that the session's task is complete.

- **Parameters**: `summary` (string, required) -- summary of what was accomplished
- **Returns**: the summary text
- **Signal**: `end_session` -- tells the session runner to finalize

## How ask_human Works

This is the most interesting flow in the system:

1. The model calls `ask_human` with a question
2. The session runner writes a `question` event to `log.jsonl` (with a unique question ID)
3. The daemon picks up the question from the log and emits it on the SSE feed
4. A bridge (Signal, `phyl watch`, etc.) presents the question to a human
5. The human answers
6. The bridge posts the answer via `POST /sessions/:id/events` with the `question_id`
7. The answer arrives on the session's FIFO
8. The session runner forwards it to `phyl-tool-session` via NDJSON stdin
9. The tool returns the answer as a tool result
10. The model continues

The session runner is the sole FIFO reader. It routes events: user messages go into history, answer events go to this tool. This avoids race conditions from multiple readers on a single pipe.

### Timeout

If no answer arrives within the configured timeout (default 30 minutes), the runner sends a timeout signal and the tool returns an error indicating the human didn't respond.

## Protocol

NDJSON on stdin/stdout. Each line is a JSON object.

**Request (from runner):**
```json
{"id":"tc_1","name":"ask_human","arguments":{"question":"Send this email?","options":["yes","no"]}}
```

**Answer forwarded (from runner):**
```json
{"id":"tc_1","answer":"yes"}
```

**Response (to runner):**
```json
{"id":"tc_1","output":"Human answered: yes"}
```

**Done signal:**
```json
{"id":"tc_2","output":"Session complete.","signal":"end_session"}
```

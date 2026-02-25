# phyl-tool-bash -- Shell Command Execution

A one-shot tool that executes shell commands. Each invocation spawns a fresh process, runs the command, and returns the output.

## Tool Spec

```json
{
  "name": "bash",
  "description": "Execute a shell command and return its output",
  "mode": "oneshot",
  "parameters": {
    "type": "object",
    "properties": {
      "command": { "type": "string", "description": "The command to run" }
    },
    "required": ["command"]
  }
}
```

## Usage

```sh
# Discovery
phyl-tool-bash --spec

# Invocation
echo '{"name":"bash","arguments":{"command":"echo hello"}}' | phyl-tool-bash
# → {"output":"hello\n"}
```

## Behavior

- **Working directory**: `$PHYLACTERY_SESSION_DIR/scratch/` (created if needed)
- **Output**: stdout and stderr are combined
- **Timeout**: `$PHYLACTERY_TOOL_TIMEOUT` environment variable, default 120 seconds
- **On timeout**: process is killed, error returned
- **On non-zero exit**: error returned with exit code

## Sandbox

The tool declares a sandbox spec for the session runner:

| Type | Paths |
|------|-------|
| Read-write | `$PHYLACTERY_SESSION_DIR/scratch/`, `/tmp` |
| Read-only | `/usr`, `/lib`, `/bin`, `/etc` |
| Network | Enabled |
| CPU limit | 120 seconds |
| File size limit | 100 MB |
| Process limit | 64 |
| File descriptor limit | 256 |

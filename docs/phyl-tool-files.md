# phyl-tool-files -- File Operations

A one-shot tool providing `read_file`, `write_file`, and `search_files` operations. One executable, three tools.

## Tool Specs

### read_file

Read the contents of a file.

- **Parameters**: `path` (string, absolute or relative to scratch directory)
- **Returns**: file contents or error

### write_file

Write content to a file, creating parent directories as needed.

- **Parameters**: `path` (string), `content` (string)
- **Returns**: bytes written + commit status (if under `knowledge/`)
- **Auto-commit**: if the target is under `$PHYLACTERY_HOME/knowledge/` and `git.auto_commit` is enabled, the file is automatically committed with a descriptive message

### search_files

Search for a substring across files.

- **Parameters**: `pattern` (string), `path` (optional, defaults to scratch directory)
- **Returns**: up to 200 matching lines in `file:line_number:content` format
- **Skips**: hidden files, `node_modules/`, `target/` directories, binary files

## Path Resolution

Paths support environment variable expansion (`$VAR` and `${VAR}` syntax):

```
$PHYLACTERY_HOME/knowledge/contacts/bob.md
$PHYLACTERY_SESSION_DIR/scratch/output.txt
```

- Absolute paths are used as-is (after expansion)
- Relative paths are resolved against `$PHYLACTERY_SESSION_DIR/scratch/`

## Auto-Commit

When `write_file` targets a path under `knowledge/`, and `git.auto_commit` is enabled in `config.toml`:

1. Acquire exclusive `flock` on `$PHYLACTERY_HOME/.git.lock`
2. Run `git add <file>` + `git commit -m "knowledge: update <path>"`
3. Report commit status in tool output
4. Gracefully handle "nothing to commit" (no-op)

This serializes with SOUL.md commits and other concurrent sessions.

## Sandbox

| Type | Paths |
|------|-------|
| Read-write | `$PHYLACTERY_SESSION_DIR/scratch/`, `$PHYLACTERY_HOME/knowledge/`, `$PHYLACTERY_HOME/.git*` |
| Read-only | `$PHYLACTERY_HOME/`, `/usr`, `/lib`, `/bin`, `/etc` |
| Network | Disabled |
| CPU limit | 30 seconds |

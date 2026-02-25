# Knowledge Base

The agent's long-term memory. Markdown files under `$PHYLACTERY_HOME/knowledge/`, tracked by git.

## Structure

Any session can read from and write to the knowledge base. The structure is agent-managed but seeded with:

```
knowledge/
  INDEX.md          # Overview, maintained by the agent
  contacts/         # People the agent knows about
  projects/         # Project notes and context
  preferences/      # User preferences and patterns
  journal/          # Daily notes and observations
```

The agent creates files and subdirectories as needed. This isn't a rigid schema -- it's a starting point that the agent evolves through use.

## Access

At session startup, `phyl-run` includes:

- The contents of `knowledge/INDEX.md` in the system prompt
- A file tree listing of all knowledge files (paths only, not content)

The agent uses `read_file` and `search_files` to access content on demand, keeping context window usage minimal.

## Writing

When the agent writes a file under `knowledge/` using `write_file`, and `git.auto_commit` is enabled (the default):

1. The file is written to disk
2. An exclusive `flock` is acquired on `$PHYLACTERY_HOME/.git.lock`
3. `git add <file>` + `git commit -m "knowledge: update <path>"`
4. The lock is released

This serializes with SOUL.md commits and writes from other concurrent sessions. No merge conflicts, no lost updates.

## Searching

The `search_files` tool supports searching the knowledge base:

```
search_files(pattern="project deadline", path="$PHYLACTERY_HOME/knowledge/")
```

This performs a recursive substring search, returning matching lines with file paths and line numbers.

## Git History

Every knowledge update is a git commit. The full history of the agent's learning is in `git log`:

```sh
cd ~/.local/share/phylactery
git log --oneline knowledge/
```

This is the agent's institutional memory -- you can review what it learned, when, and why.

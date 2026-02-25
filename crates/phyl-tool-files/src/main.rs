use phyl_core::{Config, SandboxSpec, ToolInput, ToolMode, ToolOutput, ToolSpec};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

fn tool_specs() -> Vec<ToolSpec> {
    let sandbox = Some(SandboxSpec {
        paths_rw: vec![
            "$PHYLACTERY_SESSION_DIR/scratch/".to_string(),
            "$PHYLACTERY_HOME/knowledge/".to_string(),
            "$PHYLACTERY_HOME/.git/".to_string(),
            "$PHYLACTERY_HOME/.git.lock".to_string(),
        ],
        paths_ro: vec![
            "$PHYLACTERY_HOME/".to_string(),
            "/usr".to_string(),
            "/lib".to_string(),
            "/bin".to_string(),
            "/etc".to_string(),
        ],
        net: false,
        max_cpu_seconds: Some(30),
        max_file_bytes: None,
        max_procs: None,
        max_fds: None,
    });

    vec![
        ToolSpec {
            name: "read_file".to_string(),
            description: "Read the contents of a file and return them".to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (absolute or relative to session scratch)"
                    }
                },
                "required": ["path"]
            }),
            sandbox: sandbox.clone(),
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "Write content to a file, creating it if it doesn't exist".to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (absolute or relative to session scratch)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
            sandbox: sandbox.clone(),
        },
        ToolSpec {
            name: "search_files".to_string(),
            description: "Search for a text pattern in files under a directory, returning matching lines. \
                Can search session scratch, knowledge base, or any readable path."
                .to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Text pattern to search for (substring match)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (absolute or relative to session scratch). Defaults to session scratch. Use $PHYLACTERY_HOME/knowledge/ to search the knowledge base."
                    }
                },
                "required": ["pattern"]
            }),
            sandbox,
        },
    ]
}

/// Resolve a path: expand `$PHYLACTERY_HOME` and `$PHYLACTERY_SESSION_DIR`
/// references, then resolve relative paths against $PHYLACTERY_SESSION_DIR/scratch/.
fn resolve_path(path: &str) -> PathBuf {
    // Expand environment variable references in the path.
    let expanded = expand_env_vars(path);
    let p = Path::new(&expanded);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        let base = std::env::var("PHYLACTERY_SESSION_DIR")
            .map(|d| PathBuf::from(d).join("scratch"))
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        base.join(p)
    }
}

/// Validate that a resolved path is within an allowed directory.
///
/// Allowed directories for reads:
///   - $PHYLACTERY_SESSION_DIR/scratch/
///   - $PHYLACTERY_HOME/ (entire home)
///   - /usr, /lib, /bin, /etc (system read-only paths from sandbox spec)
///
/// Allowed directories for writes:
///   - $PHYLACTERY_SESSION_DIR/scratch/
///   - $PHYLACTERY_HOME/knowledge/
fn validate_path(resolved: &Path, writable: bool) -> Result<(), String> {
    // Canonicalize what we can — for new files, canonicalize the parent.
    let canonical = if resolved.exists() {
        resolved.canonicalize()
            .map_err(|e| format!("Cannot resolve path {}: {e}", resolved.display()))?
    } else if let Some(parent) = resolved.parent() {
        if parent.exists() {
            let canon_parent = parent.canonicalize()
                .map_err(|e| format!("Cannot resolve parent {}: {e}", parent.display()))?;
            canon_parent.join(resolved.file_name().unwrap_or_default())
        } else {
            resolved.to_path_buf()
        }
    } else {
        resolved.to_path_buf()
    };

    let scratch = std::env::var("PHYLACTERY_SESSION_DIR")
        .map(|d| PathBuf::from(d).join("scratch"))
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)));

    let home = std::env::var("PHYLACTERY_HOME")
        .map(PathBuf::from)
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)));

    // Scratch is always allowed for both reads and writes.
    if let Some(ref scratch) = scratch {
        if canonical.starts_with(scratch) {
            return Ok(());
        }
    }

    if writable {
        // Writes only allowed to scratch and knowledge/.
        if let Some(ref home) = home {
            let knowledge = home.join("knowledge");
            if canonical.starts_with(&knowledge) {
                return Ok(());
            }
        }
        Err(format!(
            "Write denied: {} is outside allowed directories (scratch/, knowledge/)",
            resolved.display()
        ))
    } else {
        // Reads allowed from home, plus system paths.
        if let Some(ref home) = home {
            if canonical.starts_with(home) {
                return Ok(());
            }
        }
        let system_paths = ["/usr", "/lib", "/bin", "/etc"];
        for sys in &system_paths {
            if canonical.starts_with(sys) {
                return Ok(());
            }
        }
        Err(format!(
            "Read denied: {} is outside allowed directories",
            resolved.display()
        ))
    }
}

/// Expand `$VAR` or `${VAR}` references in a string from the process environment.
fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            let mut var_name = String::new();
            let braced = chars.peek() == Some(&'{');
            if braced {
                chars.next(); // consume '{'
                while let Some(&ch) = chars.peek() {
                    if ch == '}' {
                        chars.next();
                        break;
                    }
                    var_name.push(ch);
                    chars.next();
                }
            } else {
                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' {
                        var_name.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            if let Ok(val) = std::env::var(&var_name) {
                result.push_str(&val);
            } else {
                // Leave unresolved vars as-is.
                result.push('$');
                if braced {
                    result.push('{');
                    result.push_str(&var_name);
                    result.push('}');
                } else {
                    result.push_str(&var_name);
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

fn handle_read_file(arguments: &serde_json::Value) -> ToolOutput {
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ToolOutput {
                output: None,
                error: Some("Missing required argument: path".to_string()),
            };
        }
    };

    let resolved = resolve_path(path);
    if let Err(e) = validate_path(&resolved, false) {
        return ToolOutput {
            output: None,
            error: Some(e),
        };
    }
    match std::fs::read_to_string(&resolved) {
        Ok(contents) => ToolOutput {
            output: Some(contents),
            error: None,
        },
        Err(e) => ToolOutput {
            output: None,
            error: Some(format!("Failed to read {}: {e}", resolved.display())),
        },
    }
}

fn handle_write_file(arguments: &serde_json::Value) -> ToolOutput {
    let path = match arguments.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ToolOutput {
                output: None,
                error: Some("Missing required argument: path".to_string()),
            };
        }
    };

    let content = match arguments.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolOutput {
                output: None,
                error: Some("Missing required argument: content".to_string()),
            };
        }
    };

    let resolved = resolve_path(path);
    if let Err(e) = validate_path(&resolved, true) {
        return ToolOutput {
            output: None,
            error: Some(e),
        };
    }

    // Create parent directories if they don't exist.
    if let Some(parent) = resolved.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ToolOutput {
                output: None,
                error: Some(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                )),
            };
        }
    }

    match std::fs::write(&resolved, content) {
        Ok(()) => {
            let mut msg = format!("Wrote {} bytes to {}", content.len(), resolved.display());

            // Auto-commit if write is under knowledge/.
            if let Some(home) = get_home_dir() {
                let knowledge_dir = home.join("knowledge");
                if resolved.starts_with(&knowledge_dir) {
                    match auto_commit_knowledge(&home, &resolved) {
                        Ok(()) => {
                            msg.push_str(" (auto-committed to git)");
                        }
                        Err(e) => {
                            eprintln!("phyl-tool-files: auto-commit failed: {e}");
                            msg.push_str(&format!(" (auto-commit failed: {e})"));
                        }
                    }
                }
            }

            ToolOutput {
                output: Some(msg),
                error: None,
            }
        }
        Err(e) => ToolOutput {
            output: None,
            error: Some(format!("Failed to write {}: {e}", resolved.display())),
        },
    }
}

/// Returns $PHYLACTERY_HOME if set.
fn get_home_dir() -> Option<PathBuf> {
    std::env::var("PHYLACTERY_HOME").ok().map(PathBuf::from)
}

/// Read the agent config from $PHYLACTERY_HOME/config.toml.
fn read_config(home: &Path) -> Config {
    let config_path = home.join("config.toml");
    match std::fs::read_to_string(&config_path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// Auto-commit a knowledge file write to git.
///
/// Acquires `.git.lock` via flock to serialize with other git operations
/// (SOUL.md finalization, other sessions). Commits with a descriptive message.
fn auto_commit_knowledge(home: &Path, file_path: &Path) -> Result<(), String> {
    let config = read_config(home);
    if !config.git.auto_commit {
        return Ok(());
    }

    // Compute the relative path from home for the commit message and git add.
    let rel_path = file_path
        .strip_prefix(home)
        .unwrap_or(file_path);

    let git_lock_path = home.join(".git.lock");

    // Acquire exclusive git lock.
    let lock_fd = acquire_flock(&git_lock_path)?;

    // git add <file>
    let add_output = Command::new("git")
        .args(["add", &rel_path.to_string_lossy()])
        .current_dir(home)
        .output()
        .map_err(|e| format!("failed to run git add: {e}"))?;

    if !add_output.status.success() {
        release_flock(lock_fd);
        let stderr = String::from_utf8_lossy(&add_output.stderr);
        return Err(format!("git add failed: {stderr}"));
    }

    // git commit
    let commit_msg = format!("knowledge: update {}", rel_path.display());

    let commit_output = Command::new("git")
        .args(["commit", "-m", &commit_msg])
        .current_dir(home)
        .output()
        .map_err(|e| format!("failed to run git commit: {e}"))?;

    release_flock(lock_fd);

    if !commit_output.status.success() {
        let stderr = String::from_utf8_lossy(&commit_output.stderr);
        // "nothing to commit" is not a real error.
        if stderr.contains("nothing to commit") || stderr.contains("no changes added") {
            return Ok(());
        }
        return Err(format!("git commit failed: {stderr}"));
    }

    eprintln!("phyl-tool-files: committed {}", rel_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// File locking (flock) — same pattern as phyl-run
// ---------------------------------------------------------------------------

fn acquire_flock(path: &Path) -> Result<i32, String> {
    use std::ffi::CString;
    use std::fs::OpenOptions;

    // Create the lock file if it doesn't exist.
    let _ = OpenOptions::new().create(true).append(true).open(path);

    let c_path = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|e| format!("invalid lock path: {e}"))?;

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o600) };
    if fd < 0 {
        return Err(format!(
            "failed to open lock file {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }

    let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if ret != 0 {
        unsafe { libc::close(fd); }
        return Err(format!(
            "flock failed on {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }

    Ok(fd)
}

fn release_flock(fd: i32) {
    unsafe {
        libc::flock(fd, libc::LOCK_UN);
        libc::close(fd);
    }
}

fn handle_search_files(arguments: &serde_json::Value) -> ToolOutput {
    let pattern = match arguments.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ToolOutput {
                output: None,
                error: Some("Missing required argument: pattern".to_string()),
            };
        }
    };

    let search_dir = arguments
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| resolve_path(p))
        .unwrap_or_else(|| resolve_path("."));

    if let Err(e) = validate_path(&search_dir, false) {
        return ToolOutput {
            output: None,
            error: Some(e),
        };
    }

    if !search_dir.is_dir() {
        return ToolOutput {
            output: None,
            error: Some(format!("{} is not a directory", search_dir.display())),
        };
    }

    let mut matches = Vec::new();
    const MAX_MATCHES: usize = 200;

    if let Err(e) = search_recursive(&search_dir, pattern, &mut matches, MAX_MATCHES) {
        return ToolOutput {
            output: None,
            error: Some(format!("Search error: {e}")),
        };
    }

    if matches.is_empty() {
        ToolOutput {
            output: Some("No matches found.".to_string()),
            error: None,
        }
    } else {
        let truncated = matches.len() >= MAX_MATCHES;
        let mut result = matches.join("\n");
        if truncated {
            result.push_str(&format!("\n... (truncated at {MAX_MATCHES} matches)"));
        }
        ToolOutput {
            output: Some(result),
            error: None,
        }
    }
}

/// Recursively search files in a directory for a pattern, collecting matching lines.
fn search_recursive(
    dir: &Path,
    pattern: &str,
    matches: &mut Vec<String>,
    max_matches: usize,
) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;

    for entry in entries {
        if matches.len() >= max_matches {
            return Ok(());
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();

        // Skip hidden files/dirs and common binary/large dirs.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
        }

        if path.is_dir() {
            search_recursive(&path, pattern, matches, max_matches)?;
        } else if path.is_file() {
            // Try to read as text; skip binary files.
            if let Ok(contents) = std::fs::read_to_string(&path) {
                for (line_num, line) in contents.lines().enumerate() {
                    if matches.len() >= max_matches {
                        return Ok(());
                    }
                    if line.contains(pattern) {
                        matches.push(format!("{}:{}: {}", path.display(), line_num + 1, line));
                    }
                }
            }
        }
    }

    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--spec") {
        let specs = tool_specs();
        println!(
            "{}",
            serde_json::to_string_pretty(&specs).expect("failed to serialize specs")
        );
        return;
    }

    // Read ToolInput from stdin.
    let mut input_str = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input_str) {
        let err = ToolOutput {
            output: None,
            error: Some(format!("Failed to read stdin: {e}")),
        };
        println!("{}", serde_json::to_string(&err).unwrap());
        std::process::exit(1);
    }

    let input: ToolInput = match serde_json::from_str(&input_str) {
        Ok(v) => v,
        Err(e) => {
            let err = ToolOutput {
                output: None,
                error: Some(format!("Invalid JSON input: {e}")),
            };
            println!("{}", serde_json::to_string(&err).unwrap());
            std::process::exit(1);
        }
    };

    let result = match input.name.as_str() {
        "read_file" => handle_read_file(&input.arguments),
        "write_file" => handle_write_file(&input.arguments),
        "search_files" => handle_search_files(&input.arguments),
        other => ToolOutput {
            output: None,
            error: Some(format!("Unknown tool: {other}")),
        },
    };

    println!("{}", serde_json::to_string(&result).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_env_vars_simple() {
        unsafe { std::env::set_var("PHYL_TEST_VAR", "/test/path"); }
        let result = expand_env_vars("$PHYL_TEST_VAR/knowledge/");
        assert_eq!(result, "/test/path/knowledge/");
        unsafe { std::env::remove_var("PHYL_TEST_VAR"); }
    }

    #[test]
    fn test_expand_env_vars_braced() {
        unsafe { std::env::set_var("PHYL_TEST_VAR2", "/braced"); }
        let result = expand_env_vars("${PHYL_TEST_VAR2}/sub");
        assert_eq!(result, "/braced/sub");
        unsafe { std::env::remove_var("PHYL_TEST_VAR2"); }
    }

    #[test]
    fn test_expand_env_vars_unset() {
        let result = expand_env_vars("$NONEXISTENT_PHYL_VAR/path");
        assert_eq!(result, "$NONEXISTENT_PHYL_VAR/path");
    }

    #[test]
    fn test_expand_env_vars_no_vars() {
        let result = expand_env_vars("/absolute/path/file.md");
        assert_eq!(result, "/absolute/path/file.md");
    }

    #[test]
    fn test_resolve_path_absolute() {
        let result = resolve_path("/tmp/test.md");
        assert_eq!(result, PathBuf::from("/tmp/test.md"));
    }

    #[test]
    fn test_resolve_path_with_env_expansion() {
        unsafe { std::env::set_var("PHYLACTERY_HOME", "/test/home"); }
        let result = resolve_path("$PHYLACTERY_HOME/knowledge/contacts/alice.md");
        assert_eq!(
            result,
            PathBuf::from("/test/home/knowledge/contacts/alice.md")
        );
    }

    #[test]
    fn test_is_under_knowledge() {
        let home = PathBuf::from("/test/home");
        let knowledge_dir = home.join("knowledge");
        let file = PathBuf::from("/test/home/knowledge/contacts/alice.md");
        assert!(file.starts_with(&knowledge_dir));

        let scratch_file = PathBuf::from("/test/home/sessions/abc/scratch/notes.md");
        assert!(!scratch_file.starts_with(&knowledge_dir));
    }

    #[test]
    fn test_handle_read_file_missing() {
        let result = handle_read_file(&serde_json::json!({"path": "/nonexistent/file.md"}));
        assert!(result.error.is_some());
        assert!(result.output.is_none());
    }

    #[test]
    fn test_handle_read_file_no_path() {
        let result = handle_read_file(&serde_json::json!({}));
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Missing required argument"));
    }

    #[test]
    fn test_handle_write_file_success() {
        let tmp = std::env::temp_dir().join("phyl_test_write");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let file_path = tmp.join("test.txt");

        let result = handle_write_file(&serde_json::json!({
            "path": file_path.to_string_lossy(),
            "content": "hello world"
        }));
        assert!(result.error.is_none());
        assert!(result.output.unwrap().contains("Wrote 11 bytes"));
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "hello world");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_handle_write_file_no_content() {
        let result = handle_write_file(&serde_json::json!({"path": "/tmp/test.txt"}));
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Missing required argument: content"));
    }

    #[test]
    fn test_handle_search_files_no_pattern() {
        let result = handle_search_files(&serde_json::json!({}));
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Missing required argument: pattern"));
    }

    #[test]
    fn test_handle_search_files_in_dir() {
        let tmp = std::env::temp_dir().join("phyl_test_search");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "hello world\ngoodbye world\n").unwrap();
        std::fs::write(tmp.join("b.txt"), "no match here\n").unwrap();

        let result = handle_search_files(&serde_json::json!({
            "pattern": "hello",
            "path": tmp.to_string_lossy()
        }));
        assert!(result.error.is_none());
        let output = result.output.unwrap();
        assert!(output.contains("hello world"));
        assert!(!output.contains("no match"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_search_recursive_skips_hidden() {
        let tmp = std::env::temp_dir().join("phyl_test_search_hidden");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".hidden")).unwrap();
        std::fs::write(tmp.join(".hidden/secret.txt"), "findme\n").unwrap();
        std::fs::write(tmp.join("visible.txt"), "findme\n").unwrap();

        let mut matches = Vec::new();
        search_recursive(&tmp, "findme", &mut matches, 100).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].contains("visible.txt"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_tool_specs_count() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].name, "read_file");
        assert_eq!(specs[1].name, "write_file");
        assert_eq!(specs[2].name, "search_files");
    }

    #[test]
    fn test_tool_specs_modes() {
        let specs = tool_specs();
        for spec in &specs {
            assert_eq!(spec.mode, ToolMode::Oneshot);
        }
    }

    #[test]
    fn test_tool_specs_sandbox() {
        let specs = tool_specs();
        for spec in &specs {
            assert!(spec.sandbox.is_some());
            let sandbox = spec.sandbox.as_ref().unwrap();
            assert!(sandbox.paths_rw.iter().any(|p| p.contains("knowledge")));
            assert!(sandbox.paths_rw.iter().any(|p| p.contains(".git")));
        }
    }

    #[test]
    fn test_read_config_missing() {
        let config = read_config(Path::new("/nonexistent"));
        assert!(config.git.auto_commit); // Defaults to true.
    }
}

use phyl_core::{SandboxSpec, ToolInput, ToolMode, ToolOutput, ToolSpec};
use std::io::{self, Read};
use std::path::Path;

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
            description: "Search for a pattern in files under a directory, returning matching lines"
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
                        "description": "Directory to search in (absolute or relative to session scratch). Defaults to current directory."
                    }
                },
                "required": ["pattern"]
            }),
            sandbox,
        },
    ]
}

/// Resolve a path: if relative, resolve against $PHYLACTERY_SESSION_DIR/scratch/.
fn resolve_path(path: &str) -> std::path::PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        let base = std::env::var("PHYLACTERY_SESSION_DIR")
            .map(|d| std::path::PathBuf::from(d).join("scratch"))
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        base.join(p)
    }
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
        Ok(()) => ToolOutput {
            output: Some(format!(
                "Wrote {} bytes to {}",
                content.len(),
                resolved.display()
            )),
            error: None,
        },
        Err(e) => ToolOutput {
            output: None,
            error: Some(format!("Failed to write {}: {e}", resolved.display())),
        },
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

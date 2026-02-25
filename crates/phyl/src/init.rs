use phyl_core::home_dir;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `phyl init [path]`.
///
/// Creates the agent home directory with:
/// - A git repo
/// - config.toml (default configuration)
/// - LAW.md (empty rules, human fills in)
/// - JOB.md (empty job description, human fills in)
/// - SOUL.md ("I am new.")
/// - knowledge/ directory structure (contacts/, projects/, preferences/, journal/, INDEX.md)
/// - sessions/.gitignore (ignore everything under sessions/)
pub fn run(path: Option<&str>) -> Result<(), String> {
    let home = match path {
        Some(p) => PathBuf::from(p),
        None => home_dir(),
    };

    if home.join(".git").exists() {
        return Err(format!(
            "{} is already initialized (has .git)",
            home.display()
        ));
    }

    // Create the directory structure
    create_dirs(&home)?;

    // Initialize git repo
    git_init(&home)?;

    // Write seed files
    write_config(&home)?;
    write_file(&home.join("LAW.md"), LAW_SEED)?;
    write_file(&home.join("JOB.md"), JOB_SEED)?;
    write_file(&home.join("SOUL.md"), SOUL_SEED)?;
    write_file(&home.join("knowledge/INDEX.md"), INDEX_SEED)?;
    write_file(&home.join("sessions/.gitignore"), SESSIONS_GITIGNORE)?;

    // Initial git commit
    git_add_and_commit(&home)?;

    eprintln!("Initialized phylactery home at {}", home.display());
    Ok(())
}

fn create_dirs(home: &Path) -> Result<(), String> {
    let dirs = [
        home.to_path_buf(),
        home.join("knowledge"),
        home.join("knowledge/contacts"),
        home.join("knowledge/projects"),
        home.join("knowledge/preferences"),
        home.join("knowledge/journal"),
        home.join("sessions"),
    ];

    for dir in &dirs {
        fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {}", dir.display(), e))?;
    }

    Ok(())
}

fn git_init(home: &Path) -> Result<(), String> {
    run_git(home, &["init"])?;
    // Configure the repo for the agent: disable signing, set default identity
    run_git(home, &["config", "commit.gpgSign", "false"])?;
    run_git(home, &["config", "user.name", "phylactery"])?;
    run_git(home, &["config", "user.email", "phylactery@localhost"])?;
    Ok(())
}

fn git_add_and_commit(home: &Path) -> Result<(), String> {
    run_git(home, &["add", "-A"])?;
    run_git(home, &["commit", "-m", "phyl init: initialize agent home"])
}

fn run_git(home: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(home)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), stderr.trim()));
    }
    Ok(())
}

fn write_config(home: &Path) -> Result<(), String> {
    // Generate config with the default socket path resolved at runtime
    let socket_path = std::env::var("XDG_RUNTIME_DIR")
        .map(|dir| format!("{}/phylactery.sock", dir))
        .unwrap_or_else(|_| "/tmp/phylactery.sock".to_string());

    let config = format!(
        r#"[daemon]
socket = "{socket_path}"

[session]
timeout_minutes = 60
max_concurrent = 4
model = "phyl-model-claude"

[model]
context_window = 200000
compress_at = 0.8

[git]
auto_commit = true
# remote = "origin"          # Uncomment to auto-push after commits

# MCP servers (used by phyl-tool-mcp)
# [[mcp]]
# name = "filesystem"
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user"]
"#
    );

    write_file(&home.join("config.toml"), &config)
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }
    fs::write(path, content).map_err(|e| format!("failed to write {}: {}", path.display(), e))
}

const LAW_SEED: &str = "\
# LAW

These rules are absolute. Obey them unconditionally.

<!-- Add your rules here. The agent cannot override, ignore, or modify these. -->
";

const JOB_SEED: &str = "\
# JOB

<!-- Describe the agent's role and scope here. The agent should refuse
     sessions outside its job description. -->
";

const SOUL_SEED: &str = "I am new.\n";

const INDEX_SEED: &str = "\
# Knowledge Index

This file is maintained by the agent as a table of contents for the knowledge base.

## Structure

- `contacts/` — People the agent knows about
- `projects/` — Project notes and context
- `preferences/` — User preferences and patterns
- `journal/` — Per-session reflections and notes
";

const SESSIONS_GITIGNORE: &str = "\
# Session working directories are not git-tracked.
# They contain logs, FIFOs, scratch files, and PID files.
*
!.gitignore
";

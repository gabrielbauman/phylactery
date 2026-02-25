use anyhow::{Context, bail};
use phyl_core::home_dir;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `phyl init [path]`.
///
/// Creates the agent home directory with:
/// - A git repo
/// - config.toml (default configuration)
/// - secrets.env (empty, chmod 600, gitignored)
/// - LAW.md (empty rules, human fills in)
/// - JOB.md (empty job description, human fills in)
/// - SOUL.md ("I am new.")
/// - knowledge/ directory structure (contacts/, projects/, preferences/, journal/, INDEX.md)
/// - sessions/.gitignore (ignore everything under sessions/)
/// - poll/.gitignore (ignore poll state files)
/// - ~/.config/phylactery/ symlink (XDG config)
pub fn run(path: Option<&str>) -> anyhow::Result<()> {
    let home = match path {
        Some(p) => PathBuf::from(p),
        None => home_dir(),
    };

    if home.join(".git").exists() {
        bail!("{} is already initialized (has .git)", home.display());
    }

    // Create the directory structure
    create_dirs(&home)?;

    // Initialize git repo
    git_init(&home)?;

    // Write seed files
    write_config(&home)?;
    write_secrets_env(&home)?;
    write_file(&home.join("LAW.md"), LAW_SEED)?;
    write_file(&home.join("JOB.md"), JOB_SEED)?;
    write_file(&home.join("SOUL.md"), SOUL_SEED)?;
    write_file(&home.join("knowledge/INDEX.md"), INDEX_SEED)?;
    write_file(&home.join("sessions/.gitignore"), SESSIONS_GITIGNORE)?;
    write_file(&home.join("poll/.gitignore"), POLL_GITIGNORE)?;
    write_file(&home.join(".gitignore"), ROOT_GITIGNORE)?;

    // Initial git commit
    git_add_and_commit(&home)?;

    // Create XDG config symlink
    create_xdg_symlink(&home);

    eprintln!("Initialized phylactery home at {}", home.display());
    eprintln!();
    eprintln!("Next steps:");
    eprintln!("  phyl config edit              # Edit LAW.md, JOB.md, config.toml");
    eprintln!("  phyl config add mcp ...       # Add tool servers");
    eprintln!("  phyl setup systemd            # Install as systemd user services");
    eprintln!("  phyl start                    # Or just start the daemon");
    Ok(())
}

fn create_dirs(home: &Path) -> anyhow::Result<()> {
    let dirs = [
        home.to_path_buf(),
        home.join("knowledge"),
        home.join("knowledge/contacts"),
        home.join("knowledge/projects"),
        home.join("knowledge/preferences"),
        home.join("knowledge/journal"),
        home.join("sessions"),
        home.join("poll"),
    ];

    for dir in &dirs {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }

    Ok(())
}

fn git_init(home: &Path) -> anyhow::Result<()> {
    run_git(home, &["init"])?;
    // Configure the repo for the agent: disable signing, set default identity
    run_git(home, &["config", "commit.gpgSign", "false"])?;
    run_git(home, &["config", "user.name", "phylactery"])?;
    run_git(home, &["config", "user.email", "phylactery@localhost"])?;
    Ok(())
}

fn git_add_and_commit(home: &Path) -> anyhow::Result<()> {
    run_git(home, &["add", "-A"])?;
    run_git(home, &["commit", "-m", "phyl init: initialize agent home"])
}

fn run_git(home: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(home)
        .output()
        .context("failed to run git")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

fn write_config(home: &Path) -> anyhow::Result<()> {
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

# Signal Messenger bridge (used by phyl-bridge-signal)
# [bridge.signal]
# phone = "+1234567890"           # Agent's registered Signal number
# owner = "+0987654321"           # Your Signal number (only accept from this)
# signal_cli = "signal-cli"       # Path to signal-cli binary

# Poll rules (used by phyl-poll)
# [[poll]]
# name = "example"
# command = "echo"
# args = ["hello"]
# interval = 300                  # seconds between polls
# prompt = "Check this output change."

# Incoming event listeners (used by phyl-listen)
# [listen]
# bind = "127.0.0.1:7890"        # Only needed for webhooks

# [[listen.hook]]
# name = "github"
# path = "/hook/github"
# secret = "$GITHUB_WEBHOOK_SECRET"
# prompt = "A GitHub event arrived."

# [[listen.watch]]
# name = "inbox"
# path = "/home/user/agent-inbox"
# events = ["create"]
# prompt = "A new file appeared in the inbox."
"#
    );

    write_file(&home.join("config.toml"), &config)
}

fn write_secrets_env(home: &Path) -> anyhow::Result<()> {
    let path = home.join("secrets.env");
    write_file(&path, SECRETS_SEED)?;

    // chmod 600
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(&path, perms).context("failed to set permissions on secrets.env")?;

    Ok(())
}

fn create_xdg_symlink(home: &Path) {
    // Only create symlink if we're using XDG paths
    let home_str = home.to_string_lossy();
    if !home_str.contains(".local/share") {
        return;
    }

    if let Ok(user_home) = std::env::var("HOME") {
        let config_dir = PathBuf::from(&user_home).join(".config/phylactery");
        if fs::create_dir_all(&config_dir).is_err() {
            return;
        }
        let link = config_dir.join("config.toml");
        let target = home.join("config.toml");
        if !link.exists() {
            let _ = std::os::unix::fs::symlink(&target, &link);
        }
    }
}

fn write_file(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
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

const POLL_GITIGNORE: &str = "\
# Poll state files are not git-tracked.
# They contain the last output of each poll command.
*
!.gitignore
";

const ROOT_GITIGNORE: &str = "\
# Secrets — never commit
secrets.env
";

const SECRETS_SEED: &str = "\
# Secrets for phylactery — do not commit
# Format: KEY=VALUE (one per line)
# Referenced in config.toml as $KEY
";

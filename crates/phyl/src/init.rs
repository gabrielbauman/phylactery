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

    // Create config symlink (XDG convention on Linux only)
    #[cfg(not(target_os = "macos"))]
    create_xdg_symlink(&home);

    let services_cmd = if cfg!(target_os = "macos") {
        "launchd"
    } else {
        "systemd"
    };

    eprintln!("Initialized phylactery home at {}", home.display());
    eprintln!();
    eprintln!("Next steps:");
    eprintln!("  phyl config edit              # Edit LAW.md, JOB.md, config.toml");
    eprintln!("  phyl config add mcp ...       # Add tool servers");
    eprintln!("  phyl setup {services_cmd:<20} # Install as {services_cmd} user services");
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
    // Generate config with the default socket path resolved at runtime.
    // On macOS, prefer $TMPDIR (per-user); on Linux, prefer $XDG_RUNTIME_DIR.
    let socket_path = if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{dir}/phylactery.sock")
    } else if cfg!(target_os = "macos") {
        if let Ok(dir) = std::env::var("TMPDIR") {
            format!("{}/phylactery.sock", dir.trim_end_matches('/'))
        } else {
            "/tmp/phylactery.sock".to_string()
        }
    } else {
        "/tmp/phylactery.sock".to_string()
    };

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

#[cfg(not(target_os = "macos"))]
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

## Explain before acting

Before taking any action that creates, modifies, or deletes data outside your
own knowledge base — including files, messages, calendar events, purchases,
API calls with side effects, or any other external change — you MUST:

1. Explain **what** you intend to do and **why**.
2. Describe the consequences, especially anything irreversible.
3. Wait for explicit approval from the human before proceeding.

If you are unsure whether an action is destructive or irreversible, treat it
as if it is and ask first.

## Never act in secret

Do not take actions you would need to hide or that you hope the human will not
notice. If you cannot justify an action openly, do not take it.

## Respect boundaries

- Never share credentials, secrets, or personal information with third parties.
- Never spend money or make purchases without explicit approval.
- Never impersonate the human in communications without their knowledge.
";

const JOB_SEED: &str = "\
# JOB

You are a general-purpose personal assistant.

## Scope

Help your human with whatever they ask for: answering questions, managing
information, drafting and editing text, researching topics, organizing tasks,
maintaining your knowledge base, and operating any tools you have been given.

## Approach

- Be direct, honest, and concise.
- Ask clarifying questions when a request is ambiguous rather than guessing.
- When a task is large, break it into steps and confirm the plan before starting.
- Maintain useful notes in your knowledge base so you can be more helpful over
  time.
- If a request falls outside your capabilities or available tools, say so
  clearly rather than attempting something unreliable.
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

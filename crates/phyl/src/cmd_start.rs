//! `phyl start [-d]` — launch the phylactd daemon.

use anyhow::{Context, bail};
use std::process::{Command, Stdio};

/// Run the `start` command.
pub fn run(detach: bool) -> anyhow::Result<()> {
    let binary = find_daemon()?;

    if detach {
        // Daemonize: spawn in background, redirect stdio to /dev/null.
        let child = Command::new(&binary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start daemon")?;

        eprintln!("phylactd started (pid {})", child.id());
        Ok(())
    } else {
        // Foreground: exec the daemon (replace this process).
        let err = exec_replace(&binary);
        bail!("failed to exec daemon: {err}");
    }
}

/// Find the phylactd binary.
fn find_daemon() -> anyhow::Result<String> {
    // Check $PATH.
    if let Ok(output) = Command::new("which").arg("phylactd").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(path);
        }
    }

    // Check same directory as current executable.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("phylactd");
        if candidate.exists() {
            return Ok(candidate.to_string_lossy().to_string());
        }
    }

    // Fall back to bare name.
    Ok("phylactd".to_string())
}

/// Start all services in foreground (no systemd).
pub async fn run_all() -> anyhow::Result<()> {
    let home = phyl_core::home_dir();
    if !home.exists() {
        bail!("{} does not exist. Run `phyl init` first.", home.display());
    }

    let config = {
        let config_path = home.join("config.toml");
        let contents =
            std::fs::read_to_string(&config_path).context("failed to read config.toml")?;
        toml::from_str::<phyl_core::Config>(&contents).context("failed to parse config.toml")?
    };

    let mut children = Vec::new();

    // Start daemon first
    let daemon_bin = find_daemon()?;
    eprintln!("Starting phylactd...");
    let daemon = Command::new(&daemon_bin)
        .spawn()
        .context("failed to start daemon")?;
    children.push(("phylactd", daemon));

    // Wait for socket to appear
    let socket = &config.daemon.socket;
    for _ in 0..30 {
        if std::path::Path::new(socket).exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    // Start phyl-poll if configured
    if !config.poll.is_empty()
        && let Some(bin) = find_binary("phyl-poll")
    {
        eprintln!("Starting phyl-poll...");
        let child = Command::new(&bin)
            .spawn()
            .context("failed to start phyl-poll")?;
        children.push(("phyl-poll", child));
    }

    // Start phyl-listen if configured
    if let Some(listen) = &config.listen
        && (!listen.hook.is_empty() || !listen.sse.is_empty() || !listen.watch.is_empty())
        && let Some(bin) = find_binary("phyl-listen")
    {
        eprintln!("Starting phyl-listen...");
        let child = Command::new(&bin)
            .spawn()
            .context("failed to start phyl-listen")?;
        children.push(("phyl-listen", child));
    }

    // Start phyl-bridge-signal if configured
    if let Some(bridge) = &config.bridge
        && bridge.signal.is_some()
        && let Some(bin) = find_binary("phyl-bridge-signal")
    {
        eprintln!("Starting phyl-bridge-signal...");
        let child = Command::new(&bin)
            .spawn()
            .context("failed to start phyl-bridge-signal")?;
        children.push(("phyl-bridge-signal", child));
    }

    eprintln!("All services started. Press Ctrl-C to stop.");

    // Wait for Ctrl-C
    tokio::signal::ctrl_c()
        .await
        .context("signal handler failed")?;

    eprintln!("\nStopping all services...");

    // Send SIGTERM to all children
    for (name, child) in &children {
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        eprintln!("  sent SIGTERM to {name} (pid {})", child.id());
    }

    // Wait for children to exit
    for (name, mut child) in children {
        match child.wait() {
            Ok(status) => {
                eprintln!("  {name} exited: {status}");
            }
            Err(e) => {
                eprintln!("  {name} wait failed: {e}");
            }
        }
    }

    Ok(())
}

fn find_binary(name: &str) -> Option<String> {
    // Check same directory as current executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }

    // Check $PATH
    if let Ok(output) = Command::new("which").arg(name).output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(path);
        }
    }

    None
}

/// Replace the current process with the given binary (Unix exec).
fn exec_replace(binary: &str) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    // This only returns if exec fails.
    Command::new(binary).exec()
}

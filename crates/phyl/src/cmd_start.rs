//! `phyl start [-d]` — launch the phylactd daemon.

use std::process::{Command, Stdio};

/// Run the `start` command.
pub fn run(detach: bool) -> Result<(), String> {
    let binary = find_daemon()?;

    if detach {
        // Daemonize: spawn in background, redirect stdio to /dev/null.
        let child = Command::new(&binary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to start daemon: {e}"))?;

        eprintln!("phylactd started (pid {})", child.id());
        Ok(())
    } else {
        // Foreground: exec the daemon (replace this process).
        let err = exec_replace(&binary);
        Err(format!("failed to exec daemon: {err}"))
    }
}

/// Find the phylactd binary.
fn find_daemon() -> Result<String, String> {
    // Check $PATH.
    if let Ok(output) = Command::new("which").arg("phylactd").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }

    // Check same directory as current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("phylactd");
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().to_string());
            }
        }
    }

    // Fall back to bare name.
    Ok("phylactd".to_string())
}

/// Replace the current process with the given binary (Unix exec).
fn exec_replace(binary: &str) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    // This only returns if exec fails.
    Command::new(binary).exec()
}

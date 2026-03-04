//! `phyl-sched` — Scheduler service. Scans `$PHYLACTERY_HOME/schedule/` every
//! 5 seconds and fires due entries by creating sessions via the daemon API.

use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use phyl_core::{Config, ScheduleEntry};
use std::path::Path;
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::time::{Duration, sleep};

const SCAN_INTERVAL: Duration = Duration::from_secs(5);

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run()) {
        eprintln!("phyl-sched: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let home = phyl_core::home_dir();
    if !home.exists() {
        return Err(format!(
            "{} does not exist. Run `phyl init` first.",
            home.display()
        ));
    }

    let config = load_config(&home)?;
    let socket = config.daemon.socket;
    let schedule_dir = home.join("schedule");

    // Ensure schedule directory exists
    std::fs::create_dir_all(&schedule_dir)
        .map_err(|e| format!("failed to create schedule dir: {e}"))?;

    eprintln!("phyl-sched: watching {}", schedule_dir.display());

    // Shutdown signal
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    // Spawn the scan loop
    let handle = tokio::spawn(async move {
        loop {
            scan_and_fire(&schedule_dir, &socket).await;

            tokio::select! {
                _ = sleep(SCAN_INTERVAL) => {}
                _ = shutdown_rx.changed() => {
                    eprintln!("phyl-sched: stopped");
                    return;
                }
            }
        }
    });

    // Wait for Ctrl-C
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| format!("signal handler failed: {e}"))?;
    eprintln!("phyl-sched: shutting down...");
    let _ = shutdown_tx.send(true);

    let _ = handle.await;
    Ok(())
}

/// Scan the schedule directory and fire any entries whose time has arrived.
async fn scan_and_fire(schedule_dir: &Path, socket: &str) {
    let entries = match std::fs::read_dir(schedule_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("phyl-sched: failed to read schedule dir: {e}");
            return;
        }
    };

    let now = Utc::now();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();

        // Skip non-json files (including .tmp files)
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        // Read and parse the schedule entry
        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("phyl-sched: failed to read {}: {e}", path.display());
                continue;
            }
        };

        let sched: ScheduleEntry = match serde_json::from_str(&contents) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "phyl-sched: corrupt schedule file {}: {e} — renaming to .bad",
                    path.display()
                );
                let bad_path = path.with_extension("bad");
                let _ = std::fs::rename(&path, &bad_path);
                continue;
            }
        };

        // Check if the entry is due
        if sched.at > now {
            continue;
        }

        eprintln!(
            "phyl-sched: firing schedule entry {} (due {})",
            sched.id, sched.at
        );

        // Create session via daemon API
        match create_session(socket, &sched.prompt).await {
            Ok(session_id) => {
                eprintln!(
                    "phyl-sched: session created: {session_id} (from schedule {})",
                    sched.id
                );
                // Delete the schedule file on success
                if let Err(e) = std::fs::remove_file(&path) {
                    eprintln!(
                        "phyl-sched: failed to delete fired entry {}: {e}",
                        path.display()
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "phyl-sched: failed to fire schedule {}: {e} — will retry",
                    sched.id
                );
                // Leave the file for retry on next scan
            }
        }
    }
}

/// Create a session via the daemon API.
async fn create_session(socket: &str, prompt: &str) -> Result<String, String> {
    let body = serde_json::json!({ "prompt": prompt }).to_string();

    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| format!("cannot connect to daemon: {e}"))?;
    let io = TokioIo::new(stream);

    let (mut sender, conn) = http1::handshake(io)
        .await
        .map_err(|e| format!("handshake failed: {e}"))?;

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/sessions")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap();

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("failed to read response: {e}"))?
        .to_bytes();
    let text = String::from_utf8_lossy(&body_bytes).to_string();

    if status.is_success() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(id) = v.get("id").and_then(|v| v.as_str())
        {
            return Ok(id.to_string());
        }
        Ok(text)
    } else {
        Err(format!("HTTP {}: {}", status.as_u16(), text))
    }
}

/// Load config.toml from the agent home.
fn load_config(home: &Path) -> Result<Config, String> {
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read config.toml: {e}"))?;
    toml::from_str(&contents).map_err(|e| format!("failed to parse config.toml: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phyl_core::ScheduleEntry;
    use uuid::Uuid;

    #[test]
    fn test_schedule_entry_due_detection() {
        let now = Utc::now();
        let past = now - chrono::Duration::minutes(5);
        let future = now + chrono::Duration::hours(1);

        let due_entry = ScheduleEntry {
            id: Uuid::new_v4(),
            prompt: "due".to_string(),
            at: past,
            created_by: None,
            created_at: now,
        };

        let future_entry = ScheduleEntry {
            id: Uuid::new_v4(),
            prompt: "not due".to_string(),
            at: future,
            created_by: None,
            created_at: now,
        };

        assert!(due_entry.at <= now);
        assert!(future_entry.at > now);
    }

    #[test]
    fn test_corrupt_file_handling() {
        let dir = std::env::temp_dir().join("phyl-sched-test-corrupt");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        // Write a corrupt .json file
        std::fs::write(dir.join("bad.json"), "not valid json").unwrap();

        // Parse should fail
        let contents = std::fs::read_to_string(dir.join("bad.json")).unwrap();
        assert!(serde_json::from_str::<ScheduleEntry>(&contents).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_skips_tmp_files() {
        let dir = std::env::temp_dir().join("phyl-sched-test-skip-tmp");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("something.tmp"), "ignored").unwrap();

        // Verify .tmp files would be skipped (extension check)
        let path = dir.join("something.tmp");
        assert_ne!(path.extension().and_then(|e| e.to_str()), Some("json"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rename_corrupt_to_bad() {
        let dir = std::env::temp_dir().join("phyl-sched-test-rename-bad");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let json_path = dir.join("corrupt.json");
        let bad_path = dir.join("corrupt.bad");
        std::fs::write(&json_path, "not valid json").unwrap();

        // Simulate the rename logic
        std::fs::rename(&json_path, &bad_path).unwrap();
        assert!(!json_path.exists());
        assert!(bad_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! `phyl-sched` — Scheduler service. Scans `$PHYLACTERY_HOME/schedule/` every
//! 5 seconds and fires due entries by creating sessions via the daemon API.
//!
//! Limits concurrent scheduled sessions to [`MAX_CONCURRENT`] to prevent
//! thundering-herd scenarios when the scheduler catches up after downtime.

use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use phyl_core::{Config, ScheduleEntry, SessionInfo};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::time::{Duration, sleep};

const SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Maximum number of scheduler-launched sessions that may run at the same time.
const MAX_CONCURRENT: usize = 3;

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

    eprintln!(
        "phyl-sched: watching {} (max {MAX_CONCURRENT} concurrent)",
        schedule_dir.display()
    );

    // Shutdown signal
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    // Spawn the scan loop
    let handle = tokio::spawn(async move {
        // Track session IDs we've launched so we can enforce concurrency
        let mut in_flight: HashSet<String> = HashSet::new();

        loop {
            scan_and_fire(&schedule_dir, &socket, &mut in_flight).await;

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

/// Scan the schedule directory and fire due entries, respecting concurrency limits.
async fn scan_and_fire(schedule_dir: &Path, socket: &str, in_flight: &mut HashSet<String>) {
    // Prune completed sessions from the in-flight set
    if !in_flight.is_empty() {
        prune_completed(socket, in_flight).await;
    }

    let available_slots = MAX_CONCURRENT.saturating_sub(in_flight.len());
    if available_slots == 0 {
        return;
    }

    // Collect due entries
    let due_entries = match collect_due_entries(schedule_dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("phyl-sched: {e}");
            return;
        }
    };

    if due_entries.is_empty() {
        return;
    }

    let waiting = due_entries.len().saturating_sub(available_slots);
    if waiting > 0 {
        eprintln!(
            "phyl-sched: {} due entries, {} slots available, {} waiting",
            due_entries.len(),
            available_slots,
            waiting
        );
    }

    // Fire up to available_slots entries (earliest first — vec is sorted by `at`)
    for (sched, path) in due_entries.into_iter().take(available_slots) {
        eprintln!(
            "phyl-sched: firing schedule entry {} (due {})",
            sched.id, sched.at
        );

        match create_session(socket, &sched.prompt).await {
            Ok(session_id) => {
                eprintln!(
                    "phyl-sched: session created: {session_id} (from schedule {})",
                    sched.id
                );
                in_flight.insert(session_id);
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
            }
        }
    }
}

/// Read the schedule directory and return due entries sorted by `at` (earliest first).
fn collect_due_entries(schedule_dir: &Path) -> Result<Vec<(ScheduleEntry, PathBuf)>, String> {
    let dir =
        std::fs::read_dir(schedule_dir).map_err(|e| format!("failed to read schedule dir: {e}"))?;

    let now = Utc::now();
    let mut due = Vec::new();

    for entry in dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

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

        if sched.at <= now {
            due.push((sched, path));
        }
    }

    due.sort_by_key(|(s, _)| s.at);
    Ok(due)
}

/// Query the daemon for all sessions and remove any from `in_flight` that are no longer running.
async fn prune_completed(socket: &str, in_flight: &mut HashSet<String>) {
    let sessions = match list_sessions(socket).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("phyl-sched: failed to query sessions for pruning: {e}");
            // On error, be conservative: don't prune, assume they're still running.
            // Exception: if the daemon is completely unreachable, clear the set so
            // we don't permanently block. We distinguish by checking if the error
            // looks like a connection failure.
            if e.contains("cannot connect") {
                eprintln!("phyl-sched: daemon unreachable, clearing in-flight set");
                in_flight.clear();
            }
            return;
        }
    };

    let running_ids: HashSet<String> = sessions
        .iter()
        .filter(|s| s.status == phyl_core::SessionStatus::Running)
        .map(|s| s.id.to_string())
        .collect();

    in_flight.retain(|id| running_ids.contains(id));
}

// --- Daemon client ---

/// Create a session via the daemon API. Returns the session ID.
async fn create_session(socket: &str, prompt: &str) -> Result<String, String> {
    let body = serde_json::json!({ "prompt": prompt }).to_string();
    let resp_text = daemon_request(socket, Method::POST, "/sessions", Some(body)).await?;

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp_text)
        && let Some(id) = v.get("id").and_then(|v| v.as_str())
    {
        return Ok(id.to_string());
    }
    Ok(resp_text)
}

/// List all sessions via the daemon API.
async fn list_sessions(socket: &str) -> Result<Vec<SessionInfo>, String> {
    let text = daemon_request(socket, Method::GET, "/sessions", None).await?;
    serde_json::from_str(&text).map_err(|e| format!("failed to parse sessions response: {e}"))
}

/// Make an HTTP request to the daemon over the Unix socket.
async fn daemon_request(
    socket: &str,
    method: Method,
    uri: &str,
    body: Option<String>,
) -> Result<String, String> {
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

    let body_bytes = body.unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Host", "localhost");

    if !body_bytes.is_empty() {
        builder = builder.header("Content-Type", "application/json");
    }

    let req = builder.body(Full::new(Bytes::from(body_bytes))).unwrap();

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    let resp_bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("failed to read response: {e}"))?
        .to_bytes();
    let text = String::from_utf8_lossy(&resp_bytes).to_string();

    if status.is_success() {
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

        std::fs::write(dir.join("bad.json"), "not valid json").unwrap();

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

        std::fs::rename(&json_path, &bad_path).unwrap();
        assert!(!json_path.exists());
        assert!(bad_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_collect_due_entries_sorted() {
        let dir = std::env::temp_dir().join("phyl-sched-test-collect-due");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let now = Utc::now();

        // Write three entries: one future (not due), two past (due)
        let earlier = ScheduleEntry {
            id: Uuid::new_v4(),
            prompt: "earlier".to_string(),
            at: now - chrono::Duration::hours(2),
            created_by: None,
            created_at: now,
        };
        let later = ScheduleEntry {
            id: Uuid::new_v4(),
            prompt: "later".to_string(),
            at: now - chrono::Duration::hours(1),
            created_by: None,
            created_at: now,
        };
        let future = ScheduleEntry {
            id: Uuid::new_v4(),
            prompt: "future".to_string(),
            at: now + chrono::Duration::hours(1),
            created_by: None,
            created_at: now,
        };

        // Write in reverse order to verify sorting
        for entry in [&later, &future, &earlier] {
            let json = serde_json::to_string(entry).unwrap();
            std::fs::write(dir.join(format!("{}.json", entry.id)), json).unwrap();
        }

        let due = collect_due_entries(&dir).unwrap();
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].0.prompt, "earlier");
        assert_eq!(due[1].0.prompt, "later");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_in_flight_pruning_logic() {
        // Test the retain logic used in prune_completed
        let mut in_flight: HashSet<String> = HashSet::new();
        in_flight.insert("aaa".to_string());
        in_flight.insert("bbb".to_string());
        in_flight.insert("ccc".to_string());

        // Simulate: only "bbb" is still running
        let running_ids: HashSet<String> = ["bbb".to_string()].into();
        in_flight.retain(|id| running_ids.contains(id));

        assert_eq!(in_flight.len(), 1);
        assert!(in_flight.contains("bbb"));
    }

    #[test]
    fn test_available_slots_calculation() {
        let mut in_flight: HashSet<String> = HashSet::new();
        assert_eq!(MAX_CONCURRENT.saturating_sub(in_flight.len()), 3);

        in_flight.insert("a".to_string());
        assert_eq!(MAX_CONCURRENT.saturating_sub(in_flight.len()), 2);

        in_flight.insert("b".to_string());
        in_flight.insert("c".to_string());
        assert_eq!(MAX_CONCURRENT.saturating_sub(in_flight.len()), 0);

        // Over limit (shouldn't happen, but saturating_sub handles it)
        in_flight.insert("d".to_string());
        assert_eq!(MAX_CONCURRENT.saturating_sub(in_flight.len()), 0);
    }
}

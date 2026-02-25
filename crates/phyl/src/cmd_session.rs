//! `phyl session [-d] "prompt"` — start a session.
//!
//! Without `-d`: POST /sessions, then tail log.jsonl to stdout.
//! With `-d`: POST /sessions, print session ID, return.

use crate::client;
use crate::format::format_log_entry;
use anyhow::{Context, bail};
use phyl_core::LogEntry;
use serde::Deserialize;

#[derive(Deserialize)]
struct CreateResponse {
    id: uuid::Uuid,
    #[allow(dead_code)]
    status: String,
}

pub async fn run(prompt: &str, detach: bool) -> anyhow::Result<()> {
    let socket = client::socket_path();
    let body = serde_json::json!({ "prompt": prompt }).to_string();

    let (status, resp) = client::post(&socket, "/sessions", &body).await?;

    if !status.is_success() {
        bail!("HTTP {}: {}", status.as_u16(), resp.trim());
    }

    let created: CreateResponse = serde_json::from_str(&resp).context("bad response")?;

    if detach {
        println!("{}", created.id);
        return Ok(());
    }

    // Foreground mode: tail the session's log.jsonl.
    eprintln!(
        "Session {} started. Streaming log (Ctrl-C to detach)...",
        created.id
    );

    let home = phyl_core::home_dir();
    let log_path = home
        .join("sessions")
        .join(created.id.to_string())
        .join("log.jsonl");

    tail_log(&log_path, &socket, &created.id.to_string()).await
}

/// Tail a log.jsonl file, printing new entries as they appear.
/// Stops when the session is no longer running.
async fn tail_log(
    log_path: &std::path::Path,
    socket: &str,
    session_id: &str,
) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let mut offset: u64 = 0;
    let mut done = false;

    while !done {
        // Try to read new entries from the log file.
        if let Ok(mut file) = std::fs::File::open(log_path) {
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            if size > offset {
                let _ = file.seek(SeekFrom::Start(offset));
                let reader = BufReader::new(file);
                for line in reader.lines() {
                    let line = match line {
                        Ok(l) => l,
                        Err(_) => break,
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
                        format_log_entry(&entry);
                        if entry.entry_type == phyl_core::LogEntryType::Done
                            || entry.entry_type == phyl_core::LogEntryType::Error
                        {
                            done = true;
                        }
                    }
                }
                offset = size;
            }
        }

        if done {
            break;
        }

        // Check if session is still running via daemon API.
        if let Ok((status, body)) = client::get(socket, &format!("/sessions/{session_id}")).await
            && status.is_success()
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&body)
            && let Some(s) = val.get("status").and_then(|v| v.as_str())
            && s != "running"
        {
            // Read any remaining log entries.
            if let Ok(mut file) = std::fs::File::open(log_path) {
                use std::io::Read;
                let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                if size > offset {
                    let _ = file.seek(SeekFrom::Start(offset));
                    let mut buf = String::new();
                    let _ = file.read_to_string(&mut buf);
                    for line in buf.lines() {
                        if line.trim().is_empty() {
                            continue;
                        }
                        if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
                            format_log_entry(&entry);
                        }
                    }
                }
            }
            break;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(())
}

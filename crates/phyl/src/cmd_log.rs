//! `phyl log <id>` — tail a session's log.jsonl.

use crate::client;
use crate::format::format_log_entry;
use anyhow::{Context, bail};
use phyl_core::{LogEntry, LogEntryType};

pub async fn run(id: &str) -> anyhow::Result<()> {
    let socket = client::socket_path();

    // Verify session exists.
    let path = format!("/sessions/{id}");
    let (status, body) = client::get(&socket, &path).await?;

    if !status.is_success() {
        bail!("HTTP {}: {}", status.as_u16(), body.trim());
    }

    // Determine log file path.
    let home = phyl_core::home_dir();
    let log_path = home.join("sessions").join(id).join("log.jsonl");

    // Check if session is already finished — if so, just dump the log.
    let session_status = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from));

    let is_finished = matches!(
        session_status.as_deref(),
        Some("done") | Some("crashed") | Some("timed_out")
    );

    if is_finished {
        // Just dump the full log.
        dump_log(&log_path)?;
        return Ok(());
    }

    // Tail mode: print existing entries then follow.
    tail_log(&log_path, &socket, id).await
}

/// Dump all log entries from a file.
fn dump_log(log_path: &std::path::Path) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(log_path).context("cannot open log")?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            format_log_entry(&entry);
        }
    }

    Ok(())
}

/// Tail a log file, printing new entries as they appear.
async fn tail_log(
    log_path: &std::path::Path,
    socket: &str,
    session_id: &str,
) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let mut offset: u64 = 0;

    loop {
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
                        if entry.entry_type == LogEntryType::Done
                            || entry.entry_type == LogEntryType::Error
                        {
                            return Ok(());
                        }
                    }
                }
                offset = size;
            }
        }

        // Check if session is still running.
        if let Ok((st, body)) = client::get(socket, &format!("/sessions/{session_id}")).await
            && st.is_success()
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&body)
            && let Some(s) = val.get("status").and_then(|v| v.as_str())
            && s != "running"
        {
            // Drain remaining entries.
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
            return Ok(());
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

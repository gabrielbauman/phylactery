//! Background async tasks and event types for the TUI.

use crate::client;
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyEvent};
use http_body_util::BodyExt;
use phyl_core::{LogEntry, ScheduleEntry, SessionInfo};
use std::io::{BufRead, Seek};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

/// All events consumed by the main TUI loop.
pub enum AppEvent {
    /// Terminal key event.
    Key(KeyEvent),
    /// Periodic tick for UI refresh.
    Tick,
    /// Session list refreshed from daemon.
    SessionsUpdated(Vec<SessionInfo>),
    /// SSE feed event (question, done, error).
    FeedEvent { session_id: Uuid, entry: LogEntry },
    /// Schedule entries loaded from filesystem.
    ScheduleUpdated(Vec<ScheduleEntry>),
    /// New log entries for the currently-viewed session.
    LogEntries {
        session_id: Uuid,
        entries: Vec<LogEntry>,
    },
    /// Daemon health status.
    DaemonStatus { ok: bool, active: usize },
    /// Daemon connection error.
    DaemonError(String),
}

/// Spawn the terminal event reader. Produces Key and Tick events.
pub fn spawn_terminal_reader(tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        loop {
            // Poll crossterm for events with a 200ms timeout (tick rate).
            let has_event = tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(200)))
                .await
                .unwrap_or(Ok(false))
                .unwrap_or(false);

            if has_event {
                if let Ok(Event::Key(key)) = tokio::task::spawn_blocking(event::read)
                    .await
                    .unwrap_or(Ok(Event::FocusLost))
                    && tx.send(AppEvent::Key(key)).await.is_err()
                {
                    break;
                }
            } else {
                // No event within timeout — send a tick.
                if tx.send(AppEvent::Tick).await.is_err() {
                    break;
                }
            }
        }
    });
}

/// Spawn the session list poller. Polls GET /sessions every 3 seconds.
pub fn spawn_session_poller(tx: mpsc::Sender<AppEvent>) {
    let socket = client::socket_path();
    tokio::spawn(async move {
        loop {
            match client::get(&socket, "/sessions").await {
                Ok((status, body)) if status.is_success() => {
                    if let Ok(sessions) = serde_json::from_str::<Vec<SessionInfo>>(&body) {
                        let active = sessions
                            .iter()
                            .filter(|s| s.status == phyl_core::SessionStatus::Running)
                            .count();
                        let _ = tx.send(AppEvent::DaemonStatus { ok: true, active }).await;
                        let _ = tx.send(AppEvent::SessionsUpdated(sessions)).await;
                    }
                }
                Ok((status, body)) => {
                    let _ = tx
                        .send(AppEvent::DaemonError(format!("HTTP {}: {}", status, body)))
                        .await;
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::DaemonError(e.to_string())).await;
                    let _ = tx
                        .send(AppEvent::DaemonStatus {
                            ok: false,
                            active: 0,
                        })
                        .await;
                }
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
}

/// Spawn the SSE feed reader. Connects to GET /feed and streams events.
pub fn spawn_feed_reader(tx: mpsc::Sender<AppEvent>) {
    let socket = client::socket_path();
    tokio::spawn(async move {
        loop {
            match read_feed(&socket, &tx).await {
                Ok(()) => {}
                Err(e) => {
                    let msg = e.to_string();
                    let _ = tx
                        .send(AppEvent::DaemonError(format!("SSE feed: {msg}")))
                        .await;
                }
            }
            // Reconnect after delay.
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
}

async fn read_feed(
    socket: &str,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (status, body) = client::get_stream(socket, "/feed").await?;
    if !status.is_success() {
        return Err(format!("feed returned HTTP {}", status).into());
    }

    let mut body = body;
    let mut buf = String::new();

    loop {
        let frame = match body.frame().await {
            Some(Ok(f)) => f,
            Some(Err(e)) => return Err(e.into()),
            None => return Ok(()),
        };

        if let Some(data) = frame.data_ref() {
            let chunk = String::from_utf8_lossy(data);
            buf.push_str(&chunk);

            while let Some(pos) = buf.find("\n\n") {
                let msg = buf[..pos].to_string();
                buf = buf[pos + 2..].to_string();
                if let Some(event) = parse_sse_message(&msg)
                    && tx.send(event).await.is_err()
                {
                    return Ok(());
                }
            }
        }
    }
}

fn parse_sse_message(msg: &str) -> Option<AppEvent> {
    let mut data = String::new();

    for line in msg.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data = rest.trim().to_string();
        }
    }

    if data.is_empty() {
        return None;
    }

    let val: serde_json::Value = serde_json::from_str(&data).ok()?;
    let session_id: Uuid = val.get("session_id")?.as_str()?.parse().ok()?;
    let entry: LogEntry = serde_json::from_value(val.get("entry")?.clone()).ok()?;

    Some(AppEvent::FeedEvent { session_id, entry })
}

/// Spawn the schedule scanner. Reads $PHYLACTERY_HOME/schedule/ every 5 seconds.
pub fn spawn_schedule_scanner(tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        loop {
            let mut entries = Vec::new();
            let schedule_dir = phyl_core::home_dir().join("schedule");

            if let Ok(dir) = std::fs::read_dir(&schedule_dir) {
                for file in dir.flatten() {
                    let path = file.path();
                    if path.extension().is_some_and(|e| e == "json")
                        && let Ok(contents) = std::fs::read_to_string(&path)
                        && let Ok(entry) = serde_json::from_str::<ScheduleEntry>(&contents)
                    {
                        entries.push(entry);
                    }
                }
            }

            entries.sort_by_key(|e| e.at);
            let _ = tx.send(AppEvent::ScheduleUpdated(entries)).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

/// Spawn the log tailer. Watches a session's log.jsonl when in chat view.
pub fn spawn_log_tailer(tx: mpsc::Sender<AppEvent>, mut target: watch::Receiver<Option<Uuid>>) {
    tokio::spawn(async move {
        let mut current_id: Option<Uuid> = None;
        let mut offset: u64 = 0;

        loop {
            // Check if the target changed.
            if target.has_changed().unwrap_or(false) {
                let new_target = *target.borrow_and_update();
                if new_target != current_id {
                    current_id = new_target;
                    offset = 0;
                }
            }

            if let Some(id) = current_id {
                let log_path = phyl_core::home_dir()
                    .join("sessions")
                    .join(id.to_string())
                    .join("log.jsonl");

                if let Ok(mut file) = std::fs::File::open(&log_path)
                    && file.seek(std::io::SeekFrom::Start(offset)).is_ok()
                {
                    let reader = std::io::BufReader::new(&mut file);
                    let mut new_entries = Vec::new();
                    let mut new_offset = offset;

                    for line in reader.lines() {
                        match line {
                            Ok(line) if !line.is_empty() => {
                                new_offset += line.len() as u64 + 1; // +1 for newline
                                if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
                                    new_entries.push(entry);
                                }
                            }
                            _ => break,
                        }
                    }

                    if !new_entries.is_empty() {
                        offset = new_offset;
                        let _ = tx
                            .send(AppEvent::LogEntries {
                                session_id: id,
                                entries: new_entries,
                            })
                            .await;
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

/// Format a timestamp as relative time (e.g., "2m ago", "in 3h").
pub fn relative_time(ts: DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = now.signed_duration_since(ts);

    if diff.num_seconds() < 0 {
        // Future time.
        let abs = -diff.num_seconds();
        if abs < 60 {
            format!("in {}s", abs)
        } else if abs < 3600 {
            format!("in {}m", abs / 60)
        } else if abs < 86400 {
            format!("in {}h", abs / 3600)
        } else {
            format!("in {}d", abs / 86400)
        }
    } else {
        let secs = diff.num_seconds();
        if secs < 60 {
            format!("{}s ago", secs)
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else if secs < 86400 {
            format!("{}h ago", secs / 3600)
        } else {
            format!("{}d ago", secs / 86400)
        }
    }
}

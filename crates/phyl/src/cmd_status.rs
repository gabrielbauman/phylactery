//! `phyl status <id>` — show session detail.

use crate::client;
use crate::format::format_log_entry;
use phyl_core::{LogEntry, SessionInfo};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct SessionDetail {
    #[serde(flatten)]
    info: SessionInfo,
    prompt: String,
    recent_log: Vec<LogEntry>,
}

pub async fn run(id: &str) -> Result<(), String> {
    let socket = client::socket_path();
    let path = format!("/sessions/{id}");
    let (status, body) = client::get(&socket, &path)
        .await
        .map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status.as_u16(), body.trim()));
    }

    let detail: SessionDetail =
        serde_json::from_str(&body).map_err(|e| format!("bad response: {e}"))?;

    let status_str = format!("{:?}", detail.info.status).to_lowercase();
    let created = detail.info.created_at.format("%Y-%m-%d %H:%M:%S UTC");

    println!("Session:  {}", detail.info.id);
    println!("Status:   {}", status_str);
    println!("Created:  {}", created);
    println!("Prompt:   {}", detail.prompt);
    if let Some(ref summary) = detail.info.summary {
        println!("Summary:  {}", summary);
    }

    if !detail.recent_log.is_empty() {
        println!();
        println!("--- Recent log ({} entries) ---", detail.recent_log.len());
        for entry in &detail.recent_log {
            format_log_entry(entry);
        }
    }

    Ok(())
}

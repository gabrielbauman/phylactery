//! `phyl ls` — list sessions.

use crate::client;
use phyl_core::SessionInfo;

pub async fn run() -> Result<(), String> {
    let socket = client::socket_path();
    let (status, body) = client::get(&socket, "/sessions")
        .await
        .map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status.as_u16(), body));
    }

    let sessions: Vec<SessionInfo> =
        serde_json::from_str(&body).map_err(|e| format!("bad response: {e}"))?;

    if sessions.is_empty() {
        println!("No sessions.");
        return Ok(());
    }

    // Print header.
    println!(
        "{:<38} {:<10} {:<20} {}",
        "ID", "STATUS", "CREATED", "SUMMARY"
    );
    println!("{}", "-".repeat(90));

    for s in &sessions {
        let status_str = format!("{:?}", s.status).to_lowercase();
        let created = s.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let summary = s
            .summary
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(40)
            .collect::<String>();
        println!("{:<38} {:<10} {:<20} {}", s.id, status_str, created, summary);
    }

    Ok(())
}

//! `phyl stop <id>` — kill a running session.

use crate::client;

pub async fn run(id: &str) -> Result<(), String> {
    let socket = client::socket_path();
    let path = format!("/sessions/{id}");

    let (status, body) = client::delete(&socket, &path)
        .await
        .map_err(|e| e.to_string())?;

    if status.is_success() {
        eprintln!("Session {id} stopped.");
        Ok(())
    } else {
        Err(format!("HTTP {}: {}", status.as_u16(), body.trim()))
    }
}

//! `phyl say <id> "msg"` — inject an event into a running session.

use crate::client;

pub async fn run(id: &str, message: &str) -> Result<(), String> {
    let socket = client::socket_path();
    let path = format!("/sessions/{id}/events");
    let body = serde_json::json!({ "content": message }).to_string();

    let (status, resp_body) = client::post(&socket, &path, &body)
        .await
        .map_err(|e| e.to_string())?;

    if status.is_success() {
        eprintln!("Event sent to session {id}.");
        Ok(())
    } else {
        Err(format!("HTTP {}: {}", status.as_u16(), resp_body.trim()))
    }
}

//! `phyl say <id> "msg"` — inject an event into a running session.

use crate::client;
use anyhow::bail;

pub async fn run(id: &str, message: &str) -> anyhow::Result<()> {
    let socket = client::socket_path();
    let path = format!("/sessions/{id}/events");
    let body = serde_json::json!({ "content": message }).to_string();

    let (status, resp_body) = client::post(&socket, &path, &body).await?;

    if status.is_success() {
        eprintln!("Event sent to session {id}.");
        Ok(())
    } else {
        bail!("HTTP {}: {}", status.as_u16(), resp_body.trim())
    }
}

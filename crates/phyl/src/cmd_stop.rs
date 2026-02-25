//! `phyl stop <id>` — kill a running session.

use crate::client;
use anyhow::bail;

pub async fn run(id: &str) -> anyhow::Result<()> {
    let socket = client::socket_path();
    let path = format!("/sessions/{id}");

    let (status, body) = client::delete(&socket, &path).await?;

    if status.is_success() {
        eprintln!("Session {id} stopped.");
        Ok(())
    } else {
        bail!("HTTP {}: {}", status.as_u16(), body.trim())
    }
}

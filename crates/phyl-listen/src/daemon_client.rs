//! Shared daemon client for creating sessions via Unix socket.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;

/// Create a session via the daemon API.
pub async fn create_session(socket: &str, prompt: &str) -> Result<String, String> {
    let body = serde_json::json!({ "prompt": prompt }).to_string();

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

    let req = Request::builder()
        .method(Method::POST)
        .uri("/sessions")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap();

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("failed to read response: {e}"))?
        .to_bytes();
    let text = String::from_utf8_lossy(&body_bytes).to_string();

    if status.is_success() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(id) = v.get("id").and_then(|v| v.as_str()) {
                return Ok(id.to_string());
            }
        }
        Ok(text)
    } else {
        Err(format!("HTTP {}: {}", status.as_u16(), text))
    }
}

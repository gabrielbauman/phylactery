//! HTTP client for communicating with phylactd over a Unix socket.

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use phyl_core::Config;
use tokio::net::UnixStream;

/// Resolve the daemon socket path from config.toml or defaults.
pub fn socket_path() -> String {
    let home = phyl_core::home_dir();
    let config_path = home.join("config.toml");
    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match toml::from_str::<Config>(&contents) {
            Ok(c) => c.daemon.socket,
            Err(_) => Config::default().daemon.socket,
        },
        Err(_) => Config::default().daemon.socket,
    }
}

/// Error type for client operations.
#[derive(Debug)]
pub struct ClientError {
    pub status: Option<StatusCode>,
    pub message: String,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(status) = self.status {
            write!(f, "HTTP {}: {}", status.as_u16(), self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for ClientError {}

impl ClientError {
    fn connect(e: impl std::fmt::Display) -> Self {
        ClientError {
            status: None,
            message: format!(
                "cannot connect to daemon: {e}\nIs phylactd running? Try: phyl start"
            ),
        }
    }

    fn request(e: impl std::fmt::Display) -> Self {
        ClientError {
            status: None,
            message: format!("request failed: {e}"),
        }
    }
}

/// Make a GET request, return (status, body).
pub async fn get(socket: &str, path: &str) -> Result<(StatusCode, String), ClientError> {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("Host", "localhost")
        .body(Empty::<Bytes>::new())
        .map_err(|e| ClientError::request(e))?;

    let (status, body) = send_request(socket, req).await?;
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ClientError::request(e))?
        .to_bytes();
    let text = String::from_utf8_lossy(&body_bytes).to_string();
    Ok((status, text))
}

/// Make a POST request with JSON body, return (status, body).
pub async fn post(
    socket: &str,
    path: &str,
    json_body: &str,
) -> Result<(StatusCode, String), ClientError> {
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(json_body.to_string())))
        .map_err(|e| ClientError::request(e))?;

    let (status, body) = send_request(socket, req).await?;
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ClientError::request(e))?
        .to_bytes();
    let text = String::from_utf8_lossy(&body_bytes).to_string();
    Ok((status, text))
}

/// Make a DELETE request, return (status, body).
pub async fn delete(socket: &str, path: &str) -> Result<(StatusCode, String), ClientError> {
    let req = Request::builder()
        .method(Method::DELETE)
        .uri(path)
        .header("Host", "localhost")
        .body(Empty::<Bytes>::new())
        .map_err(|e| ClientError::request(e))?;

    let (status, body) = send_request(socket, req).await?;
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ClientError::request(e))?
        .to_bytes();
    let text = String::from_utf8_lossy(&body_bytes).to_string();
    Ok((status, text))
}

/// Make a GET request and return the raw streaming body (for SSE).
pub async fn get_stream(
    socket: &str,
    path: &str,
) -> Result<(StatusCode, Incoming), ClientError> {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("Host", "localhost")
        .body(Empty::<Bytes>::new())
        .map_err(|e| ClientError::request(e))?;

    send_request(socket, req).await
}

/// Low-level: connect to Unix socket, perform HTTP handshake, send request.
async fn send_request<B>(
    socket: &str,
    req: Request<B>,
) -> Result<(StatusCode, Incoming), ClientError>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| ClientError::connect(e))?;
    let io = TokioIo::new(stream);

    let (mut sender, conn) = http1::handshake(io)
        .await
        .map_err(|e| ClientError::connect(e))?;

    // Drive the connection in the background.
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| ClientError::request(e))?;

    let status = resp.status();
    let body = resp.into_body();
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_error_display_with_status() {
        let err = ClientError {
            status: Some(StatusCode::NOT_FOUND),
            message: "session not found".to_string(),
        };
        let s = format!("{err}");
        assert!(s.contains("404"));
        assert!(s.contains("session not found"));
    }

    #[test]
    fn test_client_error_display_without_status() {
        let err = ClientError {
            status: None,
            message: "connection refused".to_string(),
        };
        let s = format!("{err}");
        assert_eq!(s, "connection refused");
    }

    #[test]
    fn test_client_error_connect_message() {
        let err = ClientError::connect("connection refused");
        assert!(err.message.contains("cannot connect to daemon"));
        assert!(err.message.contains("phyl start"));
        assert!(err.status.is_none());
    }

    #[test]
    fn test_client_error_request_message() {
        let err = ClientError::request("timeout");
        assert!(err.message.contains("request failed"));
        assert!(err.message.contains("timeout"));
    }

    #[test]
    fn test_socket_path_returns_default_when_no_config() {
        // With no PHYLACTERY_HOME set to a real dir, it should fall back to defaults.
        let path = socket_path();
        assert!(path.contains("phylactery.sock"));
    }
}

//! `phyl-bridge-signal` — Two-way Signal Messenger bridge.
//!
//! Connects to the daemon's SSE feed (`GET /feed`) and forwards attention events
//! (questions, done, errors) as Signal messages. Listens for inbound Signal
//! messages and routes them as answers to pending questions or as new session
//! requests.
//!
//! Only accepts messages from the configured owner number.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use http_body_util::{BodyExt, Empty, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use phyl_core::{Config, LogEntry, SignalBridgeConfig};
use serde::Deserialize;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::net::UnixStream;

// ---------------------------------------------------------------------------
// Pending question tracking
// ---------------------------------------------------------------------------

/// A question awaiting a human answer via Signal.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PendingQuestion {
    session_id: String,
    question_id: String,
    text: String,
    options: Vec<String>,
    received_at: DateTime<Utc>,
}

type SharedState = Arc<Mutex<VecDeque<PendingQuestion>>>;

// ---------------------------------------------------------------------------
// signal-cli JSON output types
// ---------------------------------------------------------------------------

/// Top-level envelope from `signal-cli receive --json`.
#[derive(Debug, Deserialize)]
struct SignalEnvelope {
    envelope: Option<EnvelopeInner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvelopeInner {
    source_number: Option<String>,
    #[allow(dead_code)]
    source: Option<String>,
    data_message: Option<DataMessage>,
}

#[derive(Debug, Deserialize)]
struct DataMessage {
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// signal-cli wrapper
// ---------------------------------------------------------------------------

/// Send a message to the owner via signal-cli.
async fn signal_send(cfg: &SignalBridgeConfig, message: &str) -> Result<(), String> {
    let output = tokio::process::Command::new(&cfg.signal_cli)
        .args(["-a", &cfg.phone, "send", "-m", message, &cfg.owner])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("failed to run signal-cli send: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("signal-cli send failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Receive pending messages via signal-cli. Returns parsed inbound messages
/// as `(source_number, message_text)` pairs.
async fn signal_receive(cfg: &SignalBridgeConfig) -> Result<Vec<(String, String)>, String> {
    // --output=json is a global option (must come before the subcommand).
    // --timeout is a receive subcommand option.
    let output = tokio::process::Command::new(&cfg.signal_cli)
        .args([
            "-a",
            &cfg.phone,
            "--output=json",
            "receive",
            "--timeout",
            "2",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("failed to run signal-cli receive: {e}"))?;

    // signal-cli receive may exit non-zero on timeout with no messages — that's ok.
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut messages = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(env) = serde_json::from_str::<SignalEnvelope>(trimmed) {
            if let Some(inner) = env.envelope {
                let source = inner
                    .source_number
                    .or(inner.source)
                    .unwrap_or_default();
                if let Some(dm) = inner.data_message {
                    if let Some(msg) = dm.message {
                        if !msg.is_empty() {
                            messages.push((source, msg));
                        }
                    }
                }
            }
        }
    }

    Ok(messages)
}

// ---------------------------------------------------------------------------
// Daemon HTTP client (minimal, Unix socket)
// ---------------------------------------------------------------------------

/// Make a GET request returning a streaming body (for SSE).
async fn daemon_get_stream(
    socket: &str,
    path: &str,
) -> Result<(StatusCode, hyper::body::Incoming), String> {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("Host", "localhost")
        .body(Empty::<Bytes>::new())
        .unwrap();

    daemon_send(socket, req).await
}

/// Make a POST request with a JSON body.
async fn daemon_post(socket: &str, path: &str, json_body: &str) -> Result<String, String> {
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(json_body.to_string())))
        .unwrap();

    let (_status, body) = daemon_send(socket, req).await?;
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| format!("body read error: {e}"))?
        .to_bytes();
    Ok(String::from_utf8_lossy(&body_bytes).to_string())
}

/// Low-level: connect to Unix socket, HTTP/1.1 handshake, send request.
async fn daemon_send<B>(
    socket: &str,
    req: Request<B>,
) -> Result<(StatusCode, hyper::body::Incoming), String>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| format!("cannot connect to daemon at {socket}: {e}"))?;
    let io = TokioIo::new(stream);

    let (mut sender, conn) = http1::handshake(io)
        .await
        .map_err(|e| format!("HTTP handshake failed: {e}"))?;

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    Ok((resp.status(), resp.into_body()))
}

// ---------------------------------------------------------------------------
// SSE feed watcher
// ---------------------------------------------------------------------------

/// Connect to the daemon's SSE feed and forward attention events as Signal messages.
async fn feed_watcher(socket: String, cfg: SignalBridgeConfig, state: SharedState) {
    loop {
        eprintln!("phyl-bridge-signal: connecting to daemon feed...");

        match daemon_get_stream(&socket, "/feed").await {
            Ok((status, body)) => {
                if !status.is_success() {
                    eprintln!(
                        "phyl-bridge-signal: feed returned HTTP {}, retrying...",
                        status.as_u16()
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }

                eprintln!("phyl-bridge-signal: connected to feed");

                if let Err(e) = process_feed(body, &cfg, &state).await {
                    eprintln!("phyl-bridge-signal: feed error: {e}");
                }
            }
            Err(e) => {
                eprintln!("phyl-bridge-signal: feed connection failed: {e}");
            }
        }

        // Reconnect after a delay.
        eprintln!("phyl-bridge-signal: reconnecting in 5s...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Process SSE frames from the feed body.
async fn process_feed(
    mut body: hyper::body::Incoming,
    cfg: &SignalBridgeConfig,
    state: &SharedState,
) -> Result<(), String> {
    let mut buf = String::new();

    loop {
        let frame = match body.frame().await {
            Some(Ok(f)) => f,
            Some(Err(e)) => return Err(format!("stream error: {e}")),
            None => return Err("stream ended".to_string()),
        };

        if let Some(data) = frame.data_ref() {
            let chunk = String::from_utf8_lossy(data);
            buf.push_str(&chunk);

            // SSE messages are separated by double newlines.
            while let Some(pos) = buf.find("\n\n") {
                let msg = buf[..pos].to_string();
                buf = buf[pos + 2..].to_string();
                process_sse_message(&msg, cfg, state).await;
            }
        }
    }
}

/// Process a single SSE message and send the appropriate Signal message.
async fn process_sse_message(msg: &str, cfg: &SignalBridgeConfig, state: &SharedState) {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in msg.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data = rest.trim().to_string();
        }
    }

    if data.is_empty() {
        return;
    }

    let val: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return,
    };

    let session_id = val
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();

    let entry: LogEntry = match serde_json::from_value(
        val.get("entry").cloned().unwrap_or(serde_json::Value::Null),
    ) {
        Ok(e) => e,
        Err(_) => return,
    };

    let short_id = &session_id[..8.min(session_id.len())];

    let signal_msg = match event_type.as_str() {
        "question" => {
            let qid = entry.id.as_deref().unwrap_or("?").to_string();
            let text = entry.content.as_deref().unwrap_or("").to_string();
            let options = entry.options.clone();

            // Track the pending question.
            {
                let mut pending = state.lock().unwrap();
                pending.push_back(PendingQuestion {
                    session_id: session_id.clone(),
                    question_id: qid.clone(),
                    text: text.clone(),
                    options: options.clone(),
                    received_at: Utc::now(),
                });
                // Cap pending questions to avoid unbounded growth.
                while pending.len() > 50 {
                    pending.pop_front();
                }
            }

            let mut msg = format!("[{short_id}] Question: {text}");
            if !options.is_empty() {
                for (i, opt) in options.iter().enumerate() {
                    msg.push_str(&format!("\n  {}: {opt}", i + 1));
                }
                msg.push_str("\n\nReply with a number or type your answer.");
            }
            msg
        }
        "done" => {
            let summary = entry
                .summary
                .as_deref()
                .or(entry.content.as_deref())
                .unwrap_or("(no summary)");
            format!("[{short_id}] Done: {summary}")
        }
        "error" => {
            let err = entry.content.as_deref().unwrap_or("unknown error");
            format!("[{short_id}] Error: {err}")
        }
        _ => return,
    };

    // Send the Signal message.
    if let Err(e) = signal_send(cfg, &signal_msg).await {
        eprintln!("phyl-bridge-signal: failed to send Signal message: {e}");
    }
}

// ---------------------------------------------------------------------------
// Signal message listener
// ---------------------------------------------------------------------------

/// Poll for inbound Signal messages and route them to sessions or create new ones.
async fn signal_listener(socket: String, cfg: SignalBridgeConfig, state: SharedState) {
    loop {
        match signal_receive(&cfg).await {
            Ok(messages) => {
                for (source, text) in messages {
                    // Only accept messages from the configured owner.
                    if source != cfg.owner {
                        eprintln!(
                            "phyl-bridge-signal: ignoring message from {source} (not owner)"
                        );
                        continue;
                    }

                    process_inbound(&socket, &cfg, &state, &text).await;
                }
            }
            Err(e) => {
                eprintln!("phyl-bridge-signal: receive error: {e}");
            }
        }

        // Brief pause between receive polls.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Process a single inbound Signal message from the owner.
async fn process_inbound(
    socket: &str,
    cfg: &SignalBridgeConfig,
    state: &SharedState,
    text: &str,
) {
    let trimmed = text.trim();

    // Check if there's a pending question to answer.
    let pending = {
        let mut queue = state.lock().unwrap();
        queue.pop_front()
    };

    if let Some(pq) = pending {
        // Resolve the answer: if the reply is a number matching an option, use the option text.
        let answer = resolve_answer(trimmed, &pq.options);

        let body = serde_json::json!({
            "question_id": pq.question_id,
            "content": answer,
        })
        .to_string();

        let path = format!("/sessions/{}/events", pq.session_id);
        match daemon_post(socket, &path, &body).await {
            Ok(_) => {
                let short_id = &pq.session_id[..8.min(pq.session_id.len())];
                eprintln!("phyl-bridge-signal: answered [{short_id}]: {answer}");
            }
            Err(e) => {
                eprintln!("phyl-bridge-signal: failed to post answer: {e}");
                // Send failure notification back to owner.
                let _ = signal_send(cfg, &format!("Failed to send answer: {e}")).await;
            }
        }
    } else {
        // No pending question — treat as a new session request.
        let body = serde_json::json!({
            "prompt": trimmed,
        })
        .to_string();

        match daemon_post(socket, "/sessions", &body).await {
            Ok(resp) => {
                // Extract session ID from response.
                let id = serde_json::from_str::<serde_json::Value>(&resp)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|v| v.as_str()).map(String::from))
                    .unwrap_or_else(|| "?".to_string());
                let short_id = &id[..8.min(id.len())];
                eprintln!("phyl-bridge-signal: started session [{short_id}]");
                let _ =
                    signal_send(cfg, &format!("[{short_id}] Session started.")).await;
            }
            Err(e) => {
                eprintln!("phyl-bridge-signal: failed to start session: {e}");
                let _ =
                    signal_send(cfg, &format!("Failed to start session: {e}")).await;
            }
        }
    }
}

/// Resolve a reply into an answer string. If the reply is a number matching
/// one of the question's options, return the option text. Otherwise return
/// the raw reply.
fn resolve_answer(reply: &str, options: &[String]) -> String {
    if let Ok(n) = reply.parse::<usize>() {
        if n >= 1 && n <= options.len() {
            return options[n - 1].clone();
        }
    }
    reply.to_string()
}

// ---------------------------------------------------------------------------
// Configuration loading
// ---------------------------------------------------------------------------

/// Load the Signal bridge config from `$PHYLACTERY_HOME/config.toml`.
fn load_config() -> Result<(SignalBridgeConfig, String), String> {
    let home = phyl_core::home_dir();
    let config_path = home.join("config.toml");

    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("cannot read {}: {e}", config_path.display()))?;

    let config: Config =
        toml::from_str(&contents).map_err(|e| format!("invalid config.toml: {e}"))?;

    let signal_cfg = config
        .bridge
        .and_then(|b| b.signal)
        .ok_or_else(|| {
            "no [bridge.signal] section in config.toml\n\
             Add:\n  [bridge.signal]\n  phone = \"+1234567890\"\n  \
             owner = \"+0987654321\"\n  signal_cli = \"signal-cli\""
                .to_string()
        })?;

    Ok((signal_cfg, config.daemon.socket))
}

/// Verify that signal-cli is available.
async fn check_signal_cli(cfg: &SignalBridgeConfig) -> Result<(), String> {
    let output = tokio::process::Command::new(&cfg.signal_cli)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("signal-cli not found at '{}': {e}", cfg.signal_cli))?;

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout);
        eprintln!("phyl-bridge-signal: using {}", version.trim());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let (signal_cfg, socket) = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("phyl-bridge-signal: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = check_signal_cli(&signal_cfg).await {
        eprintln!("phyl-bridge-signal: {e}");
        std::process::exit(1);
    }

    eprintln!(
        "phyl-bridge-signal: bridging to {} (owner: {})",
        signal_cfg.phone, signal_cfg.owner
    );

    let state: SharedState = Arc::new(Mutex::new(VecDeque::new()));

    let feed_handle = tokio::spawn(feed_watcher(
        socket.clone(),
        signal_cfg.clone(),
        Arc::clone(&state),
    ));

    let listen_handle = tokio::spawn(signal_listener(
        socket,
        signal_cfg,
        Arc::clone(&state),
    ));

    // Run until either task completes (shouldn't happen) or Ctrl-C.
    tokio::select! {
        _ = feed_handle => {
            eprintln!("phyl-bridge-signal: feed watcher exited unexpectedly");
        }
        _ = listen_handle => {
            eprintln!("phyl-bridge-signal: signal listener exited unexpectedly");
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nphyl-bridge-signal: shutting down");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_answer_numeric_option() {
        let options = vec!["yes".to_string(), "no".to_string(), "edit draft".to_string()];
        assert_eq!(resolve_answer("1", &options), "yes");
        assert_eq!(resolve_answer("2", &options), "no");
        assert_eq!(resolve_answer("3", &options), "edit draft");
    }

    #[test]
    fn test_resolve_answer_numeric_out_of_range() {
        let options = vec!["yes".to_string(), "no".to_string()];
        assert_eq!(resolve_answer("0", &options), "0");
        assert_eq!(resolve_answer("3", &options), "3");
        assert_eq!(resolve_answer("99", &options), "99");
    }

    #[test]
    fn test_resolve_answer_text() {
        let options = vec!["yes".to_string(), "no".to_string()];
        assert_eq!(resolve_answer("maybe", &options), "maybe");
        assert_eq!(resolve_answer("do it", &options), "do it");
    }

    #[test]
    fn test_resolve_answer_no_options() {
        let options: Vec<String> = vec![];
        assert_eq!(resolve_answer("hello", &options), "hello");
        assert_eq!(resolve_answer("1", &options), "1");
    }

    #[test]
    fn test_parse_signal_envelope() {
        let json = r#"{"envelope":{"sourceNumber":"+1234567890","source":"+1234567890","sourceDevice":1,"dataMessage":{"message":"Hello world"}}}"#;
        let env: SignalEnvelope = serde_json::from_str(json).unwrap();
        let inner = env.envelope.unwrap();
        assert_eq!(inner.source_number.unwrap(), "+1234567890");
        assert_eq!(inner.data_message.unwrap().message.unwrap(), "Hello world");
    }

    #[test]
    fn test_parse_signal_envelope_no_message() {
        let json = r#"{"envelope":{"sourceNumber":"+1234567890","sourceDevice":1}}"#;
        let env: SignalEnvelope = serde_json::from_str(json).unwrap();
        let inner = env.envelope.unwrap();
        assert!(inner.data_message.is_none());
    }

    #[test]
    fn test_parse_signal_envelope_empty_message() {
        let json = r#"{"envelope":{"sourceNumber":"+1234567890","sourceDevice":1,"dataMessage":{"message":""}}}"#;
        let env: SignalEnvelope = serde_json::from_str(json).unwrap();
        let inner = env.envelope.unwrap();
        assert_eq!(inner.data_message.unwrap().message.unwrap(), "");
    }

    #[test]
    fn test_parse_signal_envelope_null_envelope() {
        let json = r#"{"envelope":null}"#;
        let env: SignalEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.envelope.is_none());
    }

    #[test]
    fn test_pending_question_tracking() {
        let state: SharedState = Arc::new(Mutex::new(VecDeque::new()));

        // Add a pending question.
        {
            let mut pending = state.lock().unwrap();
            pending.push_back(PendingQuestion {
                session_id: "abc-123".to_string(),
                question_id: "q1".to_string(),
                text: "Send email?".to_string(),
                options: vec!["yes".to_string(), "no".to_string()],
                received_at: Utc::now(),
            });
        }

        // Pop it.
        let pq = {
            let mut pending = state.lock().unwrap();
            pending.pop_front()
        };
        assert!(pq.is_some());
        let pq = pq.unwrap();
        assert_eq!(pq.session_id, "abc-123");
        assert_eq!(pq.question_id, "q1");

        // Queue should now be empty.
        let pq2 = {
            let mut pending = state.lock().unwrap();
            pending.pop_front()
        };
        assert!(pq2.is_none());
    }

    #[test]
    fn test_pending_question_fifo_order() {
        let state: SharedState = Arc::new(Mutex::new(VecDeque::new()));

        {
            let mut pending = state.lock().unwrap();
            pending.push_back(PendingQuestion {
                session_id: "s1".to_string(),
                question_id: "q1".to_string(),
                text: "First".to_string(),
                options: vec![],
                received_at: Utc::now(),
            });
            pending.push_back(PendingQuestion {
                session_id: "s2".to_string(),
                question_id: "q2".to_string(),
                text: "Second".to_string(),
                options: vec![],
                received_at: Utc::now(),
            });
        }

        // First in, first out.
        let pq = state.lock().unwrap().pop_front().unwrap();
        assert_eq!(pq.question_id, "q1");
        let pq = state.lock().unwrap().pop_front().unwrap();
        assert_eq!(pq.question_id, "q2");
    }

    #[test]
    fn test_config_deserialization() {
        let toml_str = r#"
[daemon]
socket = "/tmp/test.sock"

[bridge.signal]
phone = "+1111111111"
owner = "+2222222222"
signal_cli = "/usr/bin/signal-cli"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let bridge = config.bridge.unwrap();
        let signal = bridge.signal.unwrap();
        assert_eq!(signal.phone, "+1111111111");
        assert_eq!(signal.owner, "+2222222222");
        assert_eq!(signal.signal_cli, "/usr/bin/signal-cli");
    }

    #[test]
    fn test_config_deserialization_default_signal_cli() {
        let toml_str = r#"
[bridge.signal]
phone = "+1111111111"
owner = "+2222222222"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let signal = config.bridge.unwrap().signal.unwrap();
        assert_eq!(signal.signal_cli, "signal-cli");
    }

    #[test]
    fn test_config_deserialization_no_bridge() {
        let toml_str = r#"
[daemon]
socket = "/tmp/test.sock"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.bridge.is_none());
    }

    #[test]
    fn test_pending_question_cap() {
        let state: SharedState = Arc::new(Mutex::new(VecDeque::new()));

        {
            let mut pending = state.lock().unwrap();
            for i in 0..60 {
                pending.push_back(PendingQuestion {
                    session_id: format!("s{i}"),
                    question_id: format!("q{i}"),
                    text: format!("Question {i}"),
                    options: vec![],
                    received_at: Utc::now(),
                });
                // Apply the same cap as in process_sse_message.
                while pending.len() > 50 {
                    pending.pop_front();
                }
            }
            assert_eq!(pending.len(), 50);
            // The first remaining should be q10 (0-9 were evicted).
            assert_eq!(pending.front().unwrap().question_id, "q10");
        }
    }
}

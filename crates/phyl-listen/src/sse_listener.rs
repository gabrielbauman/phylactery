//! SSE subscription listener — persistent connections to external event streams.

use crate::daemon_client;
use crate::expand_env;
use crate::rate_limit::{DedupCache, RateLimiter};
use http_body_util::BodyExt;
use phyl_core::ListenSseConfig;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::sleep;

pub async fn run_sse_listener(
    config: &ListenSseConfig,
    socket: &str,
    shutdown: &mut watch::Receiver<bool>,
) {
    let rate_limiter = RateLimiter::new();
    let dedup_cache = DedupCache::new();

    let mut backoff = Duration::from_secs(5);
    let max_backoff = Duration::from_secs(60);
    let mut last_event_id: Option<String> = None;

    loop {
        eprintln!(
            "phyl-listen: [{}] connecting to {}",
            config.name, config.url
        );

        match connect_and_stream(
            config,
            socket,
            &rate_limiter,
            &dedup_cache,
            &last_event_id,
            shutdown,
        )
        .await
        {
            Ok(last_id) => {
                last_event_id = last_id;
                backoff = Duration::from_secs(5);
            }
            Err(e) => {
                eprintln!("phyl-listen: [{}] connection error: {e}", config.name);
            }
        }

        if *shutdown.borrow() {
            eprintln!("phyl-listen: [{}] stopped", config.name);
            return;
        }

        eprintln!(
            "phyl-listen: [{}] reconnecting in {}s",
            config.name,
            backoff.as_secs()
        );

        tokio::select! {
            _ = sleep(backoff) => {}
            _ = shutdown.changed() => {
                eprintln!("phyl-listen: [{}] stopped", config.name);
                return;
            }
        }

        backoff = (backoff * 2).min(max_backoff);
    }
}

async fn connect_and_stream(
    config: &ListenSseConfig,
    socket: &str,
    rate_limiter: &RateLimiter,
    dedup_cache: &DedupCache,
    last_event_id: &Option<String>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Option<String>, String> {
    let parsed_url = config
        .url
        .parse::<hyper::Uri>()
        .map_err(|e| format!("invalid URL: {e}"))?;

    let host = parsed_url.host().ok_or("URL has no host")?.to_string();
    let port = parsed_url
        .port_u16()
        .unwrap_or(match parsed_url.scheme_str() {
            Some("https") => 443,
            _ => 80,
        });

    let stream = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::net::TcpStream::connect(format!("{host}:{port}")),
    )
    .await
    .map_err(|_| "connection timeout".to_string())?
    .map_err(|e| format!("connection failed: {e}"))?;

    let io = hyper_util::rt::TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| format!("handshake failed: {e}"))?;

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let path = parsed_url
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let mut req_builder = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(path)
        .header("Host", &host)
        .header("Accept", "text/event-stream")
        .header("Cache-Control", "no-cache");

    if let Some(id) = last_event_id {
        req_builder = req_builder.header("Last-Event-ID", id.as_str());
    }

    for (k, v) in &config.headers {
        let expanded = expand_env(v);
        req_builder = req_builder.header(k.as_str(), expanded);
    }

    let req = req_builder
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|e| format!("failed to build request: {e}"))?;

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }

    eprintln!("phyl-listen: [{}] connected", config.name);

    let mut body = resp.into_body();

    // SSE parsing state
    let mut current_event_type = String::new();
    let mut current_data = String::new();
    let mut current_id: Option<String> = None;
    let mut last_id: Option<String> = None;
    let mut buffer = String::new();
    const MAX_BUFFER_SIZE: usize = 1_048_576; // 1 MB maximum buffer

    let stale_timeout = Duration::from_secs(300);
    let mut last_activity = tokio::time::Instant::now();

    loop {
        tokio::select! {
            frame_result = body.frame() => {
                match frame_result {
                    Some(Ok(frame)) => {
                        if let Ok(data) = frame.into_data() {
                            last_activity = tokio::time::Instant::now();
                            buffer.push_str(&String::from_utf8_lossy(&data));

                            // Guard against unbounded buffer growth from a
                            // misbehaving server sending data without newlines.
                            if buffer.len() > MAX_BUFFER_SIZE {
                                eprintln!("phyl-listen: [{}] SSE buffer exceeded {} bytes, resetting",
                                    config.name, MAX_BUFFER_SIZE);
                                buffer.clear();
                                current_event_type.clear();
                                current_data.clear();
                                current_id = None;
                                continue;
                            }

                            while let Some(newline_pos) = buffer.find('\n') {
                                let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                                buffer = buffer[newline_pos + 1..].to_string();

                                if line.is_empty() {
                                    if !current_data.is_empty() {
                                        let event_type = if current_event_type.is_empty() {
                                            "message".to_string()
                                        } else {
                                            current_event_type.clone()
                                        };

                                        if let Some(id) = &current_id {
                                            last_id = Some(id.clone());
                                        }

                                        process_sse_event(
                                            config,
                                            socket,
                                            &event_type,
                                            current_data.trim_end_matches('\n'),
                                            &current_id,
                                            rate_limiter,
                                            dedup_cache,
                                        ).await;
                                    }
                                    current_event_type.clear();
                                    current_data.clear();
                                    current_id = None;
                                } else if let Some(field) = line.strip_prefix("event:") {
                                    current_event_type = field.trim().to_string();
                                } else if let Some(field) = line.strip_prefix("data:") {
                                    if !current_data.is_empty() {
                                        current_data.push('\n');
                                    }
                                    current_data.push_str(field.trim_start());
                                } else if let Some(field) = line.strip_prefix("id:") {
                                    current_id = Some(field.trim().to_string());
                                }
                                // Comments (starting with ':') are ignored (keep-alive)
                            }
                        }
                    }
                    Some(Err(e)) => {
                        return Err(format!("stream error: {e}"));
                    }
                    None => {
                        return Ok(last_id);
                    }
                }
            }
            _ = sleep(stale_timeout) => {
                if last_activity.elapsed() > stale_timeout {
                    return Err("connection stale (no activity for 5 minutes)".to_string());
                }
            }
            _ = shutdown.changed() => {
                return Ok(last_id);
            }
        }
    }
}

async fn process_sse_event(
    config: &ListenSseConfig,
    socket: &str,
    event_type: &str,
    data: &str,
    event_id: &Option<String>,
    rate_limiter: &RateLimiter,
    dedup_cache: &DedupCache,
) {
    if !config.events.is_empty() && !config.events.iter().any(|e| e == event_type) {
        return;
    }

    if let Some(id) = event_id
        && dedup_cache.is_duplicate(id)
    {
        return;
    }

    if !rate_limiter.check(&config.name, config.rate_limit) {
        eprintln!(
            "phyl-listen: [{}] rate limited, dropping event",
            config.name
        );
        return;
    }

    let prompt = if config.route_event {
        config
            .routes
            .get(event_type)
            .cloned()
            .unwrap_or_else(|| config.prompt.clone())
    } else {
        config.prompt.clone()
    };

    let ts = chrono::Utc::now().to_rfc3339();
    let id_str = event_id.as_deref().unwrap_or("(none)");
    let session_prompt = format!(
        "{prompt}\n\n=== EVENT ===\nSource: {} ({})\nEvent type: {event_type}\nEvent ID: {id_str}\nReceived: {ts}\n\n=== DATA ===\n{data}",
        config.name, config.url
    );

    match daemon_client::create_session(socket, &session_prompt).await {
        Ok(id) => {
            eprintln!("phyl-listen: [{}] session created: {id}", config.name);
        }
        Err(e) => {
            eprintln!(
                "phyl-listen: [{}] failed to create session: {e}",
                config.name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_sse_config_deserialize() {
        let toml_str = r#"
            [listen]
            bind = "127.0.0.1:7890"

            [[listen.sse]]
            name = "deploys"
            url = "https://example.com/events"
            prompt = "A deploy event occurred."
            route_event = true
            events = ["deploy_start", "deploy_fail"]

            [listen.sse.routes]
            deploy_fail = "A deployment failed. Investigate."

            [listen.sse.headers]
            Authorization = "Bearer $TOKEN"
        "#;
        let config: phyl_core::Config = toml::from_str(toml_str).unwrap();
        let listen = config.listen.unwrap();
        assert_eq!(listen.sse.len(), 1);
        let sse = &listen.sse[0];
        assert_eq!(sse.name, "deploys");
        assert!(sse.route_event);
        assert_eq!(sse.events.len(), 2);
        assert_eq!(sse.routes.len(), 1);
        assert_eq!(sse.headers.len(), 1);
    }

    #[test]
    fn test_sse_config_defaults() {
        let toml_str = r#"
            [listen]

            [[listen.sse]]
            name = "test"
            url = "https://example.com/events"
            prompt = "An event occurred."
        "#;
        let config: phyl_core::Config = toml::from_str(toml_str).unwrap();
        let listen = config.listen.unwrap();
        let sse = &listen.sse[0];
        assert!(!sse.route_event);
        assert!(sse.events.is_empty());
        assert!(sse.routes.is_empty());
        assert!(sse.headers.is_empty());
        assert_eq!(sse.rate_limit, 10);
    }
}

//! Webhook listener — HTTP POST handler with HMAC verification,
//! rate limiting, deduplication, and event-type routing.

use crate::daemon_client;
use crate::expand_env;
use crate::rate_limit::{DedupCache, RateLimiter};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use hmac::{Hmac, Mac};
use phyl_core::ListenHookConfig;
use sha2::Sha256;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;

type HmacSha256 = Hmac<Sha256>;

struct WebhookState {
    hooks: Vec<ListenHookConfig>,
    socket: String,
    rate_limiter: RateLimiter,
    dedup_cache: DedupCache,
}

pub async fn run_webhook_server(
    bind: &str,
    hooks: Vec<ListenHookConfig>,
    socket: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let state = Arc::new(WebhookState {
        hooks,
        socket: socket.to_string(),
        rate_limiter: RateLimiter::new(),
        dedup_cache: DedupCache::new(),
    });

    let app = Router::new()
        .fallback(post(handle_webhook).get(handle_not_found))
        .with_state(state);

    let listener = TcpListener::bind(bind)
        .await
        .map_err(|e| format!("failed to bind {bind}: {e}"))?;

    eprintln!("phyl-listen: webhook server listening on {bind}");

    let mut rx = shutdown.clone();
    tokio::select! {
        result = axum::serve(listener, app) => {
            result.map_err(|e| format!("webhook server error: {e}"))?;
        }
        _ = rx.changed() => {
            eprintln!("phyl-listen: webhook server stopped");
        }
    }

    Ok(())
}

async fn handle_not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "Not Found")
}

async fn handle_webhook(
    State(state): State<Arc<WebhookState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Extract the request path from headers (axum provides it)
    let uri_path = headers
        .get("x-original-uri")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/");

    // Find hooks matching this path (exact match only to prevent prefix attacks).
    let matching_hooks: Vec<&ListenHookConfig> = state
        .hooks
        .iter()
        .filter(|h| uri_path == h.path || uri_path.strip_suffix('/') == Some(&h.path))
        .collect();

    if matching_hooks.is_empty() {
        return (StatusCode::NOT_FOUND, "No hook configured for this path".to_string());
    }

    // Check body size against the first hook's limit
    if let Some(hook) = matching_hooks.first() {
        if body.len() > hook.max_body_size {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload too large".to_string(),
            );
        }
    }

    // Verify webhook secret (shared for hooks on the same path)
    for hook in &matching_hooks {
        if let Some(secret) = &hook.secret {
            let secret_value = expand_env(secret);
            if !verify_signature(&headers, &body, &secret_value) {
                return (StatusCode::UNAUTHORIZED, "Invalid signature".to_string());
            }
            break; // Only verify once per path
        }
    }

    // Find the first matching hook (filter by header if configured)
    let matched_hook = matching_hooks
        .iter()
        .find(|h| {
            if let Some(filter_header) = &h.filter_header {
                if let Some(header_value) = headers.get(filter_header.as_str()) {
                    let val = header_value.to_str().unwrap_or("");
                    if h.filter_values.is_empty() {
                        return true;
                    }
                    return h.filter_values.iter().any(|fv| fv == val);
                }
                return false;
            }
            true // No filter — matches unconditionally
        })
        .or_else(|| matching_hooks.last()); // Fall back to last (catchall)

    let hook = match matched_hook {
        Some(h) => *h,
        None => {
            return (StatusCode::NOT_FOUND, "No matching hook".to_string());
        }
    };

    // Rate limiting
    if !state.rate_limiter.check(&hook.name, hook.rate_limit) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded".to_string(),
        );
    }

    // Deduplication
    let dedup_id = headers
        .get(hook.dedup_header.as_str())
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !dedup_id.is_empty() && state.dedup_cache.is_duplicate(dedup_id) {
        return (StatusCode::OK, "Duplicate delivery, skipped".to_string());
    }

    // Resolve prompt (event-type routing)
    let prompt = resolve_prompt(hook, &headers);

    // Assemble session prompt
    let body_str = String::from_utf8_lossy(&body);
    let header_summary = format_relevant_headers(&headers);
    let ts = chrono::Utc::now().to_rfc3339();
    let session_prompt = format!(
        "{prompt}\n\n=== EVENT ===\nSource: {} (POST {})\nReceived: {ts}\n{header_summary}\n\n=== PAYLOAD ===\n{body_str}",
        hook.name, hook.path
    );

    // Create session
    match daemon_client::create_session(&state.socket, &session_prompt).await {
        Ok(id) => {
            eprintln!("phyl-listen: [{}] session created: {id}", hook.name);
            (StatusCode::ACCEPTED, format!("{{\"session_id\":\"{id}\"}}"))
        }
        Err(e) => {
            eprintln!("phyl-listen: [{}] failed to create session: {e}", hook.name);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create session: {e}"),
            )
        }
    }
}

fn resolve_prompt(hook: &ListenHookConfig, headers: &HeaderMap) -> String {
    if let Some(route_header) = &hook.route_header {
        if let Some(header_value) = headers.get(route_header.as_str()) {
            let val = header_value.to_str().unwrap_or("");
            if let Some(routed_prompt) = hook.routes.get(val) {
                return routed_prompt.clone();
            }
        }
    }
    hook.prompt.clone()
}

fn verify_signature(headers: &HeaderMap, body: &[u8], secret: &str) -> bool {
    // GitHub-style: X-Hub-Signature-256 = sha256=<hex>
    if let Some(sig_header) = headers.get("X-Hub-Signature-256") {
        let sig_str = sig_header.to_str().unwrap_or("");
        if let Some(hex_sig) = sig_str.strip_prefix("sha256=") {
            let mut mac =
                HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC key creation");
            mac.update(body);
            if let Ok(expected) = hex::decode(hex_sig) {
                return mac.verify_slice(&expected).is_ok();
            }
        }
        return false;
    }

    // GitLab-style: X-Gitlab-Token = <token> (direct comparison)
    if let Some(token_header) = headers.get("X-Gitlab-Token") {
        return token_header.to_str().unwrap_or("") == secret;
    }

    // If no recognized signature header is present, reject
    false
}

fn format_relevant_headers(headers: &HeaderMap) -> String {
    let relevant = [
        "X-GitHub-Event",
        "X-GitHub-Delivery",
        "X-GitLab-Event",
        "X-Gitlab-Token",
        "Content-Type",
        "X-Request-Id",
    ];

    let mut lines = vec!["Headers:".to_string()];
    for name in &relevant {
        if let Some(value) = headers.get(*name) {
            if let Ok(v) = value.to_str() {
                lines.push(format!("  {name}: {v}"));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature_github_valid() {
        let secret = "mysecret";
        let body = b"hello world";

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize();
        let hex_sig = hex::encode(result.into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Hub-Signature-256",
            format!("sha256={hex_sig}").parse().unwrap(),
        );

        assert!(verify_signature(&headers, body, secret));
    }

    #[test]
    fn test_verify_signature_github_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Hub-Signature-256",
            "sha256=0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
        );

        assert!(!verify_signature(&headers, b"hello", "secret"));
    }

    #[test]
    fn test_verify_signature_gitlab_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", "mytoken".parse().unwrap());

        assert!(verify_signature(&headers, b"body", "mytoken"));
    }

    #[test]
    fn test_verify_signature_gitlab_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", "wrong".parse().unwrap());

        assert!(!verify_signature(&headers, b"body", "mytoken"));
    }

    #[test]
    fn test_verify_signature_no_header() {
        let headers = HeaderMap::new();
        assert!(!verify_signature(&headers, b"body", "secret"));
    }

    #[test]
    fn test_resolve_prompt_with_route() {
        let hook = ListenHookConfig {
            name: "test".to_string(),
            path: "/hook/test".to_string(),
            prompt: "fallback".to_string(),
            secret: None,
            filter_header: None,
            filter_values: vec![],
            rate_limit: 10,
            dedup_header: "X-Request-Id".to_string(),
            max_body_size: 1_048_576,
            route_header: Some("X-Event-Type".to_string()),
            routes: [("push".to_string(), "Code pushed".to_string())]
                .into_iter()
                .collect(),
        };

        let mut headers = HeaderMap::new();
        headers.insert("X-Event-Type", "push".parse().unwrap());
        assert_eq!(resolve_prompt(&hook, &headers), "Code pushed");
    }

    #[test]
    fn test_resolve_prompt_fallback() {
        let hook = ListenHookConfig {
            name: "test".to_string(),
            path: "/hook/test".to_string(),
            prompt: "fallback".to_string(),
            secret: None,
            filter_header: None,
            filter_values: vec![],
            rate_limit: 10,
            dedup_header: "X-Request-Id".to_string(),
            max_body_size: 1_048_576,
            route_header: Some("X-Event-Type".to_string()),
            routes: [("push".to_string(), "Code pushed".to_string())]
                .into_iter()
                .collect(),
        };

        let mut headers = HeaderMap::new();
        headers.insert("X-Event-Type", "unknown".parse().unwrap());
        assert_eq!(resolve_prompt(&hook, &headers), "fallback");
    }

    #[test]
    fn test_resolve_prompt_no_route_header() {
        let hook = ListenHookConfig {
            name: "test".to_string(),
            path: "/hook/test".to_string(),
            prompt: "default prompt".to_string(),
            secret: None,
            filter_header: None,
            filter_values: vec![],
            rate_limit: 10,
            dedup_header: "X-Request-Id".to_string(),
            max_body_size: 1_048_576,
            route_header: None,
            routes: std::collections::HashMap::new(),
        };

        let headers = HeaderMap::new();
        assert_eq!(resolve_prompt(&hook, &headers), "default prompt");
    }

    #[test]
    fn test_listen_hook_config_deserialize() {
        let toml_str = r#"
            [listen]
            bind = "127.0.0.1:7890"

            [[listen.hook]]
            name = "github"
            path = "/hook/github"
            prompt = "A GitHub event arrived."
            secret = "$GITHUB_SECRET"
            route_header = "X-GitHub-Event"

            [listen.hook.routes]
            push = "Code pushed"
            pull_request = "PR opened"
        "#;
        let config: phyl_core::Config = toml::from_str(toml_str).unwrap();
        let listen = config.listen.unwrap();
        assert_eq!(listen.bind, "127.0.0.1:7890");
        assert_eq!(listen.hook.len(), 1);
        assert_eq!(listen.hook[0].name, "github");
        assert_eq!(listen.hook[0].routes.len(), 2);
        assert_eq!(listen.hook[0].rate_limit, 10); // default
    }
}

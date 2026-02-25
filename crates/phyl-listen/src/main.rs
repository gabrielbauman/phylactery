//! `phyl-listen` — Incoming event listener. Three listener types:
//! - Webhooks (`[[listen.hook]]`) — HTTP POST on a TCP port, HMAC-SHA256 verification
//! - SSE subscriptions (`[[listen.sse]]`) — persistent connections to event streams
//! - File watches (`[[listen.watch]]`) — cross-platform (notify), glob filtering, debouncing
//!
//! All create sessions via the daemon API.

mod daemon_client;
mod file_watch;
mod rate_limit;
mod sse_listener;
mod webhook;

use phyl_core::Config;
use std::path::Path;
use tokio::sync::watch;

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run()) {
        eprintln!("phyl-listen: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let home = phyl_core::home_dir();
    if !home.exists() {
        return Err(format!(
            "{} does not exist. Run `phyl init` first.",
            home.display()
        ));
    }

    // Load secrets.env if it exists
    let secrets_path = home.join("secrets.env");
    if secrets_path.exists() {
        load_secrets_env(&secrets_path);
    }

    let config = load_config(&home)?;
    let listen = match config.listen {
        Some(l) => l,
        None => {
            eprintln!("phyl-listen: no [listen] config found in config.toml");
            return Ok(());
        }
    };

    let socket = config.daemon.socket.clone();

    let has_hooks = !listen.hook.is_empty();
    let has_sse = !listen.sse.is_empty();
    let has_watches = !listen.watch.is_empty();

    if !has_hooks && !has_sse && !has_watches {
        eprintln!("phyl-listen: no listeners configured");
        return Ok(());
    }

    eprintln!(
        "phyl-listen: {} webhook(s), {} SSE subscription(s), {} file watch(es)",
        listen.hook.len(),
        listen.sse.len(),
        listen.watch.len()
    );

    // Shutdown signal
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut handles = Vec::new();

    // Start webhook HTTP server if hooks are configured
    if has_hooks {
        let bind = listen.bind.clone();
        let hooks = listen.hook.clone();
        let sock = socket.clone();
        let mut rx = shutdown_rx.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = webhook::run_webhook_server(&bind, hooks, &sock, &mut rx).await {
                eprintln!("phyl-listen: webhook server error: {e}");
            }
        });
        handles.push(handle);
    }

    // Start SSE subscription listeners
    for sse_config in &listen.sse {
        let cfg = sse_config.clone();
        let sock = socket.clone();
        let mut rx = shutdown_rx.clone();

        let handle = tokio::spawn(async move {
            sse_listener::run_sse_listener(&cfg, &sock, &mut rx).await;
        });
        handles.push(handle);
    }

    // Start file watch listeners
    if has_watches {
        let watches = listen.watch.clone();
        let sock = socket.clone();
        let mut rx = shutdown_rx.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = file_watch::run_file_watches(watches, &sock, &mut rx).await {
                eprintln!("phyl-listen: file watch error: {e}");
            }
        });
        handles.push(handle);
    }

    // Wait for Ctrl-C
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| format!("signal handler failed: {e}"))?;
    eprintln!("phyl-listen: shutting down...");
    let _ = shutdown_tx.send(true);

    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

fn load_config(home: &Path) -> Result<Config, String> {
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read config.toml: {e}"))?;
    toml::from_str(&contents).map_err(|e| format!("failed to parse config.toml: {e}"))
}

fn load_secrets_env(path: &Path) {
    if let Ok(contents) = std::fs::read_to_string(path) {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                if !key.is_empty() {
                    // SAFETY: We load secrets at startup before spawning threads.
                    unsafe {
                        std::env::set_var(key, value);
                    }
                }
            }
        }
    }
}

/// Expand `$VAR` and `${VAR}` references from process environment.
pub fn expand_env(s: &str) -> String {
    let mut result = s.to_string();
    // Handle ${VAR} syntax
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let value = std::env::var(var_name).unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..start],
                value,
                &result[start + end + 1..]
            );
        } else {
            break;
        }
    }
    // Handle $VAR syntax
    let mut i = 0;
    let bytes = result.as_bytes();
    let mut out = String::new();
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] != b'{' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                let var_name = &result[start..end];
                let value = std::env::var(var_name).unwrap_or_default();
                out.push_str(&value);
                i = end;
            } else {
                out.push('$');
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

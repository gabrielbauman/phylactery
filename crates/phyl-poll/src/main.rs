//! `phyl-poll` — Poller. Runs commands on configurable intervals,
//! compares output to previous results, and starts sessions via the daemon
//! API when changes are detected.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use phyl_core::{Config, PollConfig};
use std::path::PathBuf;
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::time::{Duration, sleep};

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run()) {
        eprintln!("phyl-poll: {e}");
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
    let poll_rules = config.poll;

    if poll_rules.is_empty() {
        eprintln!("phyl-poll: no [[poll]] rules configured in config.toml");
        return Ok(());
    }

    let socket = config.daemon.socket.clone();
    let poll_dir = home.join("poll");
    std::fs::create_dir_all(&poll_dir)
        .map_err(|e| format!("failed to create poll dir: {e}"))?;

    eprintln!(
        "phyl-poll: starting with {} poll rule(s)",
        poll_rules.len()
    );

    // Shutdown signal
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Spawn a task per poll rule, staggered
    let mut handles = Vec::new();
    for (i, rule) in poll_rules.into_iter().enumerate() {
        let poll_dir = poll_dir.clone();
        let socket = socket.clone();
        let mut rx = shutdown_rx.clone();
        let stagger = Duration::from_millis(100 * i as u64);

        let handle = tokio::spawn(async move {
            sleep(stagger).await;
            run_poll_rule(&rule, &poll_dir, &socket, &mut rx).await;
        });
        handles.push(handle);
    }

    // Wait for Ctrl-C
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| format!("signal handler failed: {e}"))?;
    eprintln!("phyl-poll: shutting down...");
    let _ = shutdown_tx.send(true);

    // Wait for all tasks to complete
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

/// Run a single poll rule on its configured interval.
async fn run_poll_rule(
    rule: &PollConfig,
    poll_dir: &PathBuf,
    socket: &str,
    shutdown: &mut watch::Receiver<bool>,
) {
    let interval_secs = rule.interval.max(10); // minimum 10 seconds
    let interval = Duration::from_secs(interval_secs);
    let last_file = poll_dir.join(format!("{}.last", rule.name));

    eprintln!(
        "phyl-poll: [{}] polling every {}s: {} {}",
        rule.name,
        interval_secs,
        rule.command,
        rule.args.join(" ")
    );

    loop {
        // Run the command
        match run_command(rule).await {
            Ok(output) => {
                // Read previous output
                let previous = std::fs::read_to_string(&last_file).ok();

                match previous {
                    None => {
                        // First run: establish baseline
                        eprintln!("phyl-poll: [{}] baseline established", rule.name);
                        if let Err(e) = std::fs::write(&last_file, &output) {
                            eprintln!("phyl-poll: [{}] failed to write state: {e}", rule.name);
                        }
                    }
                    Some(prev) if prev == output => {
                        // No change
                    }
                    Some(prev) => {
                        // Output changed — create a session
                        eprintln!("phyl-poll: [{}] change detected, creating session", rule.name);
                        let prompt = assemble_prompt(&rule.prompt, &prev, &output);
                        match create_session(socket, &prompt).await {
                            Ok(id) => {
                                eprintln!("phyl-poll: [{}] session created: {id}", rule.name);
                            }
                            Err(e) => {
                                eprintln!("phyl-poll: [{}] failed to create session: {e}", rule.name);
                            }
                        }
                        if let Err(e) = std::fs::write(&last_file, &output) {
                            eprintln!("phyl-poll: [{}] failed to write state: {e}", rule.name);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("phyl-poll: [{}] command failed: {e}", rule.name);
                // Don't update .last on failure — avoids spurious sessions
            }
        }

        // Sleep until next interval or shutdown
        tokio::select! {
            _ = sleep(interval) => {}
            _ = shutdown.changed() => {
                eprintln!("phyl-poll: [{}] stopped", rule.name);
                return;
            }
        }
    }
}

/// Run a poll command and capture stdout.
async fn run_command(rule: &PollConfig) -> Result<String, String> {
    let timeout = Duration::from_secs(rule.timeout);

    let result = tokio::time::timeout(timeout, async {
        let mut cmd = if rule.shell {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(&rule.command);
            c
        } else {
            let mut c = tokio::process::Command::new(&rule.command);
            c.args(&rule.args);
            c
        };

        // Set environment variables
        for (k, v) in &rule.env {
            let expanded = expand_env_var(v);
            cmd.env(k, expanded);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("failed to execute: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "exited with status {}",
                output.status.code().unwrap_or(-1)
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => Err(format!(
            "timed out after {}s",
            rule.timeout
        )),
    }
}

/// Assemble the session prompt from the rule's prompt template plus diff context.
fn assemble_prompt(prompt: &str, previous: &str, current: &str) -> String {
    let diff = generate_diff(previous, current);
    format!(
        "{prompt}\n\n=== PREVIOUS OUTPUT ===\n{previous}\n\n=== CURRENT OUTPUT ===\n{current}\n\n=== DIFF ===\n{diff}"
    )
}

/// Generate a simple unified-style diff between two strings.
fn generate_diff(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut diff = String::new();
    // Simple line-by-line comparison (not a full diff algorithm, but functional)
    let max_len = old_lines.len().max(new_lines.len());
    for i in 0..max_len {
        match (old_lines.get(i), new_lines.get(i)) {
            (Some(o), Some(n)) if o == n => {
                diff.push_str(&format!(" {o}\n"));
            }
            (Some(o), Some(n)) => {
                diff.push_str(&format!("-{o}\n"));
                diff.push_str(&format!("+{n}\n"));
            }
            (Some(o), None) => {
                diff.push_str(&format!("-{o}\n"));
            }
            (None, Some(n)) => {
                diff.push_str(&format!("+{n}\n"));
            }
            (None, None) => {}
        }
    }
    diff
}

/// Expand `$VAR` references in a string from the process environment.
fn expand_env_var(s: &str) -> String {
    let mut result = s.to_string();
    // Handle ${VAR} syntax
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let value = std::env::var(var_name).unwrap_or_default();
            result = format!("{}{}{}", &result[..start], value, &result[start + end + 1..]);
        } else {
            break;
        }
    }
    // Handle $VAR syntax (only if not already part of ${})
    let mut i = 0;
    let bytes = result.as_bytes();
    let mut out = String::new();
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] != b'{' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
            {
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
            out.push(result.as_bytes()[i] as char);
            i += 1;
        }
    }
    out
}

/// Load config.toml from the agent home.
fn load_config(home: &std::path::Path) -> Result<Config, String> {
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read config.toml: {e}"))?;
    toml::from_str(&contents).map_err(|e| format!("failed to parse config.toml: {e}"))
}

/// Load secrets.env file, exporting key=value pairs to the process environment.
fn load_secrets_env(path: &std::path::Path) {
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
                    unsafe { std::env::set_var(key, value); }
                }
            }
        }
    }
}

/// Create a session via the daemon API.
async fn create_session(socket: &str, prompt: &str) -> Result<String, String> {
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
        // Try to extract session ID from response
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_identical() {
        let diff = generate_diff("hello\nworld", "hello\nworld");
        assert!(diff.contains(" hello"));
        assert!(diff.contains(" world"));
        assert!(!diff.contains('+'));
        assert!(!diff.contains('-'));
    }

    #[test]
    fn test_generate_diff_changes() {
        let diff = generate_diff("hello\nworld", "hello\nearth");
        assert!(diff.contains(" hello"));
        assert!(diff.contains("-world"));
        assert!(diff.contains("+earth"));
    }

    #[test]
    fn test_generate_diff_additions() {
        let diff = generate_diff("hello", "hello\nworld");
        assert!(diff.contains(" hello"));
        assert!(diff.contains("+world"));
    }

    #[test]
    fn test_generate_diff_deletions() {
        let diff = generate_diff("hello\nworld", "hello");
        assert!(diff.contains(" hello"));
        assert!(diff.contains("-world"));
    }

    #[test]
    fn test_assemble_prompt() {
        let prompt = assemble_prompt("Check this", "old output", "new output");
        assert!(prompt.starts_with("Check this"));
        assert!(prompt.contains("=== PREVIOUS OUTPUT ==="));
        assert!(prompt.contains("old output"));
        assert!(prompt.contains("=== CURRENT OUTPUT ==="));
        assert!(prompt.contains("new output"));
        assert!(prompt.contains("=== DIFF ==="));
    }

    #[test]
    fn test_expand_env_var_simple() {
        unsafe { std::env::set_var("PHYL_TEST_VAR", "hello"); }
        assert_eq!(expand_env_var("$PHYL_TEST_VAR"), "hello");
        unsafe { std::env::remove_var("PHYL_TEST_VAR"); }
    }

    #[test]
    fn test_expand_env_var_braces() {
        unsafe { std::env::set_var("PHYL_TEST_VAR2", "world"); }
        assert_eq!(expand_env_var("${PHYL_TEST_VAR2}"), "world");
        unsafe { std::env::remove_var("PHYL_TEST_VAR2"); }
    }

    #[test]
    fn test_expand_env_var_missing() {
        assert_eq!(
            expand_env_var("$PHYL_NONEXISTENT_VAR_12345"),
            ""
        );
    }

    #[test]
    fn test_expand_env_var_mixed() {
        unsafe { std::env::set_var("PHYL_MIX_A", "foo"); }
        unsafe { std::env::set_var("PHYL_MIX_B", "bar"); }
        assert_eq!(
            expand_env_var("prefix_${PHYL_MIX_A}_$PHYL_MIX_B_suffix"),
            // PHYL_MIX_B_suffix doesn't exist, so it expands to empty
            "prefix_foo_"
        );
        unsafe { std::env::remove_var("PHYL_MIX_A"); }
        unsafe { std::env::remove_var("PHYL_MIX_B"); }
    }

    #[test]
    fn test_poll_config_deserialize() {
        let toml_str = r#"
            [[poll]]
            name = "test"
            command = "echo"
            args = ["hello"]
            interval = 60
            prompt = "Test prompt"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.poll.len(), 1);
        assert_eq!(config.poll[0].name, "test");
        assert_eq!(config.poll[0].command, "echo");
        assert_eq!(config.poll[0].interval, 60);
        assert_eq!(config.poll[0].timeout, 30); // default
    }

    #[test]
    fn test_poll_config_defaults() {
        let toml_str = r#"
            [[poll]]
            name = "test"
            command = "echo"
            prompt = "Test"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.poll[0].interval, 300); // default
        assert_eq!(config.poll[0].timeout, 30); // default
        assert!(!config.poll[0].shell);
        assert!(config.poll[0].args.is_empty());
        assert!(config.poll[0].env.is_empty());
    }

    #[test]
    fn test_poll_config_shell_mode() {
        let toml_str = r#"
            [[poll]]
            name = "test"
            command = "echo hello | grep hello"
            shell = true
            prompt = "Test"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.poll[0].shell);
    }

    #[test]
    fn test_poll_config_with_env() {
        let toml_str = r#"
            [[poll]]
            name = "test"
            command = "curl"
            args = ["-sf", "http://example.com"]
            interval = 60
            prompt = "Check health"
            timeout = 10

            [poll.env]
            API_KEY = "$MY_KEY"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.poll[0].timeout, 10);
        assert_eq!(config.poll[0].env.get("API_KEY").unwrap(), "$MY_KEY");
    }

    #[test]
    fn test_load_secrets_env() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("phyl-poll-test-secrets");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_secrets.env");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "PHYL_TEST_SECRET_A=value_a").unwrap();
        writeln!(f, "PHYL_TEST_SECRET_B = value_b").unwrap();
        drop(f);

        load_secrets_env(&path);
        assert_eq!(std::env::var("PHYL_TEST_SECRET_A").unwrap(), "value_a");
        assert_eq!(std::env::var("PHYL_TEST_SECRET_B").unwrap(), "value_b");

        // Cleanup
        unsafe { std::env::remove_var("PHYL_TEST_SECRET_A"); }
        unsafe { std::env::remove_var("PHYL_TEST_SECRET_B"); }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

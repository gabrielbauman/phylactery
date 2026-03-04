//! `phyl-tool-self` — Server-mode tool providing agent self-direction capabilities.
//!
//! Tools:
//! - `spawn_session` — Create a new session immediately or at a scheduled future time
//! - `sleep_until` — End current session and schedule a wake-up session for later
//! - `list_scheduled` — View all pending scheduled entries
//! - `cancel_scheduled` — Cancel a pending scheduled entry by ID

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use phyl_core::{
    Config, ScheduleEntry, ServerRequest, ServerResponse, ToolMode, ToolSpec, parse_time_spec,
};
use std::io::{self, BufRead, Write};
use tokio::net::UnixStream;
use uuid::Uuid;

/// Maximum number of pending schedule entries allowed.
const MAX_SCHEDULED: usize = 50;

fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "spawn_session".to_string(),
            description: format!(
                "Create a new session. If 'at' is omitted the session starts \
                 immediately. If 'at' is provided (ISO 8601 datetime or relative \
                 interval like '30s', '5m', '2h', '1d', '1w'), the session is \
                 scheduled for that time. Maximum {MAX_SCHEDULED} scheduled entries allowed."
            ),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The prompt / task description for the new session"
                    },
                    "at": {
                        "type": "string",
                        "description": "When to start the session: ISO 8601 datetime or relative interval (e.g. '30s', '5m', '2h', '1d', '1w'). Omit for immediate."
                    }
                },
                "required": ["prompt"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "sleep_until".to_string(),
            description: format!(
                "End the current session and schedule a new session to start at \
                 the specified time. Use this to defer work to a future point. \
                 Maximum {MAX_SCHEDULED} scheduled entries allowed."
            ),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The prompt / task description for the wake-up session"
                    },
                    "at": {
                        "type": "string",
                        "description": "When to wake up: ISO 8601 datetime or relative interval (e.g. '30s', '5m', '2h', '1d', '1w')"
                    }
                },
                "required": ["prompt", "at"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "list_scheduled".to_string(),
            description: format!(
                "List all pending scheduled session entries, sorted by time. \
                 Shows current count and the maximum limit ({MAX_SCHEDULED})."
            ),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "cancel_scheduled".to_string(),
            description: "Cancel a pending scheduled entry by its ID.".to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The UUID of the scheduled entry to cancel"
                    }
                },
                "required": ["id"]
            }),
            sandbox: None,
        },
    ]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--spec") {
        let specs = tool_specs();
        println!(
            "{}",
            serde_json::to_string_pretty(&specs).expect("failed to serialize specs")
        );
        return;
    }

    if args.iter().any(|a| a == "--serve") {
        serve();
        return;
    }

    eprintln!("phyl-tool-self: use --spec or --serve");
    std::process::exit(1);
}

fn serve() {
    // Build a tokio runtime for async daemon API calls
    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    let home = phyl_core::home_dir();
    let schedule_dir = home.join("schedule");

    // Load config once for daemon socket path
    let socket = match load_socket_path(&home) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("phyl-tool-self: failed to load config: {e}");
            std::process::exit(1);
        }
    };

    // Session ID (for created_by field)
    let session_id = std::env::var("PHYLACTERY_SESSION_ID").ok();

    let stdin = io::stdin();
    let stdout = io::stdout();

    let reader = stdin.lock();
    let mut writer = stdout.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("phyl-tool-self: stdin read error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: ServerRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(_) => {
                eprintln!("phyl-tool-self: ignoring unrecognized input: {trimmed}");
                continue;
            }
        };

        let response = match req.name.as_str() {
            "spawn_session" => {
                handle_spawn_session(&rt, &req, &schedule_dir, &socket, session_id.as_deref())
            }
            "sleep_until" => handle_sleep_until(&req, &schedule_dir, session_id.as_deref()),
            "list_scheduled" => handle_list_scheduled(&req, &schedule_dir),
            "cancel_scheduled" => handle_cancel_scheduled(&req, &schedule_dir),
            other => ServerResponse {
                id: req.id,
                output: None,
                error: Some(format!("unknown tool: {other}")),
                signal: None,
            },
        };

        write_response(&mut writer, &response);

        // If sleep_until responded with end_session, exit
        if response.signal.as_deref() == Some("end_session") {
            return;
        }
    }

    eprintln!("phyl-tool-self: stdin closed, exiting");
}

fn handle_spawn_session(
    rt: &tokio::runtime::Runtime,
    req: &ServerRequest,
    schedule_dir: &std::path::Path,
    socket: &str,
    session_id: Option<&str>,
) -> ServerResponse {
    let prompt = match req.arguments.get("prompt").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some("missing required parameter: prompt".to_string()),
                signal: None,
            };
        }
    };

    let at = req.arguments.get("at").and_then(|v| v.as_str());

    match at {
        None => {
            // Immediate: create session via daemon API
            match rt.block_on(create_session(socket, prompt)) {
                Ok(id) => ServerResponse {
                    id: req.id.clone(),
                    output: Some(format!("Session created: {id}")),
                    error: None,
                    signal: None,
                },
                Err(e) => ServerResponse {
                    id: req.id.clone(),
                    output: None,
                    error: Some(format!("failed to create session: {e}")),
                    signal: None,
                },
            }
        }
        Some(at_str) => {
            // Scheduled: write schedule entry
            match parse_time_spec(at_str) {
                Ok(dt) => match write_schedule_entry(schedule_dir, prompt, dt, session_id) {
                    Ok(entry_id) => ServerResponse {
                        id: req.id.clone(),
                        output: Some(format!("Scheduled session {entry_id} for {dt}")),
                        error: None,
                        signal: None,
                    },
                    Err(e) => ServerResponse {
                        id: req.id.clone(),
                        output: None,
                        error: Some(format!("failed to write schedule entry: {e}")),
                        signal: None,
                    },
                },
                Err(e) => ServerResponse {
                    id: req.id.clone(),
                    output: None,
                    error: Some(format!("invalid time spec: {e}")),
                    signal: None,
                },
            }
        }
    }
}

fn handle_sleep_until(
    req: &ServerRequest,
    schedule_dir: &std::path::Path,
    session_id: Option<&str>,
) -> ServerResponse {
    let prompt = match req.arguments.get("prompt").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some("missing required parameter: prompt".to_string()),
                signal: None,
            };
        }
    };

    let at_str = match req.arguments.get("at").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => {
            return ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some("missing required parameter: at".to_string()),
                signal: None,
            };
        }
    };

    match parse_time_spec(at_str) {
        Ok(dt) => match write_schedule_entry(schedule_dir, prompt, dt, session_id) {
            Ok(entry_id) => ServerResponse {
                id: req.id.clone(),
                output: Some(format!(
                    "Sleeping. Scheduled wake-up session {entry_id} for {dt}"
                )),
                error: None,
                signal: Some("end_session".to_string()),
            },
            Err(e) => ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some(format!("failed to write schedule entry: {e}")),
                signal: None,
            },
        },
        Err(e) => ServerResponse {
            id: req.id.clone(),
            output: None,
            error: Some(format!("invalid time spec: {e}")),
            signal: None,
        },
    }
}

fn handle_list_scheduled(req: &ServerRequest, schedule_dir: &std::path::Path) -> ServerResponse {
    match list_schedule_entries(schedule_dir) {
        Ok(entries) => {
            let count = entries.len();
            let wrapper = serde_json::json!({
                "count": count,
                "limit": MAX_SCHEDULED,
                "entries": entries,
            });
            let json = serde_json::to_string_pretty(&wrapper).unwrap_or_else(|_| "{}".to_string());
            ServerResponse {
                id: req.id.clone(),
                output: Some(json),
                error: None,
                signal: None,
            }
        }
        Err(e) => ServerResponse {
            id: req.id.clone(),
            output: None,
            error: Some(format!("failed to list schedule entries: {e}")),
            signal: None,
        },
    }
}

fn handle_cancel_scheduled(req: &ServerRequest, schedule_dir: &std::path::Path) -> ServerResponse {
    let id_str = match req.arguments.get("id").and_then(|v| v.as_str()) {
        Some(i) => i,
        None => {
            return ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some("missing required parameter: id".to_string()),
                signal: None,
            };
        }
    };

    let path = schedule_dir.join(format!("{id_str}.json"));
    if !path.exists() {
        return ServerResponse {
            id: req.id.clone(),
            output: None,
            error: Some(format!("schedule entry not found: {id_str}")),
            signal: None,
        };
    }

    match std::fs::remove_file(&path) {
        Ok(()) => ServerResponse {
            id: req.id.clone(),
            output: Some(format!("Cancelled scheduled entry {id_str}")),
            error: None,
            signal: None,
        },
        Err(e) => ServerResponse {
            id: req.id.clone(),
            output: None,
            error: Some(format!("failed to delete schedule entry: {e}")),
            signal: None,
        },
    }
}

// --- Schedule file helpers ---

fn count_schedule_entries(schedule_dir: &std::path::Path) -> usize {
    let Ok(dir) = std::fs::read_dir(schedule_dir) else {
        return 0;
    };
    dir.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .count()
}

fn write_schedule_entry(
    schedule_dir: &std::path::Path,
    prompt: &str,
    at: chrono::DateTime<chrono::Utc>,
    created_by: Option<&str>,
) -> Result<Uuid, String> {
    std::fs::create_dir_all(schedule_dir)
        .map_err(|e| format!("failed to create schedule dir: {e}"))?;

    let count = count_schedule_entries(schedule_dir);
    if count >= MAX_SCHEDULED {
        return Err(format!(
            "schedule limit reached ({count}/{MAX_SCHEDULED}). Cancel existing entries to make room."
        ));
    }

    let id = Uuid::new_v4();
    let entry = ScheduleEntry {
        id,
        prompt: prompt.to_string(),
        at,
        created_by: created_by.map(|s| s.to_string()),
        created_at: chrono::Utc::now(),
    };

    let json = serde_json::to_string_pretty(&entry)
        .map_err(|e| format!("failed to serialize schedule entry: {e}"))?;

    // Atomic write: write to .tmp, then rename
    let tmp_path = schedule_dir.join(format!("{id}.tmp"));
    let final_path = schedule_dir.join(format!("{id}.json"));

    std::fs::write(&tmp_path, json).map_err(|e| format!("failed to write temp file: {e}"))?;
    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("failed to rename temp file: {e}"))?;

    Ok(id)
}

fn list_schedule_entries(schedule_dir: &std::path::Path) -> Result<Vec<ScheduleEntry>, String> {
    if !schedule_dir.exists() {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();
    let dir =
        std::fs::read_dir(schedule_dir).map_err(|e| format!("failed to read schedule dir: {e}"))?;

    for entry in dir {
        let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<ScheduleEntry>(&contents) {
                Ok(sched) => entries.push(sched),
                Err(e) => {
                    eprintln!(
                        "phyl-tool-self: skipping corrupt schedule file {}: {e}",
                        path.display()
                    );
                }
            },
            Err(e) => {
                eprintln!("phyl-tool-self: failed to read {}: {e}", path.display());
            }
        }
    }

    entries.sort_by_key(|e| e.at);
    Ok(entries)
}

// --- Daemon client ---

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
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(id) = v.get("id").and_then(|v| v.as_str())
        {
            return Ok(id.to_string());
        }
        Ok(text)
    } else {
        Err(format!("HTTP {}: {}", status.as_u16(), text))
    }
}

// --- Config helpers ---

fn load_socket_path(home: &std::path::Path) -> Result<String, String> {
    let config_path = home.join("config.toml");
    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read config.toml: {e}"))?;
    let config: Config =
        toml::from_str(&contents).map_err(|e| format!("failed to parse config.toml: {e}"))?;
    Ok(config.daemon.socket)
}

fn write_response(writer: &mut impl Write, response: &ServerResponse) {
    let mut json = serde_json::to_string(response).expect("failed to serialize response");
    json.push('\n');
    let _ = writer.write_all(json.as_bytes());
    let _ = writer.flush();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_specs() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 4);
        assert_eq!(specs[0].name, "spawn_session");
        assert_eq!(specs[1].name, "sleep_until");
        assert_eq!(specs[2].name, "list_scheduled");
        assert_eq!(specs[3].name, "cancel_scheduled");
        for spec in &specs {
            assert_eq!(spec.mode, ToolMode::Server);
        }
    }

    #[test]
    fn test_spec_serialization() {
        let specs = tool_specs();
        let json = serde_json::to_string_pretty(&specs).unwrap();
        assert!(json.contains("spawn_session"));
        assert!(json.contains("sleep_until"));
        assert!(json.contains("list_scheduled"));
        assert!(json.contains("cancel_scheduled"));

        let parsed: Vec<ToolSpec> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 4);
    }

    #[test]
    fn test_write_and_list_schedule_entries() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-sched");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let at = chrono::Utc::now() + chrono::Duration::hours(1);
        let id = write_schedule_entry(&dir, "test prompt", at, Some("sess-1")).unwrap();

        let entries = list_schedule_entries(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].prompt, "test prompt");
        assert_eq!(entries[0].created_by.as_deref(), Some("sess-1"));

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_schedule_entry_atomic() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-atomic");
        let _ = std::fs::remove_dir_all(&dir);

        let at = chrono::Utc::now() + chrono::Duration::minutes(5);
        let id = write_schedule_entry(&dir, "atomic test", at, None).unwrap();

        // Verify no .tmp file remains
        let tmp_path = dir.join(format!("{id}.tmp"));
        assert!(!tmp_path.exists());

        // Verify .json file exists
        let json_path = dir.join(format!("{id}.json"));
        assert!(json_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cancel_schedule_entry() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-cancel");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let at = chrono::Utc::now() + chrono::Duration::hours(1);
        let id = write_schedule_entry(&dir, "to be cancelled", at, None).unwrap();

        let path = dir.join(format!("{id}.json"));
        assert!(path.exists());

        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());

        let entries = list_schedule_entries(&dir).unwrap();
        assert!(entries.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_schedule_nonexistent_dir() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-nonexistent");
        let _ = std::fs::remove_dir_all(&dir);

        let entries = list_schedule_entries(&dir).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_list_schedule_skips_non_json() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-skip");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        // Write a .tmp file
        std::fs::write(dir.join("something.tmp"), "not json").unwrap();

        let at = chrono::Utc::now() + chrono::Duration::hours(1);
        write_schedule_entry(&dir, "real entry", at, None).unwrap();

        let entries = list_schedule_entries(&dir).unwrap();
        assert_eq!(entries.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_schedule_limit_enforced() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-limit");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let now = chrono::Utc::now();
        for i in 0..MAX_SCHEDULED {
            write_schedule_entry(
                &dir,
                &format!("entry {i}"),
                now + chrono::Duration::hours(i as i64 + 1),
                None,
            )
            .unwrap();
        }

        // The next one should fail
        let result = write_schedule_entry(
            &dir,
            "one too many",
            now + chrono::Duration::hours(100),
            None,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("schedule limit reached"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_count_schedule_entries() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-count");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        assert_eq!(count_schedule_entries(&dir), 0);

        let now = chrono::Utc::now();
        write_schedule_entry(&dir, "a", now + chrono::Duration::hours(1), None).unwrap();
        assert_eq!(count_schedule_entries(&dir), 1);

        // .tmp files should not be counted
        std::fs::write(dir.join("something.tmp"), "ignored").unwrap();
        assert_eq!(count_schedule_entries(&dir), 1);

        write_schedule_entry(&dir, "b", now + chrono::Duration::hours(2), None).unwrap();
        assert_eq!(count_schedule_entries(&dir), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_schedule_sorted_by_time() {
        let dir = std::env::temp_dir().join("phyl-tool-self-test-sorted");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let now = chrono::Utc::now();
        write_schedule_entry(&dir, "later", now + chrono::Duration::hours(2), None).unwrap();
        write_schedule_entry(&dir, "sooner", now + chrono::Duration::hours(1), None).unwrap();

        let entries = list_schedule_entries(&dir).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].prompt, "sooner");
        assert_eq!(entries[1].prompt, "later");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use phyl_core::{Config, LogEntry, LogEntryType, SessionInfo, SessionStatus, home_dir};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::net::UnixListener;
use tokio_stream::Stream;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// In-memory state for a tracked session.
struct TrackedSession {
    id: Uuid,
    status: SessionStatus,
    created_at: DateTime<Utc>,
    prompt: String,
    /// OS child process handle (None if re-adopted from crash recovery).
    child: Option<Child>,
    /// PID of the phyl-run process.
    pid: u32,
    /// Session directory path.
    session_dir: PathBuf,
    /// Summary extracted from the done log entry.
    summary: Option<String>,
}

/// Shared daemon state behind Arc<Mutex<_>>.
struct DaemonState {
    sessions: HashMap<Uuid, TrackedSession>,
    config: Config,
    home: PathBuf,
}

type AppState = Arc<Mutex<DaemonState>>;

/// Request body for POST /sessions.
#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    prompt: String,
}

/// Response body for POST /sessions.
#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    id: Uuid,
    status: String,
}

/// Response body for GET /sessions/:id.
#[derive(Debug, Serialize)]
struct SessionDetail {
    #[serde(flatten)]
    info: SessionInfo,
    prompt: String,
    recent_log: Vec<LogEntry>,
}

/// Request body for POST /sessions/:id/events.
#[derive(Debug, Deserialize)]
struct InjectEventRequest {
    #[serde(default)]
    content: Option<String>,
    /// For answering ask_human questions.
    #[serde(default)]
    question_id: Option<String>,
    /// Alias: some callers use "type" to specify event type.
    #[serde(rename = "type", default)]
    event_type: Option<String>,
}

/// Health check response.
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    sessions_active: usize,
    sessions_total: usize,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let home = home_dir();

    // Read config.
    let config = read_config(&home);
    let socket_path = config.daemon.socket.clone();

    eprintln!("phylactd: agent home: {}", home.display());
    eprintln!("phylactd: socket: {socket_path}");

    // Verify home directory exists.
    if !home.exists() {
        eprintln!(
            "phylactd: agent home directory does not exist: {}",
            home.display()
        );
        eprintln!("phylactd: run `phyl init` first");
        std::process::exit(1);
    }

    // Build initial state.
    let state: AppState = Arc::new(Mutex::new(DaemonState {
        sessions: HashMap::new(),
        config,
        home: home.clone(),
    }));

    // Crash recovery: scan sessions/ for orphaned sessions.
    recover_sessions(&state);

    // Remove stale socket file.
    let _ = std::fs::remove_file(&socket_path);

    // Ensure parent directory of socket exists.
    if let Some(parent) = std::path::Path::new(&socket_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Bind Unix socket.
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("phylactd: failed to bind socket {socket_path}: {e}");
            std::process::exit(1);
        }
    };

    // Set socket permissions to owner-only (rw for owner, no access for others).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        if let Err(e) = std::fs::set_permissions(&socket_path, perms) {
            eprintln!("phylactd: warning: failed to set socket permissions: {e}");
        }
    }

    // Write daemon PID file.
    let pid_path = std::env::var("XDG_RUNTIME_DIR")
        .map(|dir| format!("{dir}/phylactd.pid"))
        .unwrap_or_else(|_| "/tmp/phylactd.pid".to_string());
    if let Err(e) = std::fs::write(&pid_path, std::process::id().to_string()) {
        eprintln!("phylactd: warning: failed to write PID file: {e}");
    }

    // Build router.
    let app = build_router(state.clone());

    eprintln!("phylactd: listening on {socket_path}");

    // Spawn background reaper task.
    let reaper_state = state.clone();
    tokio::spawn(async move {
        reaper_loop(reaper_state).await;
    });

    // Serve.
    let serve_result = axum::serve(listener, app).await;
    if let Err(e) = serve_result {
        eprintln!("phylactd: server error: {e}");
    }

    // Cleanup.
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_path);
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/sessions", post(handle_create_session))
        .route("/sessions", get(handle_list_sessions))
        .route("/sessions/{id}", get(handle_get_session))
        .route("/sessions/{id}", delete(handle_delete_session))
        .route("/sessions/{id}/events", post(handle_inject_event))
        .route("/feed", get(handle_feed))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn read_config(home: &std::path::Path) -> Config {
    let config_path = home.join("config.toml");
    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match toml::from_str::<Config>(&contents) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("phylactd: warning: failed to parse config.toml: {e}");
                eprintln!("phylactd: using defaults");
                Config::default()
            }
        },
        Err(_) => {
            eprintln!("phylactd: config.toml not found, using defaults");
            Config::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Crash recovery
// ---------------------------------------------------------------------------

/// On startup, scan sessions/ for directories with pid files.
/// Re-adopt running processes, mark dead ones as crashed.
fn recover_sessions(state: &AppState) {
    let home = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.home.clone()
    };
    let sessions_dir = home.join("sessions");

    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let pid_path = path.join("pid");
        if !pid_path.exists() {
            continue;
        }

        // Parse directory name as UUID.
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let session_id = match Uuid::parse_str(&dir_name) {
            Ok(id) => id,
            Err(_) => continue,
        };

        // Read PID.
        let pid_str = match std::fs::read_to_string(&pid_path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Check if process is still running.
        let is_alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;

        // Try to extract created_at from log.jsonl first entry.
        let created_at = read_session_created_at(&path).unwrap_or_else(Utc::now);

        // Try to extract the prompt from log.jsonl.
        let prompt = read_session_prompt(&path).unwrap_or_default();

        let status = if is_alive {
            eprintln!("phylactd: re-adopting running session {session_id} (pid {pid})");
            SessionStatus::Running
        } else {
            // Check if there's a done entry in the log.
            let summary = read_session_summary(&path);
            if summary.is_some() {
                eprintln!("phylactd: found completed session {session_id}");
                SessionStatus::Done
            } else {
                eprintln!("phylactd: marking crashed session {session_id} (pid {pid})");
                SessionStatus::Crashed
            }
        };

        let summary = if status == SessionStatus::Done {
            read_session_summary(&path)
        } else {
            None
        };

        let tracked = TrackedSession {
            id: session_id,
            status,
            created_at,
            prompt,
            child: None, // Re-adopted, no Child handle.
            pid,
            session_dir: path,
            summary,
        };

        state.lock().unwrap_or_else(|e| e.into_inner()).sessions.insert(session_id, tracked);
    }
}

// ---------------------------------------------------------------------------
// Background reaper
// ---------------------------------------------------------------------------

/// Periodically check session processes and reap finished ones.
async fn reaper_loop(state: AppState) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        reap_sessions(&state);
    }
}

/// Check all running sessions; update status for finished processes.
fn reap_sessions(state: &AppState) {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    let ids: Vec<Uuid> = s
        .sessions
        .iter()
        .filter(|(_, t)| t.status == SessionStatus::Running)
        .map(|(id, _)| *id)
        .collect();

    for id in ids {
        let tracked = match s.sessions.get_mut(&id) {
            Some(t) => t,
            None => continue,
        };

        // Try to reap child process if we have a handle.
        let finished = if let Some(ref mut child) = tracked.child {
            match child.try_wait() {
                Ok(Some(exit_status)) => Some(exit_status.success()),
                Ok(None) => None, // Still running.
                Err(e) => {
                    eprintln!("phylactd: error checking child for {id}: {e}");
                    None
                }
            }
        } else {
            // Re-adopted session — check via kill(0).
            let alive = unsafe { libc::kill(tracked.pid as libc::pid_t, 0) } == 0;
            if alive {
                None
            } else {
                // Process is gone. Check log for done entry.
                let has_done = read_session_summary(&tracked.session_dir).is_some();
                Some(has_done)
            }
        };

        if let Some(success) = finished {
            let summary = read_session_summary(&tracked.session_dir);
            tracked.summary = summary;

            if success {
                tracked.status = SessionStatus::Done;
                eprintln!("phylactd: session {id} completed");
            } else {
                // Check if there was a done log entry (exit code may be non-zero
                // but session still completed normally via the done tool).
                if tracked.summary.is_some() {
                    tracked.status = SessionStatus::Done;
                    eprintln!("phylactd: session {id} completed (with done entry)");
                } else {
                    tracked.status = SessionStatus::Crashed;
                    eprintln!("phylactd: session {id} crashed");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// API Handlers
// ---------------------------------------------------------------------------

/// GET /health
async fn handle_health(State(state): State<AppState>) -> Json<HealthResponse> {
    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    let active = s
        .sessions
        .values()
        .filter(|t| t.status == SessionStatus::Running)
        .count();
    Json(HealthResponse {
        status: "ok".to_string(),
        sessions_active: active,
        sessions_total: s.sessions.len(),
    })
}

/// POST /sessions — start a new session.
async fn handle_create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), (StatusCode, String)> {
    let (home, config, running_count) = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        let running = s
            .sessions
            .values()
            .filter(|t| t.status == SessionStatus::Running)
            .count();
        (s.home.clone(), s.config.clone(), running)
    };

    // Enforce concurrency limit.
    if running_count >= config.session.max_concurrent as usize {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "Maximum concurrent sessions reached ({})",
                config.session.max_concurrent
            ),
        ));
    }

    let session_id = Uuid::new_v4();
    let session_dir = home.join("sessions").join(session_id.to_string());

    // Create session directory.
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create session directory: {e}"),
        ));
    }

    // Spawn phyl-run.
    let child = match spawn_session(&session_dir, &req.prompt, &home) {
        Ok(c) => c,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to spawn session runner: {e}"),
            ));
        }
    };

    let pid = child.id();
    let now = Utc::now();

    let tracked = TrackedSession {
        id: session_id,
        status: SessionStatus::Running,
        created_at: now,
        prompt: req.prompt,
        child: Some(child),
        pid,
        session_dir,
        summary: None,
    };

    state.lock().unwrap_or_else(|e| e.into_inner()).sessions.insert(session_id, tracked);

    eprintln!("phylactd: started session {session_id} (pid {pid})");

    Ok((
        StatusCode::CREATED,
        Json(CreateSessionResponse {
            id: session_id,
            status: "running".to_string(),
        }),
    ))
}

/// GET /sessions — list all sessions.
async fn handle_list_sessions(State(state): State<AppState>) -> Json<Vec<SessionInfo>> {
    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    let mut sessions: Vec<SessionInfo> = s
        .sessions
        .values()
        .map(|t| SessionInfo {
            id: t.id,
            status: t.status.clone(),
            created_at: t.created_at,
            summary: t.summary.clone(),
        })
        .collect();
    // Sort by created_at descending (newest first).
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Json(sessions)
}

/// GET /sessions/:id — session detail with recent log entries.
async fn handle_get_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionDetail>, (StatusCode, String)> {
    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    let tracked = s
        .sessions
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, format!("Session {id} not found")))?;

    let info = SessionInfo {
        id: tracked.id,
        status: tracked.status.clone(),
        created_at: tracked.created_at,
        summary: tracked.summary.clone(),
    };

    let recent_log = read_recent_log(&tracked.session_dir, 50);

    Ok(Json(SessionDetail {
        info,
        prompt: tracked.prompt.clone(),
        recent_log,
    }))
}

/// DELETE /sessions/:id — kill a running session.
async fn handle_delete_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    // First pass: validate and send SIGTERM/kill, then drop the lock.
    let needs_sigkill = {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        let tracked = s
            .sessions
            .get_mut(&id)
            .ok_or((StatusCode::NOT_FOUND, format!("Session {id} not found")))?;

        if tracked.status != SessionStatus::Running {
            return Err((
                StatusCode::CONFLICT,
                format!("Session {id} is not running (status: {:?})", tracked.status),
            ));
        }

        // Kill the process.
        if let Some(ref mut child) = tracked.child {
            if let Err(e) = child.kill() {
                eprintln!("phylactd: failed to kill child for {id}: {e}");
            }
            let _ = child.wait();
            None // No follow-up needed.
        } else {
            // Re-adopted session: send SIGTERM first.
            unsafe {
                libc::kill(tracked.pid as libc::pid_t, libc::SIGTERM);
            }
            Some(tracked.pid) // Need follow-up SIGKILL.
        }
    }; // Lock dropped here.

    // If we need to escalate to SIGKILL, wait asynchronously (don't block the runtime).
    if let Some(pid) = needs_sigkill {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }

    // Second pass: update status.
    {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tracked) = s.sessions.get_mut(&id) {
            tracked.status = SessionStatus::Done;
            tracked.summary = Some("Killed by user".to_string());
        }
    }

    eprintln!("phylactd: killed session {id}");

    Ok(StatusCode::NO_CONTENT)
}

/// POST /sessions/:id/events — inject an event into a running session's FIFO.
async fn handle_inject_event(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<InjectEventRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    let tracked = s
        .sessions
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, format!("Session {id} not found")))?;

    if tracked.status != SessionStatus::Running {
        return Err((
            StatusCode::CONFLICT,
            format!("Session {id} is not running"),
        ));
    }

    let fifo_path = tracked.session_dir.join("events");
    if !fifo_path.exists() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Session FIFO not yet created".to_string(),
        ));
    }

    // Build the event JSON to write to the FIFO.
    let event = build_fifo_event(&req);

    // Write to FIFO. Use O_WRONLY | O_NONBLOCK to avoid blocking if no reader.
    match write_to_fifo(&fifo_path, &event) {
        Ok(()) => Ok(StatusCode::ACCEPTED),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write to FIFO: {e}"),
        )),
    }
}

/// GET /feed — SSE stream of attention-worthy events across all sessions.
async fn handle_feed(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let stream = async_stream::stream! {
        // Track read positions per session.
        let mut positions: HashMap<Uuid, u64> = HashMap::new();

        loop {
            let sessions: Vec<(Uuid, PathBuf, SessionStatus)> = {
                let s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.sessions
                    .values()
                    .map(|t| (t.id, t.session_dir.clone(), t.status.clone()))
                    .collect()
            };

            for (id, session_dir, _status) in &sessions {
                let log_path = session_dir.join("log.jsonl");
                let pos = positions.entry(*id).or_insert(0);

                if let Ok(entries) = read_log_from_offset(&log_path, pos) {
                    for entry in entries {
                        // Filter for attention-worthy events.
                        match entry.entry_type {
                            LogEntryType::Question
                            | LogEntryType::Done
                            | LogEntryType::Error => {
                                let event_data = serde_json::json!({
                                    "session_id": id.to_string(),
                                    "entry": entry,
                                });
                                if let Ok(data) = serde_json::to_string(&event_data) {
                                    let event_type = match entry.entry_type {
                                        LogEntryType::Question => "question",
                                        LogEntryType::Done => "done",
                                        LogEntryType::Error => "error",
                                        _ => "event",
                                    };
                                    yield Ok(Event::default()
                                        .event(event_type)
                                        .data(data));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// Session spawning
// ---------------------------------------------------------------------------

/// Spawn a phyl-run process for a new session.
fn spawn_session(session_dir: &std::path::Path, prompt: &str, home: &std::path::Path) -> Result<Child, String> {
    // Find phyl-run binary. Prefer $PATH, then same directory as phylactd.
    let phyl_run = find_binary("phyl-run")?;

    let child = Command::new(&phyl_run)
        .arg("--session-dir")
        .arg(session_dir)
        .arg("--prompt")
        .arg(prompt)
        .env("PHYLACTERY_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null()) // phyl-run redirects its own stderr to stderr.log.
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", phyl_run))?;

    Ok(child)
}

/// Find a binary by name: check $PATH, then the directory of the current executable.
fn find_binary(name: &str) -> Result<String, String> {
    // Check if it's on PATH using `which`.
    if let Ok(output) = Command::new("which").arg(name).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }

    // Check same directory as current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().to_string());
            }
        }
    }

    // Fall back to bare name (let OS search PATH).
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------
// FIFO writing
// ---------------------------------------------------------------------------

/// Build a JSON event from the inject request.
fn build_fifo_event(req: &InjectEventRequest) -> String {
    if let Some(ref qid) = req.question_id {
        // Answer to an ask_human question.
        serde_json::json!({
            "type": "answer",
            "question_id": qid,
            "content": req.content.as_deref().unwrap_or(""),
        })
        .to_string()
    } else {
        // Plain user message or typed event.
        let event_type = req.event_type.as_deref().unwrap_or("user");
        serde_json::json!({
            "type": event_type,
            "content": req.content.as_deref().unwrap_or(""),
        })
        .to_string()
    }
}

/// Write a single line to the session FIFO (non-blocking).
fn write_to_fifo(fifo_path: &std::path::Path, data: &str) -> Result<(), String> {
    // Open FIFO with O_WRONLY | O_NONBLOCK.
    let fd = unsafe {
        let path_c = std::ffi::CString::new(fifo_path.to_string_lossy().as_bytes())
            .map_err(|e| format!("invalid path: {e}"))?;
        libc::open(path_c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK)
    };

    if fd < 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("failed to open FIFO: {err}"));
    }

    // Write data + newline atomically (must be < PIPE_BUF = 4096).
    let line = format!("{data}\n");
    let bytes = line.as_bytes();
    if bytes.len() > 4096 {
        unsafe { libc::close(fd) };
        return Err("event data exceeds PIPE_BUF (4096 bytes)".to_string());
    }

    let written = unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
    unsafe { libc::close(fd) };

    if written < 0 {
        let err = std::io::Error::last_os_error();
        Err(format!("failed to write to FIFO: {err}"))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Log reading helpers
// ---------------------------------------------------------------------------

/// Read the most recent N log entries from a session's log.jsonl.
fn read_recent_log(session_dir: &std::path::Path, max_entries: usize) -> Vec<LogEntry> {
    let log_path = session_dir.join("log.jsonl");
    let file = match std::fs::File::open(&log_path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut entries: Vec<LogEntry> = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            entries.push(entry);
        }
    }

    // Return last max_entries.
    if entries.len() > max_entries {
        entries.split_off(entries.len() - max_entries)
    } else {
        entries
    }
}

/// Read log entries from a byte offset, updating the offset.
fn read_log_from_offset(
    log_path: &std::path::Path,
    offset: &mut u64,
) -> Result<Vec<LogEntry>, String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::File::open(log_path) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };

    // Get file size.
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let file_size = metadata.len();

    if *offset >= file_size {
        return Ok(Vec::new());
    }

    file.seek(SeekFrom::Start(*offset))
        .map_err(|e| e.to_string())?;

    let mut buf = String::new();
    file.read_to_string(&mut buf).map_err(|e| e.to_string())?;

    *offset = file_size;

    let entries: Vec<LogEntry> = buf
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    Ok(entries)
}

/// Read the created_at timestamp from the first log entry.
fn read_session_created_at(session_dir: &std::path::Path) -> Option<DateTime<Utc>> {
    let log_path = session_dir.join("log.jsonl");
    let file = std::fs::File::open(log_path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            return Some(entry.ts);
        }
    }
    None
}

/// Read the prompt (first user message) from the log.
fn read_session_prompt(session_dir: &std::path::Path) -> Option<String> {
    let log_path = session_dir.join("log.jsonl");
    let file = std::fs::File::open(log_path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            if entry.entry_type == LogEntryType::User {
                return entry.content;
            }
        }
    }
    None
}

/// Read the summary from the done log entry.
fn read_session_summary(session_dir: &std::path::Path) -> Option<String> {
    let log_path = session_dir.join("log.jsonl");
    let file = std::fs::File::open(log_path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            if entry.entry_type == LogEntryType::Done {
                return entry.summary.or(entry.content);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_health_response_serialization() {
        let resp = HealthResponse {
            status: "ok".to_string(),
            sessions_active: 2,
            sessions_total: 5,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"sessions_active\":2"));
        assert!(json.contains("\"sessions_total\":5"));
    }

    #[test]
    fn test_create_session_request_deserialization() {
        let json = r#"{"prompt":"do the thing"}"#;
        let req: CreateSessionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.prompt, "do the thing");
    }

    #[test]
    fn test_inject_event_request_user_message() {
        let json = r#"{"content":"hello world"}"#;
        let req: InjectEventRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.content.as_deref(), Some("hello world"));
        assert!(req.question_id.is_none());
    }

    #[test]
    fn test_inject_event_request_answer() {
        let json = r#"{"content":"yes","question_id":"q_1"}"#;
        let req: InjectEventRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.content.as_deref(), Some("yes"));
        assert_eq!(req.question_id.as_deref(), Some("q_1"));
    }

    #[test]
    fn test_inject_event_request_with_type() {
        let json = r#"{"type":"answer","question_id":"q_1","content":"yes"}"#;
        let req: InjectEventRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.event_type.as_deref(), Some("answer"));
        assert_eq!(req.question_id.as_deref(), Some("q_1"));
    }

    #[test]
    fn test_build_fifo_event_user_message() {
        let req = InjectEventRequest {
            content: Some("hello".to_string()),
            question_id: None,
            event_type: None,
        };
        let event = build_fifo_event(&req);
        let parsed: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert_eq!(parsed["type"], "user");
        assert_eq!(parsed["content"], "hello");
    }

    #[test]
    fn test_build_fifo_event_answer() {
        let req = InjectEventRequest {
            content: Some("yes".to_string()),
            question_id: Some("q_1".to_string()),
            event_type: None,
        };
        let event = build_fifo_event(&req);
        let parsed: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert_eq!(parsed["type"], "answer");
        assert_eq!(parsed["question_id"], "q_1");
        assert_eq!(parsed["content"], "yes");
    }

    #[test]
    fn test_read_recent_log_empty() {
        let dir = std::env::temp_dir().join(format!("phylactd-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let entries = read_recent_log(&dir, 10);
        assert!(entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_recent_log_with_entries() {
        let dir = std::env::temp_dir().join(format!("phylactd-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let log_path = dir.join("log.jsonl");
        let mut f = std::fs::File::create(&log_path).unwrap();
        for i in 0..5 {
            let entry = LogEntry {
                ts: Utc::now(),
                entry_type: LogEntryType::User,
                content: Some(format!("message {i}")),
                summary: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
                id: None,
                question_id: None,
                options: Vec::new(),
            };
            writeln!(f, "{}", serde_json::to_string(&entry).unwrap()).unwrap();
        }

        let entries = read_recent_log(&dir, 3);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].content.as_deref(), Some("message 2"));
        assert_eq!(entries[2].content.as_deref(), Some("message 4"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_session_summary() {
        let dir = std::env::temp_dir().join(format!("phylactd-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let log_path = dir.join("log.jsonl");
        let mut f = std::fs::File::create(&log_path).unwrap();

        let entry = LogEntry {
            ts: Utc::now(),
            entry_type: LogEntryType::Done,
            content: None,
            summary: Some("All done".to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            id: None,
            question_id: None,
            options: Vec::new(),
        };
        writeln!(f, "{}", serde_json::to_string(&entry).unwrap()).unwrap();

        let summary = read_session_summary(&dir);
        assert_eq!(summary.as_deref(), Some("All done"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_session_prompt() {
        let dir = std::env::temp_dir().join(format!("phylactd-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let log_path = dir.join("log.jsonl");
        let mut f = std::fs::File::create(&log_path).unwrap();

        // System entry first.
        let sys_entry = LogEntry {
            ts: Utc::now(),
            entry_type: LogEntryType::System,
            content: Some("Session started".to_string()),
            summary: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            id: None,
            question_id: None,
            options: Vec::new(),
        };
        writeln!(f, "{}", serde_json::to_string(&sys_entry).unwrap()).unwrap();

        // User entry.
        let user_entry = LogEntry {
            ts: Utc::now(),
            entry_type: LogEntryType::User,
            content: Some("do the thing".to_string()),
            summary: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            id: None,
            question_id: None,
            options: Vec::new(),
        };
        writeln!(f, "{}", serde_json::to_string(&user_entry).unwrap()).unwrap();

        let prompt = read_session_prompt(&dir);
        assert_eq!(prompt.as_deref(), Some("do the thing"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.session.timeout_minutes, 60);
        assert_eq!(config.session.max_concurrent, 4);
        assert_eq!(config.session.model, "phyl-model-claude");
        assert_eq!(config.model.context_window, 200_000);
    }

    #[test]
    fn test_router_creation() {
        let state: AppState = Arc::new(Mutex::new(DaemonState {
            sessions: HashMap::new(),
            config: Config::default(),
            home: PathBuf::from("/tmp/test-phylactery"),
        }));
        let _router = build_router(state);
        // Just verify it doesn't panic.
    }
}

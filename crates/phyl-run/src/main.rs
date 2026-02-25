use chrono::Utc;
use phyl_core::{
    Config, LogEntry, LogEntryType, Message, ModelRequest, ModelResponse, Role, ServerRequest,
    ServerResponse, ToolCall, ToolInput, ToolMode, ToolOutput, ToolSpec,
};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Brief wait (seconds) for FIFO events when the model spoke without tool calls.
const IMPLICIT_DONE_WAIT_SECS: u64 = 2;

/// Maximum retries for a model adapter invocation.
const MODEL_MAX_RETRIES: u32 = 1;

/// Model adapter invocation timeout (seconds).
const MODEL_TIMEOUT_SECS: u64 = 300;

/// Maximum word count for SOUL.md before truncation.
const SOUL_MAX_WORDS: usize = 3000;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("phyl-run: {e}");
            std::process::exit(1);
        }
    };

    // Ensure session directory exists.
    if let Err(e) = fs::create_dir_all(&args.session_dir) {
        eprintln!(
            "phyl-run: failed to create session directory {}: {e}",
            args.session_dir.display()
        );
        std::process::exit(1);
    }

    // Step 1: Redirect stderr to sessions/<uuid>/stderr.log
    redirect_stderr(&args.session_dir);

    // Step 2: Write PID file.
    let pid_path = args.session_dir.join("pid");
    if let Err(e) = fs::write(&pid_path, std::process::id().to_string()) {
        eprintln!("phyl-run: failed to write PID file: {e}");
    }

    eprintln!(
        "phyl-run: starting session in {}",
        args.session_dir.display()
    );

    // Run the session. On exit, clean up.
    let exit_code = match run_session(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("phyl-run: session failed: {e}");
            1
        }
    };

    // Step 13: Cleanup.
    let fifo_path = args.session_dir.join("events");
    let _ = fs::remove_file(&fifo_path);
    let _ = fs::remove_file(&pid_path);

    eprintln!("phyl-run: session complete, exit code {exit_code}");
    std::process::exit(exit_code);
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

struct Args {
    session_dir: PathBuf,
    prompt: String,
}

fn parse_args() -> Result<Args, String> {
    let args: Vec<String> = std::env::args().collect();
    let mut session_dir: Option<PathBuf> = None;
    let mut prompt: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--session-dir" => {
                i += 1;
                if i >= args.len() {
                    return Err("--session-dir requires a value".into());
                }
                session_dir = Some(PathBuf::from(&args[i]));
            }
            "--prompt" => {
                i += 1;
                if i >= args.len() {
                    return Err("--prompt requires a value".into());
                }
                prompt = Some(args[i].clone());
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
        i += 1;
    }

    Ok(Args {
        session_dir: session_dir.ok_or("--session-dir is required")?,
        prompt: prompt.ok_or("--prompt is required")?,
    })
}

// ---------------------------------------------------------------------------
// Session runner
// ---------------------------------------------------------------------------

fn run_session(args: &Args) -> Result<(), String> {
    let home = phyl_core::home_dir();

    // Derive session ID from the directory name.
    let session_id = args
        .session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Step 3: Read config.toml.
    let config = read_config(&home)?;

    // Step 4: Read LAW.md, JOB.md, SOUL.md, knowledge/INDEX.md + knowledge summary.
    let law = read_file_or_default(&home.join("LAW.md"), "");
    let job = read_file_or_default(&home.join("JOB.md"), "");
    let soul = read_file_or_default(&home.join("SOUL.md"), "I am new.");
    let index = read_file_or_default(&home.join("knowledge/INDEX.md"), "");
    let knowledge_summary = generate_knowledge_summary(&home.join("knowledge"));

    // Step 6: Discover tools from $PATH.
    let discovered = discover_tools();
    eprintln!(
        "phyl-run: discovered {} tool spec(s) from {} executable(s)",
        discovered.iter().map(|d| d.specs.len()).sum::<usize>(),
        discovered.len(),
    );

    // Collect all tool specs for the model request.
    let all_specs: Vec<ToolSpec> = discovered.iter().flat_map(|d| d.specs.clone()).collect();
    let tool_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();

    // Step 5: Assemble system prompt.
    let system_prompt = build_system_prompt(
        &law,
        &job,
        &soul,
        &index,
        &knowledge_summary,
        &session_id,
        &args.session_dir,
        &tool_names,
    );

    // Step 7: Start server-mode tools.
    let mut server_tools = start_server_tools(&discovered)?;
    eprintln!(
        "phyl-run: started {} server-mode tool process(es)",
        server_tools.len()
    );

    // Build a mapping from tool name → (executable path, mode, which server process).
    let tool_map = build_tool_map(&discovered);

    // Step 8: Create FIFO.
    let fifo_path = args.session_dir.join("events");
    let fifo_fd = create_fifo(&fifo_path)?;

    // Create scratch directory.
    let scratch_dir = args.session_dir.join("scratch");
    let _ = fs::create_dir_all(&scratch_dir);

    // Open log file.
    let log_path = args.session_dir.join("log.jsonl");
    let mut log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("failed to open log.jsonl: {e}"))?;

    // Step 9: Initialize history.
    let mut history: Vec<Message> = vec![
        Message {
            role: Role::System,
            content: system_prompt,
            tool_calls: vec![],
            tool_call_id: None,
        },
        Message {
            role: Role::User,
            content: args.prompt.clone(),
            tool_calls: vec![],
            tool_call_id: None,
        },
    ];

    // Write initial log entries.
    write_log(
        &mut log_file,
        LogEntryType::System,
        Some("Session started"),
        None,
        &[],
        None,
    )?;
    write_log(
        &mut log_file,
        LogEntryType::User,
        Some(&args.prompt),
        None,
        &[],
        None,
    )?;

    // Context tracking.
    let mut cumulative_tokens: u64 = 0;
    let compress_threshold = (config.model.context_window as f64 * config.model.compress_at) as u64;

    let session_start = Instant::now();
    let session_timeout = Duration::from_secs(config.session.timeout_minutes * 60);
    let model_binary = &config.session.model;
    let session_dir_abs =
        fs::canonicalize(&args.session_dir).unwrap_or_else(|_| args.session_dir.clone());

    // Set environment variables for tools.
    // SAFETY: phyl-run is single-threaded at this point (before spawning tool threads).
    unsafe {
        std::env::set_var("PHYLACTERY_SESSION_ID", &session_id);
        std::env::set_var("PHYLACTERY_SESSION_DIR", &session_dir_abs);
        std::env::set_var("PHYLACTERY_HOME", &home);
        std::env::set_var("PHYLACTERY_KNOWLEDGE_DIR", home.join("knowledge"));
    }

    // Step 10: The agentic loop.
    let mut end_session = false;
    let mut final_summary: Option<String> = None;

    loop {
        // 10g: Check cumulative timeout.
        if session_start.elapsed() > session_timeout {
            eprintln!("phyl-run: session timed out after {:?}", session_timeout);
            write_log(
                &mut log_file,
                LogEntryType::Error,
                Some("Session timed out"),
                None,
                &[],
                None,
            )?;
            break;
        }

        // 10a-b: Build model request and invoke model adapter.
        let model_request = ModelRequest {
            messages: history.clone(),
            tools: all_specs.clone(),
        };

        eprintln!(
            "phyl-run: invoking model adapter ({model_binary}), history has {} messages",
            history.len()
        );
        let response = invoke_model_with_retry(model_binary, &model_request, MODEL_MAX_RETRIES)?;

        // Track token usage.
        if let Some(ref usage) = response.usage {
            cumulative_tokens =
                cumulative_tokens.saturating_add(usage.input_tokens + usage.output_tokens);
        } else {
            // Rough estimate: chars / 4.
            let chars: u64 = history.iter().map(|m| m.content.len() as u64).sum();
            cumulative_tokens = chars / 4;
        }

        // 10c: Append assistant message to history.
        let assistant_msg = Message {
            role: Role::Assistant,
            content: response.content.clone(),
            tool_calls: response.tool_calls.clone(),
            tool_call_id: None,
        };
        history.push(assistant_msg);

        // 10d: Write assistant entry to log.
        write_log(
            &mut log_file,
            LogEntryType::Assistant,
            if response.content.is_empty() {
                None
            } else {
                Some(&response.content)
            },
            None,
            &response.tool_calls,
            None,
        )?;

        // 10e: If response has tool_calls.
        if !response.tool_calls.is_empty() {
            eprintln!(
                "phyl-run: dispatching {} tool call(s): {}",
                response.tool_calls.len(),
                response
                    .tool_calls
                    .iter()
                    .map(|tc| tc.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            let results = dispatch_tool_calls(
                &response.tool_calls,
                &tool_map,
                &discovered,
                &mut server_tools,
                fifo_fd,
            )?;

            // Append tool results to history and log.
            for result in &results {
                history.push(Message {
                    role: Role::Tool,
                    content: result.content.clone(),
                    tool_calls: vec![],
                    tool_call_id: Some(result.tool_call_id.clone()),
                });

                write_log(
                    &mut log_file,
                    LogEntryType::ToolResult,
                    Some(&result.content),
                    None,
                    &[],
                    Some(&result.tool_call_id),
                )?;

                if result.end_session {
                    end_session = true;
                    final_summary = Some(result.content.clone());
                }
            }

            if end_session {
                eprintln!("phyl-run: received end_session signal, finalizing");
                break;
            }
        } else {
            // 10f: Model spoke without tool calls.
            eprintln!("phyl-run: model responded without tool calls, checking FIFO for events");

            let events = poll_fifo(fifo_fd, Duration::from_secs(IMPLICIT_DONE_WAIT_SECS));
            if events.is_empty() {
                // Implicit done.
                eprintln!("phyl-run: no new events, treating as implicit done");
                final_summary = Some(response.content.clone());
                break;
            }

            // Append user events.
            for event in events {
                history.push(Message {
                    role: Role::User,
                    content: event.clone(),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
                write_log(
                    &mut log_file,
                    LogEntryType::User,
                    Some(&event),
                    None,
                    &[],
                    None,
                )?;
            }
        }

        // Context window management: compress if needed.
        if cumulative_tokens > compress_threshold {
            eprintln!(
                "phyl-run: context approaching limit ({cumulative_tokens}/{} tokens), compressing",
                config.model.context_window
            );
            history = compress_history(model_binary, &history, &all_specs)?;
            // Reset token estimate after compression.
            let chars: u64 = history.iter().map(|m| m.content.len() as u64).sum();
            cumulative_tokens = chars / 4;
        }
    }

    // Step 11: Finalize (SOUL.md reflection).
    eprintln!("phyl-run: beginning finalization");

    // 11a: Close stdin on server-mode tools.
    for (_, st) in server_tools.iter_mut() {
        st.close_stdin();
    }

    // Finalization — SOUL.md reflection.
    finalize_soul(&home, model_binary, &history, &session_id)?;

    // Step 12: Write final done entry.
    let summary = final_summary.as_deref().unwrap_or("Session complete");
    write_log(
        &mut log_file,
        LogEntryType::Done,
        None,
        Some(summary),
        &[],
        None,
    )?;

    // Close FIFO fd.
    unsafe {
        libc::close(fifo_fd);
    }

    // Wait for server-mode tool processes to exit.
    for (name, st) in server_tools.iter_mut() {
        eprintln!("phyl-run: waiting for server tool '{name}' to exit");
        let _ = st.child.wait();
    }

    eprintln!("phyl-run: finalization complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn read_config(home: &Path) -> Result<Config, String> {
    let config_path = home.join("config.toml");
    if !config_path.exists() {
        eprintln!(
            "phyl-run: config.toml not found at {}, using defaults",
            config_path.display()
        );
        return Ok(Config::default());
    }

    let contents =
        fs::read_to_string(&config_path).map_err(|e| format!("failed to read config.toml: {e}"))?;
    toml::from_str(&contents).map_err(|e| format!("failed to parse config.toml: {e}"))
}

fn read_file_or_default(path: &Path, default: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|_| default.to_string())
}

// ---------------------------------------------------------------------------
// Knowledge base summary generation
// ---------------------------------------------------------------------------

/// Generate a structured file tree of the knowledge base for inclusion in the
/// system prompt.
///
/// Lists files and directories under `knowledge/` in a compact tree format.
/// The agent can use `read_file` to fetch specific files on demand — no file
/// content is included here, keeping context window usage minimal.
fn generate_knowledge_summary(knowledge_dir: &Path) -> String {
    if !knowledge_dir.is_dir() {
        return String::new();
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_knowledge_files(knowledge_dir, &mut files);

    if files.is_empty() {
        return String::new();
    }

    // Sort for deterministic, readable output.
    files.sort();

    let mut summary =
        String::from("Files in knowledge/ (use read_file to access, search_files to search):\n");

    for file_path in &files {
        let rel_path = file_path.strip_prefix(knowledge_dir).unwrap_or(file_path);

        // Skip INDEX.md — it's already included separately.
        if rel_path.to_string_lossy() == "INDEX.md" {
            continue;
        }

        summary.push_str(&format!("  {}\n", rel_path.display()));
    }

    summary
}

/// Recursively collect all files under a directory, skipping hidden files/dirs.
fn collect_knowledge_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Skip hidden entries.
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with('.')
        {
            continue;
        }

        if path.is_dir() {
            collect_knowledge_files(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// System prompt assembly
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_system_prompt(
    law: &str,
    job: &str,
    soul: &str,
    index: &str,
    knowledge_summary: &str,
    session_id: &str,
    session_dir: &Path,
    tool_names: &[String],
) -> String {
    let scratch = session_dir.join("scratch");
    let tools_list = if tool_names.is_empty() {
        "none".to_string()
    } else {
        tool_names.join(", ")
    };

    let knowledge_section = if knowledge_summary.is_empty() {
        format!("=== KNOWLEDGE INDEX ===\n{index}")
    } else {
        format!(
            "=== KNOWLEDGE INDEX ===\n{index}\n\n\
             === KNOWLEDGE SUMMARY ===\n{knowledge_summary}"
        )
    };

    format!(
        "=== LAW ===\n\
         {law}\n\n\
         === JOB ===\n\
         {job}\n\n\
         === SOUL ===\n\
         {soul}\n\n\
         {knowledge_section}\n\n\
         === SESSION ===\n\
         Session ID: {session_id}\n\
         Working directory: {scratch}\n\
         You have access to the following tools: {tools_list}\n\n\
         Remember: LAW rules are absolute. Obey them unconditionally.",
        scratch = scratch.display(),
    )
}

// ---------------------------------------------------------------------------
// Tool discovery
// ---------------------------------------------------------------------------

/// Information about a discovered tool executable.
struct DiscoveredTool {
    /// Full path to the executable.
    path: PathBuf,
    /// Tool specs returned by `--spec`.
    specs: Vec<ToolSpec>,
    /// Does this executable have any server-mode tools?
    has_server_mode: bool,
}

fn discover_tools() -> Vec<DiscoveredTool> {
    let mut results = Vec::new();

    let path_var = std::env::var("PATH").unwrap_or_default();
    let mut seen_executables: std::collections::HashSet<String> = std::collections::HashSet::new();

    for dir in path_var.split(':') {
        let dir_path = Path::new(dir);
        let entries = match fs::read_dir(dir_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("phyl-tool-") {
                continue;
            }

            // Deduplicate: first occurrence on PATH wins.
            if seen_executables.contains(&name) {
                continue;
            }
            seen_executables.insert(name.clone());

            let path = entry.path();
            eprintln!("phyl-run: discovering tool: {}", path.display());

            match run_tool_spec(&path) {
                Ok(specs) => {
                    let has_server_mode = specs.iter().any(|s| s.mode == ToolMode::Server);
                    results.push(DiscoveredTool {
                        path,
                        specs,
                        has_server_mode,
                    });
                }
                Err(e) => {
                    eprintln!("phyl-run: failed to get spec from {name}: {e}");
                }
            }
        }
    }

    results
}

/// Run `phyl-tool-X --spec` and parse the output.
fn run_tool_spec(path: &Path) -> Result<Vec<ToolSpec>, String> {
    let output = Command::new(path)
        .arg("--spec")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run --spec: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("--spec exited with {}: {stderr}", output.status));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();

    // Try parsing as array first, then as single spec.
    if let Ok(specs) = serde_json::from_str::<Vec<ToolSpec>>(trimmed) {
        Ok(specs)
    } else if let Ok(spec) = serde_json::from_str::<ToolSpec>(trimmed) {
        Ok(vec![spec])
    } else {
        Err(format!(
            "failed to parse --spec output as ToolSpec: {trimmed}"
        ))
    }
}

// ---------------------------------------------------------------------------
// Tool mapping
// ---------------------------------------------------------------------------

/// Info about how to dispatch a tool call.
struct ToolInfo {
    /// Path to the executable.
    executable: PathBuf,
    /// Tool mode.
    mode: ToolMode,
}

fn build_tool_map(discovered: &[DiscoveredTool]) -> HashMap<String, ToolInfo> {
    let mut map = HashMap::new();
    for dt in discovered {
        for spec in &dt.specs {
            map.insert(
                spec.name.clone(),
                ToolInfo {
                    executable: dt.path.clone(),
                    mode: spec.mode.clone(),
                },
            );
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Server-mode tool management
// ---------------------------------------------------------------------------

struct ServerTool {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stdout_reader: BufReader<std::process::ChildStdout>,
}

impl ServerTool {
    fn send_request(&mut self, req: &ServerRequest) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("server tool stdin closed")?;
        let mut json = serde_json::to_string(req).map_err(|e| format!("serialize: {e}"))?;
        json.push('\n');
        stdin
            .write_all(json.as_bytes())
            .map_err(|e| format!("write to server tool: {e}"))?;
        stdin
            .flush()
            .map_err(|e| format!("flush server tool: {e}"))?;
        Ok(())
    }

    fn read_response(&mut self) -> Result<ServerResponse, String> {
        let mut line = String::new();
        self.stdout_reader
            .read_line(&mut line)
            .map_err(|e| format!("read from server tool: {e}"))?;
        if line.is_empty() {
            return Err("server tool closed stdout (pipe broken)".into());
        }
        serde_json::from_str(line.trim()).map_err(|e| format!("parse server response: {e}: {line}"))
    }

    fn close_stdin(&mut self) {
        self.stdin.take(); // Dropping closes the pipe.
    }
}

fn start_server_tools(
    discovered: &[DiscoveredTool],
) -> Result<HashMap<String, ServerTool>, String> {
    let mut server_tools = HashMap::new();

    for dt in discovered {
        if !dt.has_server_mode {
            continue;
        }

        let exec_name = dt
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        eprintln!("phyl-run: starting server-mode tool: {}", exec_name);

        let mut child = Command::new(&dt.path)
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to start server tool {exec_name}: {e}"))?;

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("no stdout from server tool {exec_name}"))?;

        server_tools.insert(
            exec_name,
            ServerTool {
                child,
                stdin,
                stdout_reader: BufReader::new(stdout),
            },
        );
    }

    Ok(server_tools)
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

struct ToolResult {
    tool_call_id: String,
    content: String,
    end_session: bool,
}

fn dispatch_tool_calls(
    tool_calls: &[ToolCall],
    tool_map: &HashMap<String, ToolInfo>,
    discovered: &[DiscoveredTool],
    server_tools: &mut HashMap<String, ServerTool>,
    _fifo_fd: i32,
) -> Result<Vec<ToolResult>, String> {
    let mut results = Vec::new();

    // Separate into oneshot and server-mode calls.
    let mut oneshot_calls = Vec::new();
    let mut server_calls = Vec::new();

    for tc in tool_calls {
        match tool_map.get(&tc.name) {
            Some(info) => match info.mode {
                ToolMode::Oneshot => oneshot_calls.push(tc),
                ToolMode::Server => server_calls.push(tc),
            },
            None => {
                // Unknown tool — return error to model.
                results.push(ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: format!("Error: unknown tool '{}'", tc.name),
                    end_session: false,
                });
            }
        }
    }

    // Dispatch oneshot tools in parallel using threads.
    let oneshot_results = dispatch_oneshot_parallel(&oneshot_calls, tool_map);
    results.extend(oneshot_results);

    // Dispatch server-mode tool calls.
    for tc in server_calls {
        let result = dispatch_server_call(tc, tool_map, discovered, server_tools)?;
        results.push(result);
    }

    // Sort results to match the original order from the model.
    let order: HashMap<&str, usize> = tool_calls
        .iter()
        .enumerate()
        .map(|(i, tc)| (tc.id.as_str(), i))
        .collect();
    results.sort_by_key(|r| {
        order
            .get(r.tool_call_id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });

    Ok(results)
}

fn dispatch_oneshot_parallel(
    calls: &[&ToolCall],
    tool_map: &HashMap<String, ToolInfo>,
) -> Vec<ToolResult> {
    if calls.is_empty() {
        return vec![];
    }

    // For a single call, just do it inline.
    if calls.len() == 1 {
        let tc = calls[0];
        let info = tool_map.get(&tc.name).unwrap();
        return vec![invoke_oneshot_tool(&info.executable, tc)];
    }

    // Multiple calls: spawn threads.
    let handles: Vec<_> = calls
        .iter()
        .map(|tc| {
            let executable = tool_map.get(&tc.name).unwrap().executable.clone();
            let tc_id = tc.id.clone();
            let tc_name = tc.name.clone();
            let tc_args = tc.arguments.clone();
            std::thread::spawn(move || {
                let tc = ToolCall {
                    id: tc_id,
                    name: tc_name,
                    arguments: tc_args,
                };
                invoke_oneshot_tool(&executable, &tc)
            })
        })
        .collect();

    handles
        .into_iter()
        .map(|h| {
            h.join().unwrap_or_else(|_| ToolResult {
                tool_call_id: String::new(),
                content: "Error: tool thread panicked".to_string(),
                end_session: false,
            })
        })
        .collect()
}

fn invoke_oneshot_tool(executable: &Path, tc: &ToolCall) -> ToolResult {
    let input = ToolInput {
        name: tc.name.clone(),
        arguments: tc.arguments.clone(),
    };

    let input_json = match serde_json::to_string(&input) {
        Ok(j) => j,
        Err(e) => {
            return ToolResult {
                tool_call_id: tc.id.clone(),
                content: format!("Error: failed to serialize tool input: {e}"),
                end_session: false,
            };
        }
    };

    eprintln!(
        "phyl-run: invoking oneshot tool '{}' via {}",
        tc.name,
        executable.display()
    );

    let mut child = match Command::new(executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ToolResult {
                tool_call_id: tc.id.clone(),
                content: format!("Error: failed to spawn tool: {e}"),
                end_session: false,
            };
        }
    };

    // Write input to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input_json.as_bytes());
    }

    // Wait for output with timeout (use the tool's default timeout — 2 minutes).
    let timeout = Duration::from_secs(120);
    match wait_for_output(child, timeout) {
        Ok((status, stdout, stderr)) => {
            if !stderr.is_empty() {
                eprintln!("phyl-run: tool '{}' stderr: {stderr}", tc.name);
            }

            if !status.success() {
                let msg = if stderr.is_empty() {
                    format!("Tool exited with status {}", status.code().unwrap_or(-1))
                } else {
                    stderr
                };
                return ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: format!("Error: {msg}"),
                    end_session: false,
                };
            }

            // Parse ToolOutput.
            match serde_json::from_str::<ToolOutput>(&stdout) {
                Ok(output) => {
                    let content = if let Some(err) = output.error {
                        if let Some(out) = output.output {
                            format!("{out}\nError: {err}")
                        } else {
                            format!("Error: {err}")
                        }
                    } else {
                        output.output.unwrap_or_default()
                    };
                    ToolResult {
                        tool_call_id: tc.id.clone(),
                        content,
                        end_session: false,
                    }
                }
                Err(e) => ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: format!("Error: invalid JSON from tool: {e}\nRaw: {stdout}"),
                    end_session: false,
                },
            }
        }
        Err(e) => ToolResult {
            tool_call_id: tc.id.clone(),
            content: format!("Error: {e}"),
            end_session: false,
        },
    }
}

fn dispatch_server_call(
    tc: &ToolCall,
    tool_map: &HashMap<String, ToolInfo>,
    _discovered: &[DiscoveredTool],
    server_tools: &mut HashMap<String, ServerTool>,
) -> Result<ToolResult, String> {
    let info = tool_map
        .get(&tc.name)
        .ok_or_else(|| format!("no tool info for '{}'", tc.name))?;

    // Find which server tool process handles this.
    let exec_name = info
        .executable
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let server = server_tools
        .get_mut(&exec_name)
        .ok_or_else(|| format!("no server process for '{exec_name}'"))?;

    let request = ServerRequest {
        id: tc.id.clone(),
        name: tc.name.clone(),
        arguments: tc.arguments.clone(),
    };

    eprintln!(
        "phyl-run: sending server-mode call '{}' (id: {})",
        tc.name, tc.id
    );
    server.send_request(&request)?;

    // Read response.
    let response = server.read_response()?;

    let content = if let Some(err) = response.error {
        format!("Error: {err}")
    } else {
        response.output.unwrap_or_default()
    };

    let end_session = response.signal.as_deref() == Some("end_session");

    Ok(ToolResult {
        tool_call_id: response.id,
        content,
        end_session,
    })
}

// ---------------------------------------------------------------------------
// Model adapter invocation
// ---------------------------------------------------------------------------

fn invoke_model_with_retry(
    model_binary: &str,
    request: &ModelRequest,
    max_retries: u32,
) -> Result<ModelResponse, String> {
    let mut last_error = String::new();

    for attempt in 0..=max_retries {
        if attempt > 0 {
            eprintln!(
                "phyl-run: retrying model invocation (attempt {}/{})",
                attempt + 1,
                max_retries + 1
            );
        }

        match invoke_model(model_binary, request) {
            Ok(response) => {
                // Check if the response itself indicates an error.
                if response.content.starts_with("Error:")
                    && response.tool_calls.is_empty()
                    && attempt < max_retries
                {
                    last_error = response.content.clone();
                    eprintln!("phyl-run: model returned error: {last_error}");
                    continue;
                }
                return Ok(response);
            }
            Err(e) => {
                last_error = e;
                eprintln!("phyl-run: model invocation failed: {last_error}");
            }
        }
    }

    Err(format!(
        "model adapter failed after {} attempts: {last_error}",
        max_retries + 1
    ))
}

fn invoke_model(model_binary: &str, request: &ModelRequest) -> Result<ModelResponse, String> {
    let input_json = serde_json::to_string(request)
        .map_err(|e| format!("failed to serialize model request: {e}"))?;

    let mut child = Command::new(model_binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn model adapter ({model_binary}): {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input_json.as_bytes());
    }

    let (status, stdout, stderr) = wait_for_output(child, Duration::from_secs(MODEL_TIMEOUT_SECS))
        .map_err(|e| format!("model adapter error: {e}"))?;

    if !stderr.is_empty() {
        eprintln!("phyl-run: model adapter stderr: {stderr}");
    }

    if !status.success() {
        return Err(format!(
            "model adapter exited with status {}. stderr: {}",
            status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    serde_json::from_str::<ModelResponse>(&stdout)
        .map_err(|e| format!("invalid JSON from model adapter: {e}\nRaw: {stdout}"))
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

fn wait_for_output(
    child: Child,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), String> {
    use std::sync::mpsc;

    let pid = child.id();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let mut child = child;
        let stdout = child
            .stdout
            .take()
            .map(|mut r| {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                s
            })
            .unwrap_or_default();
        let stderr = child
            .stderr
            .take()
            .map(|mut r| {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                s
            })
            .unwrap_or_default();
        let status = child.wait();
        let _ = tx.send((status, stdout, stderr));
    });

    match rx.recv_timeout(timeout) {
        Ok((Ok(status), stdout, stderr)) => Ok((status, stdout, stderr)),
        Ok((Err(e), _, _)) => Err(format!("failed to wait on process: {e}")),
        Err(_) => {
            // Timeout — kill the process.
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            Err(format!("process timed out after {timeout:?}"))
        }
    }
}

// ---------------------------------------------------------------------------
// FIFO management
// ---------------------------------------------------------------------------

fn create_fifo(path: &Path) -> Result<i32, String> {
    use std::ffi::CString;

    // Remove existing FIFO if present.
    let _ = fs::remove_file(path);

    let c_path = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|e| format!("invalid FIFO path: {e}"))?;

    let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if ret != 0 {
        return Err(format!("mkfifo failed: {}", io::Error::last_os_error()));
    }

    // Open with O_RDWR | O_NONBLOCK to avoid blocking on open.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
    if fd < 0 {
        return Err(format!(
            "failed to open FIFO: {}",
            io::Error::last_os_error()
        ));
    }

    eprintln!("phyl-run: FIFO created at {}", path.display());
    Ok(fd)
}

fn poll_fifo(fd: i32, timeout: Duration) -> Vec<String> {
    let mut events = Vec::new();

    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };

    let timeout_ms = timeout.as_millis() as i32;
    let ret = unsafe { libc::poll(&mut pollfd as *mut _, 1, timeout_ms) };

    if ret <= 0 {
        return events;
    }

    if pollfd.revents & libc::POLLIN != 0 {
        // Read available data.
        let file = unsafe { File::from_raw_fd(fd) };
        let reader = BufReader::new(&file);

        for line in reader.lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    // Try to parse as JSON event with a "content" field.
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&l) {
                        if let Some(content) = val.get("content").and_then(|c| c.as_str()) {
                            events.push(content.to_string());
                        }
                    } else {
                        // Plain text event.
                        events.push(l);
                    }
                }
                Ok(_) => {} // Empty line.
                Err(_) => break,
            }
        }

        // Don't close the fd — we reuse it. Prevent File from dropping and closing.
        std::mem::forget(file);
    }

    events
}

// ---------------------------------------------------------------------------
// Log writing
// ---------------------------------------------------------------------------

fn write_log(
    file: &mut File,
    entry_type: LogEntryType,
    content: Option<&str>,
    summary: Option<&str>,
    tool_calls: &[ToolCall],
    tool_call_id: Option<&str>,
) -> Result<(), String> {
    let entry = LogEntry {
        ts: Utc::now(),
        entry_type,
        content: content.map(|s| s.to_string()),
        summary: summary.map(|s| s.to_string()),
        tool_calls: tool_calls.to_vec(),
        tool_call_id: tool_call_id.map(|s| s.to_string()),
        id: None,
        question_id: None,
        options: vec![],
    };

    let mut json =
        serde_json::to_string(&entry).map_err(|e| format!("failed to serialize log entry: {e}"))?;
    json.push('\n');
    file.write_all(json.as_bytes())
        .map_err(|e| format!("failed to write log entry: {e}"))?;
    file.flush()
        .map_err(|e| format!("failed to flush log: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Context window management
// ---------------------------------------------------------------------------

fn compress_history(
    model_binary: &str,
    history: &[Message],
    _tools: &[ToolSpec],
) -> Result<Vec<Message>, String> {
    // Keep system prompt (first message).
    let system_msg = history.first().ok_or("empty history")?.clone();

    // Skip system message for compression.
    let rest = &history[1..];
    if rest.len() <= 3 {
        // Too few messages to compress.
        return Ok(history.to_vec());
    }

    // Take the oldest 2/3 of non-system messages for summarization.
    let compress_count = (rest.len() * 2) / 3;
    let to_compress = &rest[..compress_count];
    let to_keep = &rest[compress_count..];

    // Build summary of compressed messages.
    let mut summary_text = String::new();
    for msg in to_compress {
        match msg.role {
            Role::User => summary_text.push_str(&format!("User: {}\n", msg.content)),
            Role::Assistant => {
                if !msg.content.is_empty() {
                    summary_text.push_str(&format!("Assistant: {}\n", msg.content));
                }
                for tc in &msg.tool_calls {
                    summary_text.push_str(&format!("  [called {}]\n", tc.name));
                }
            }
            Role::Tool => {
                let preview: String = msg.content.chars().take(200).collect();
                summary_text.push_str(&format!("Tool result: {preview}\n"));
            }
            Role::System => {}
        }
    }

    // Ask model to summarize.
    let summary_request = ModelRequest {
        messages: vec![
            Message {
                role: Role::System,
                content: "Summarize the following conversation segment in 2-3 paragraphs. \
                         Preserve key facts, decisions, and context needed to continue the conversation."
                    .to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: summary_text,
                tool_calls: vec![],
                tool_call_id: None,
            },
        ],
        tools: vec![], // No tools for summarization.
    };

    let summary_response = invoke_model(model_binary, &summary_request)?;

    // Rebuild history: system + summary + remaining messages.
    let mut new_history = vec![system_msg];
    new_history.push(Message {
        role: Role::User,
        content: format!(
            "[Summary of earlier conversation]\n{}",
            summary_response.content
        ),
        tool_calls: vec![],
        tool_call_id: None,
    });
    new_history.extend(to_keep.iter().cloned());

    eprintln!(
        "phyl-run: compressed history from {} to {} messages",
        history.len(),
        new_history.len()
    );

    Ok(new_history)
}

// ---------------------------------------------------------------------------
// Finalization: SOUL.md reflection
// ---------------------------------------------------------------------------

fn finalize_soul(
    home: &Path,
    model_binary: &str,
    history: &[Message],
    session_id: &str,
) -> Result<(), String> {
    let soul_path = home.join("SOUL.md");
    let soul_lock_path = home.join(".soul.lock");
    let git_lock_path = home.join(".git.lock");

    // 11b: flock --exclusive .soul.lock.
    let soul_lock = acquire_flock(&soul_lock_path)?;

    // 11c: Re-read SOUL.md from disk (not the version loaded at session start).
    let current_soul = read_file_or_default(&soul_path, "I am new.");

    // Build a summary of the session for the reflection prompt.
    let session_summary = summarize_session(history);

    // 11d: Invoke model adapter for reflection.
    let reflection_request = ModelRequest {
        messages: vec![
            Message {
                role: Role::System,
                content: format!(
                    "You are reflecting on a session that just completed. \
                     Here is your current SOUL.md — your living self-portrait:\n\n\
                     ---\n{current_soul}\n---\n\n\
                     Here is what happened in this session:\n\n\
                     {session_summary}\n\n\
                     Reflect on this session. Output an updated version of SOUL.md.\n\
                     SOUL.md must stay under 2000 words. If you need to add something, \
                     revise and compress — don't just append. Old reflections that no longer \
                     feel relevant can be removed. This file is your living self-portrait, \
                     not a journal. Keep it current, not comprehensive.\n\n\
                     Output ONLY the new content for SOUL.md — no preamble, no explanation, \
                     no code fences."
                ),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: "Please reflect and produce the updated SOUL.md.".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ],
        tools: vec![], // No tools for reflection.
    };

    match invoke_model(model_binary, &reflection_request) {
        Ok(response) => {
            let mut new_soul = response.content.trim().to_string();

            // 11j: Truncate if over 3000 words.
            let word_count = new_soul.split_whitespace().count();
            if word_count > SOUL_MAX_WORDS {
                eprintln!(
                    "phyl-run: WARNING: SOUL.md is {word_count} words (limit {SOUL_MAX_WORDS}), truncating"
                );
                new_soul = truncate_soul(&new_soul);
            }

            // 11e: Write to disk.
            if let Err(e) = fs::write(&soul_path, &new_soul) {
                eprintln!("phyl-run: failed to write SOUL.md: {e}");
                release_flock(soul_lock);
                return Ok(()); // Non-fatal.
            }

            // 11f-h: Git commit.
            let git_lock = acquire_flock(&git_lock_path)?;

            let commit_result = Command::new("git")
                .args(["add", "SOUL.md"])
                .current_dir(home)
                .output()
                .and_then(|_| {
                    Command::new("git")
                        .args([
                            "commit",
                            "-m",
                            &format!("soul: reflect on session {session_id}"),
                        ])
                        .current_dir(home)
                        .output()
                });

            match commit_result {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        eprintln!(
                            "phyl-run: git commit for SOUL.md returned {}: {stderr}",
                            output.status
                        );
                    } else {
                        eprintln!("phyl-run: SOUL.md updated and committed");
                    }
                }
                Err(e) => {
                    eprintln!("phyl-run: git commit failed for SOUL.md: {e}");
                }
            }

            release_flock(git_lock);
        }
        Err(e) => {
            eprintln!("phyl-run: finalization model call failed, skipping SOUL.md update: {e}");
        }
    }

    // 11i: Release soul lock.
    release_flock(soul_lock);

    Ok(())
}

fn summarize_session(history: &[Message]) -> String {
    let mut summary = String::new();
    for msg in history {
        match msg.role {
            Role::System => {} // Skip system prompt in summary.
            Role::User => {
                summary.push_str(&format!("User: {}\n\n", msg.content));
            }
            Role::Assistant => {
                if !msg.content.is_empty() {
                    summary.push_str(&format!("Assistant: {}\n\n", msg.content));
                }
                for tc in &msg.tool_calls {
                    summary.push_str(&format!("  [Called tool: {}]\n", tc.name));
                }
            }
            Role::Tool => {
                // Truncate long tool results.
                let preview: String = msg.content.chars().take(500).collect();
                let truncated = if msg.content.len() > 500 { "..." } else { "" };
                summary.push_str(&format!(
                    "  [Tool result for {}]: {preview}{truncated}\n\n",
                    msg.tool_call_id.as_deref().unwrap_or("?")
                ));
            }
        }
    }
    summary
}

fn truncate_soul(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= 10 {
        return content.to_string();
    }

    // Keep first 1/3 and last 1/3 of lines.
    let keep = lines.len() / 3;
    let mut result = String::new();
    for line in &lines[..keep] {
        result.push_str(line);
        result.push('\n');
    }
    result.push_str("\n[... earlier reflections trimmed for length ...]\n\n");
    for line in &lines[lines.len() - keep..] {
        result.push_str(line);
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// File locking (flock)
// ---------------------------------------------------------------------------

fn acquire_flock(path: &Path) -> Result<i32, String> {
    use std::ffi::CString;

    // Create the lock file if it doesn't exist.
    let _ = OpenOptions::new().create(true).append(true).open(path);

    let c_path = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|e| format!("invalid lock path: {e}"))?;

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o600) };
    if fd < 0 {
        return Err(format!(
            "failed to open lock file {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }

    let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if ret != 0 {
        unsafe {
            libc::close(fd);
        }
        return Err(format!(
            "flock failed on {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }

    eprintln!("phyl-run: acquired lock on {}", path.display());
    Ok(fd)
}

fn release_flock(fd: i32) {
    unsafe {
        libc::flock(fd, libc::LOCK_UN);
        libc::close(fd);
    }
}

// ---------------------------------------------------------------------------
// stderr redirection
// ---------------------------------------------------------------------------

fn redirect_stderr(session_dir: &Path) {
    use std::ffi::CString;

    let log_path = session_dir.join("stderr.log");
    let c_path = match CString::new(log_path.to_string_lossy().as_bytes()) {
        Ok(p) => p,
        Err(_) => return,
    };

    unsafe {
        let fd = libc::open(
            c_path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            0o644,
        );
        if fd >= 0 {
            libc::dup2(fd, 2); // Redirect stderr (fd 2).
            libc::close(fd);
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
    fn test_build_system_prompt() {
        let prompt = build_system_prompt(
            "Do no harm.",
            "You are a helper.",
            "I am new.",
            "No entries yet.",
            "",
            "test-123",
            Path::new("/tmp/sessions/test-123"),
            &["bash".to_string(), "read_file".to_string()],
        );
        assert!(prompt.contains("=== LAW ==="));
        assert!(prompt.contains("Do no harm."));
        assert!(prompt.contains("=== JOB ==="));
        assert!(prompt.contains("You are a helper."));
        assert!(prompt.contains("=== SOUL ==="));
        assert!(prompt.contains("I am new."));
        assert!(prompt.contains("=== KNOWLEDGE INDEX ==="));
        assert!(prompt.contains("No entries yet."));
        assert!(prompt.contains("Session ID: test-123"));
        assert!(prompt.contains("bash, read_file"));
        assert!(prompt.contains("LAW rules are absolute"));
    }

    #[test]
    fn test_build_system_prompt_no_tools() {
        let prompt = build_system_prompt(
            "law",
            "job",
            "soul",
            "index",
            "",
            "s1",
            Path::new("/tmp/s1"),
            &[],
        );
        assert!(prompt.contains("tools: none"));
    }

    #[test]
    fn test_build_system_prompt_with_knowledge_summary() {
        let prompt = build_system_prompt(
            "law",
            "job",
            "soul",
            "index",
            "Files in knowledge/ (use read_file to access):\n  contacts/alice.md\n  projects/rust.md\n",
            "s1",
            Path::new("/tmp/s1"),
            &["bash".to_string()],
        );
        assert!(prompt.contains("=== KNOWLEDGE SUMMARY ==="));
        assert!(prompt.contains("contacts/alice.md"));
        assert!(prompt.contains("projects/rust.md"));
    }

    #[test]
    fn test_build_system_prompt_empty_knowledge_summary() {
        let prompt = build_system_prompt(
            "law",
            "job",
            "soul",
            "index",
            "",
            "s1",
            Path::new("/tmp/s1"),
            &["bash".to_string()],
        );
        // When knowledge summary is empty, no KNOWLEDGE SUMMARY section.
        assert!(!prompt.contains("=== KNOWLEDGE SUMMARY ==="));
        assert!(prompt.contains("=== KNOWLEDGE INDEX ==="));
    }

    #[test]
    fn test_summarize_session() {
        let history = vec![
            Message {
                role: Role::System,
                content: "System prompt".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: "Hello".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::Assistant,
                content: "Hi there".to_string(),
                tool_calls: vec![ToolCall {
                    id: "tc_1".to_string(),
                    name: "bash".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
                tool_call_id: None,
            },
        ];
        let summary = summarize_session(&history);
        assert!(!summary.contains("System prompt")); // System messages excluded.
        assert!(summary.contains("User: Hello"));
        assert!(summary.contains("Assistant: Hi there"));
        assert!(summary.contains("Called tool: bash"));
    }

    #[test]
    fn test_truncate_soul() {
        let long_soul = (0..30)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = truncate_soul(&long_soul);
        assert!(truncated.contains("Line 0")); // First section kept.
        assert!(truncated.contains("Line 29")); // Last section kept.
        assert!(truncated.contains("trimmed for length"));
    }

    #[test]
    fn test_truncate_soul_short() {
        let short_soul = "I am new.\nI like learning.";
        let result = truncate_soul(short_soul);
        assert_eq!(result, short_soul);
    }

    #[test]
    fn test_parse_args_valid() {
        // We can't easily test parse_args since it reads std::env::args,
        // but we can test the system prompt builder and other pure functions.
    }

    #[test]
    fn test_read_file_or_default() {
        let result = read_file_or_default(Path::new("/nonexistent/path/file.md"), "default value");
        assert_eq!(result, "default value");
    }

    #[test]
    fn test_generate_knowledge_summary_nonexistent() {
        let summary = generate_knowledge_summary(Path::new("/nonexistent/knowledge"));
        assert!(summary.is_empty());
    }

    #[test]
    fn test_generate_knowledge_summary_with_files() {
        let tmp = std::env::temp_dir().join("phyl_test_kb_summary");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("contacts")).unwrap();
        fs::create_dir_all(tmp.join("projects")).unwrap();
        fs::write(tmp.join("INDEX.md"), "# Knowledge Index\n").unwrap();
        fs::write(tmp.join("contacts/alice.md"), "Alice is a friend.\n").unwrap();
        fs::write(tmp.join("projects/rust.md"), "Learning Rust.\n").unwrap();

        let summary = generate_knowledge_summary(&tmp);
        assert!(summary.contains("contacts/alice.md"));
        assert!(summary.contains("projects/rust.md"));
        // INDEX.md should be excluded from the file list (it's included separately).
        assert!(!summary.contains("  INDEX.md"));
        // File contents should NOT be included — just the file tree.
        assert!(!summary.contains("Alice is a friend."));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_generate_knowledge_summary_empty_dir() {
        let tmp = std::env::temp_dir().join("phyl_test_kb_empty");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let summary = generate_knowledge_summary(&tmp);
        assert!(summary.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_generate_knowledge_summary_skips_hidden() {
        let tmp = std::env::temp_dir().join("phyl_test_kb_hidden");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join(".hidden_file"), "secret").unwrap();
        fs::write(tmp.join("visible.md"), "visible content").unwrap();

        let summary = generate_knowledge_summary(&tmp);
        assert!(!summary.contains("hidden_file"));
        assert!(summary.contains("visible.md"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_collect_knowledge_files_recursive() {
        let tmp = std::env::temp_dir().join("phyl_test_kb_recurse");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("a/b")).unwrap();
        fs::write(tmp.join("top.md"), "top").unwrap();
        fs::write(tmp.join("a/mid.md"), "mid").unwrap();
        fs::write(tmp.join("a/b/deep.md"), "deep").unwrap();

        let mut files = Vec::new();
        collect_knowledge_files(&tmp, &mut files);
        assert_eq!(files.len(), 3);

        let _ = fs::remove_dir_all(&tmp);
    }
}

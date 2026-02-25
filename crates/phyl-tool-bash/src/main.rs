use phyl_core::{SandboxSpec, ToolInput, ToolMode, ToolOutput, ToolSpec};
use std::io::{self, Read};
use std::process::Command;
use std::time::Duration;

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

fn tool_spec() -> ToolSpec {
    ToolSpec {
        name: "bash".to_string(),
        description: "Execute a shell command and return its output".to_string(),
        mode: ToolMode::Oneshot,
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        }),
        sandbox: Some(SandboxSpec {
            paths_rw: vec![
                "$PHYLACTERY_SESSION_DIR/scratch/".to_string(),
                "/tmp".to_string(),
            ],
            paths_ro: vec![
                "/usr".to_string(),
                "/lib".to_string(),
                "/bin".to_string(),
                "/etc".to_string(),
            ],
            net: true,
            max_cpu_seconds: Some(120),
            max_file_bytes: Some(104_857_600),
            max_procs: Some(64),
            max_fds: Some(256),
        }),
    }
}

fn run_command(command: &str) -> ToolOutput {
    // Determine working directory: $PHYLACTERY_SESSION_DIR/scratch/ if set.
    let work_dir = std::env::var("PHYLACTERY_SESSION_DIR")
        .ok()
        .map(|dir| std::path::PathBuf::from(dir).join("scratch"));

    // Create scratch directory if it doesn't exist.
    if let Some(ref dir) = work_dir
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        return ToolOutput {
            output: None,
            error: Some(format!("Failed to create scratch directory: {e}")),
        };
    }

    let timeout = std::env::var("PHYLACTERY_TOOL_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);

    if let Some(ref dir) = work_dir {
        cmd.current_dir(dir);
    }

    // Spawn and wait with timeout.
    let child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return ToolOutput {
                output: None,
                error: Some(format!("Failed to spawn shell: {e}")),
            };
        }
    };

    match wait_with_timeout(child, Duration::from_secs(timeout)) {
        Ok((status, stdout, stderr)) => {
            let mut combined = stdout;
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }

            if status.success() {
                ToolOutput {
                    output: Some(combined),
                    error: None,
                }
            } else {
                let code = status
                    .code()
                    .map_or("unknown".to_string(), |c| c.to_string());
                ToolOutput {
                    output: if combined.is_empty() {
                        None
                    } else {
                        Some(combined)
                    },
                    error: Some(format!("Command exited with status {code}")),
                }
            }
        }
        Err(e) => ToolOutput {
            output: None,
            error: Some(e),
        },
    }
}

/// Wait for a child process with a timeout. Returns (ExitStatus, stdout, stderr).
fn wait_with_timeout(
    child: std::process::Child,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), String> {
    use std::sync::mpsc;
    use std::thread;

    let pid = child.id();
    let (tx, rx) = mpsc::channel();

    // Spawn a thread to wait on the child so we can enforce a timeout.
    thread::spawn(move || {
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
        Ok((Err(e), _, _)) => Err(format!("Failed to wait on process: {e}")),
        Err(_) => {
            // Timeout — kill the process.
            #[cfg(unix)]
            {
                // Kill the process group to clean up any children.
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
            Err(format!("Command timed out after {timeout:?}"))
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--spec") {
        let spec = tool_spec();
        println!(
            "{}",
            serde_json::to_string_pretty(&spec).expect("failed to serialize spec")
        );
        return;
    }

    // Read ToolInput from stdin.
    let mut input_str = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input_str) {
        let err = ToolOutput {
            output: None,
            error: Some(format!("Failed to read stdin: {e}")),
        };
        println!("{}", serde_json::to_string(&err).unwrap());
        std::process::exit(1);
    }

    let input: ToolInput = match serde_json::from_str(&input_str) {
        Ok(v) => v,
        Err(e) => {
            let err = ToolOutput {
                output: None,
                error: Some(format!("Invalid JSON input: {e}")),
            };
            println!("{}", serde_json::to_string(&err).unwrap());
            std::process::exit(1);
        }
    };

    if input.name != "bash" {
        let err = ToolOutput {
            output: None,
            error: Some(format!("Unknown tool: {}", input.name)),
        };
        println!("{}", serde_json::to_string(&err).unwrap());
        std::process::exit(1);
    }

    let command = match input.arguments.get("command").and_then(|v| v.as_str()) {
        Some(cmd) => cmd,
        None => {
            let err = ToolOutput {
                output: None,
                error: Some("Missing required argument: command".to_string()),
            };
            println!("{}", serde_json::to_string(&err).unwrap());
            std::process::exit(1);
        }
    };

    let result = run_command(command);
    println!("{}", serde_json::to_string(&result).unwrap());
}

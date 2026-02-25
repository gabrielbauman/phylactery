use phyl_core::{ServerRequest, ServerResponse, ToolMode, ToolSpec};
use serde::Deserialize;
use std::io::{self, BufRead, Write};

fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "ask_human".to_string(),
            description: "Ask the human operator a question and wait for their response. \
                         Use this when you need human input, approval, or clarification. \
                         The question will be delivered to the human via whatever bridge \
                         they have configured (terminal, Signal, etc)."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask the human"
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of suggested answer options"
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional additional context to show the human"
                    }
                },
                "required": ["question"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "done".to_string(),
            description: "End the current session. Call this when you have completed \
                         the task or there is nothing more to do. Provide a brief \
                         summary of what was accomplished."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Brief summary of what was accomplished in this session"
                    }
                },
                "required": ["summary"]
            }),
            sandbox: None,
        },
    ]
}

/// Message forwarded by the session runner with an answer from the human.
/// The runner sends this on our stdin when a human responds to an ask_human call.
#[derive(Debug, Deserialize)]
struct ForwardedAnswer {
    id: String,
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    timeout: bool,
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

    eprintln!("phyl-tool-session: use --spec or --serve");
    std::process::exit(1);
}

fn serve() {
    let stdin = io::stdin();
    let stdout = io::stdout();

    let reader = stdin.lock();
    let mut writer = stdout.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("phyl-tool-session: stdin read error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // First try to parse as a ServerRequest (tool call from the runner).
        if let Ok(req) = serde_json::from_str::<ServerRequest>(trimmed) {
            match req.name.as_str() {
                "done" => {
                    let summary = req
                        .arguments
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Session complete");

                    let response = ServerResponse {
                        id: req.id,
                        output: Some(summary.to_string()),
                        error: None,
                        signal: Some("end_session".to_string()),
                    };

                    write_response(&mut writer, &response);
                    // After done, we can exit — the runner will close stdin.
                    return;
                }
                "ask_human" => {
                    let question = req
                        .arguments
                        .get("question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no question provided)");

                    eprintln!(
                        "phyl-tool-session: ask_human waiting for answer (id: {}): {question}",
                        req.id
                    );

                    // We block here waiting for the session runner to forward an answer.
                    // The runner reads the FIFO, matches the answer to our request ID,
                    // and sends us a ForwardedAnswer on stdin.
                    //
                    // But since we're reading lines from stdin in a loop, the next line
                    // should be the forwarded answer. The runner ensures this by sending
                    // the answer immediately after the question event.
                    //
                    // For now, we emit the response immediately — the actual blocking
                    // and answer forwarding is handled by the runner. We output a
                    // "waiting" response and then expect the runner to send us the answer.

                    // Actually, we need to wait for the answer on stdin.
                    // The protocol is: runner sends us a ForwardedAnswer JSON line.
                    // We continue reading from stdin in our main loop and will get it.
                    // So we need to handle this differently — store pending requests
                    // and match them when answers arrive.

                    // Simple approach: since the runner serializes calls to us,
                    // the next non-request line on stdin should be the forwarded answer.
                    // We'll read lines until we get one that parses as ForwardedAnswer
                    // with our ID.

                    // For the initial implementation, just report that we're waiting.
                    // The response will come when we get the forwarded answer.

                    // Store the pending request ID and continue reading.
                    // We handle the answer in the main loop below.
                    let pending_id = req.id.clone();

                    // Read next lines looking for the answer.
                    // (In the real flow, the runner may interleave other calls.)
                    // For simplicity, handle it inline here since done and ask_human
                    // are the only tools we serve.
                    loop {
                        let mut answer_line = String::new();
                        match io::stdin().read_line(&mut answer_line) {
                            Ok(0) => {
                                // EOF — stdin closed, session ending.
                                let response = ServerResponse {
                                    id: pending_id,
                                    output: Some(
                                        "Session ended before human responded".to_string(),
                                    ),
                                    error: None,
                                    signal: None,
                                };
                                write_response(&mut writer, &response);
                                return;
                            }
                            Ok(_) => {
                                let trimmed = answer_line.trim();
                                if trimmed.is_empty() {
                                    continue;
                                }

                                // Try to parse as ForwardedAnswer.
                                if let Ok(fwd) = serde_json::from_str::<ForwardedAnswer>(trimmed)
                                    && fwd.id == pending_id
                                {
                                    let output = if fwd.timeout {
                                        "No response from human — timed out".to_string()
                                    } else {
                                        fwd.answer.unwrap_or_else(|| "No response".to_string())
                                    };

                                    let response = ServerResponse {
                                        id: pending_id,
                                        output: Some(format!("Human answered: {output}")),
                                        error: None,
                                        signal: None,
                                    };
                                    write_response(&mut writer, &response);
                                    break;
                                }

                                // If it's a different ServerRequest (e.g., a done call),
                                // handle it inline.
                                if let Ok(other_req) =
                                    serde_json::from_str::<ServerRequest>(trimmed)
                                    && other_req.name == "done"
                                {
                                    let summary = other_req
                                        .arguments
                                        .get("summary")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Session complete");

                                    let response = ServerResponse {
                                        id: other_req.id,
                                        output: Some(summary.to_string()),
                                        error: None,
                                        signal: Some("end_session".to_string()),
                                    };
                                    write_response(&mut writer, &response);

                                    // Also respond to the pending ask_human.
                                    let cancel_response = ServerResponse {
                                        id: pending_id,
                                        output: Some(
                                            "Session ended while waiting for human".to_string(),
                                        ),
                                        error: None,
                                        signal: None,
                                    };
                                    write_response(&mut writer, &cancel_response);
                                    return;
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "phyl-tool-session: stdin error while waiting for answer: {e}"
                                );
                                let response = ServerResponse {
                                    id: pending_id,
                                    output: None,
                                    error: Some(format!("stdin error: {e}")),
                                    signal: None,
                                };
                                write_response(&mut writer, &response);
                                break;
                            }
                        }
                    }
                }
                other => {
                    let response = ServerResponse {
                        id: req.id,
                        output: None,
                        error: Some(format!("unknown tool: {other}")),
                        signal: None,
                    };
                    write_response(&mut writer, &response);
                }
            }
            continue;
        }

        // If it doesn't parse as a ServerRequest, ignore it.
        eprintln!("phyl-tool-session: ignoring unrecognized input: {trimmed}");
    }

    // stdin closed — exit cleanly.
    eprintln!("phyl-tool-session: stdin closed, exiting");
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
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "ask_human");
        assert_eq!(specs[0].mode, ToolMode::Server);
        assert_eq!(specs[1].name, "done");
        assert_eq!(specs[1].mode, ToolMode::Server);
        assert!(specs[1].sandbox.is_none());
    }

    #[test]
    fn test_spec_serialization() {
        let specs = tool_specs();
        let json = serde_json::to_string_pretty(&specs).unwrap();
        assert!(json.contains("ask_human"));
        assert!(json.contains("done"));
        assert!(json.contains("End the current session"));

        // Verify it round-trips.
        let parsed: Vec<ToolSpec> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
    }
}

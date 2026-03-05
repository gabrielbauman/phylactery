use chrono::Utc;
use phyl_core::{ServerRequest, ServerResponse, ToolMode, ToolSpec};
use rusqlite::Connection;
use serde::Deserialize;
use std::io::{self, BufRead, Write};
use uuid::Uuid;

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
            name: "escalate".to_string(),
            description: "Escalate something to the human operator with structured metadata. \
                         Use for blocks, decisions needed, FYI notices, or capability requests. \
                         For 'blocked' and 'decision_required' kinds, the operator will be \
                         notified immediately via ask_human. The escalation is recorded in the \
                         psyche database for tracking."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "subject": {
                        "type": "string",
                        "description": "Short subject line for the escalation"
                    },
                    "body": {
                        "type": "string",
                        "description": "Full description of the escalation"
                    },
                    "urgency": {
                        "type": "string",
                        "enum": ["low", "normal", "high"],
                        "description": "Urgency level (default: normal)"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["blocked", "decision_required", "fyi", "request_capability"],
                        "description": "Type of escalation"
                    },
                    "concern_id": {
                        "type": "string",
                        "description": "Optional linked concern ID"
                    },
                    "commitment_id": {
                        "type": "string",
                        "description": "Optional linked commitment ID"
                    },
                    "blocking_action": {
                        "type": "string",
                        "description": "What action is blocked (for blocked kind)"
                    },
                    "proposed_resolution": {
                        "type": "string",
                        "description": "What you think should happen"
                    }
                },
                "required": ["subject", "body", "kind"]
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
                "escalate" => {
                    let response = handle_escalate(&req);
                    write_response(&mut writer, &response);
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

/// Handle the `escalate` tool — write escalation to psyche.db and return ID.
fn handle_escalate(req: &ServerRequest) -> ServerResponse {
    let subject = match req.arguments.get("subject").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some("missing required parameter: subject".to_string()),
                signal: None,
            };
        }
    };

    let body = match req.arguments.get("body").and_then(|v| v.as_str()) {
        Some(b) => b,
        None => {
            return ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some("missing required parameter: body".to_string()),
                signal: None,
            };
        }
    };

    let kind = match req.arguments.get("kind").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => {
            return ServerResponse {
                id: req.id.clone(),
                output: None,
                error: Some("missing required parameter: kind".to_string()),
                signal: None,
            };
        }
    };

    if !matches!(
        kind,
        "blocked" | "decision_required" | "fyi" | "request_capability"
    ) {
        return ServerResponse {
            id: req.id.clone(),
            output: None,
            error: Some(
                "kind must be 'blocked', 'decision_required', 'fyi', or 'request_capability'"
                    .to_string(),
            ),
            signal: None,
        };
    }

    let urgency = req
        .arguments
        .get("urgency")
        .and_then(|v| v.as_str())
        .unwrap_or("normal");
    let concern_id = req.arguments.get("concern_id").and_then(|v| v.as_str());
    let commitment_id = req.arguments.get("commitment_id").and_then(|v| v.as_str());
    let blocking_action = req
        .arguments
        .get("blocking_action")
        .and_then(|v| v.as_str());
    let proposed_resolution = req
        .arguments
        .get("proposed_resolution")
        .and_then(|v| v.as_str());

    let escalation_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    // Write to psyche.db if available.
    if let Some(db_path) = psyche_db_path() {
        match Connection::open(&db_path) {
            Ok(conn) => {
                let result = conn.execute(
                    "INSERT INTO escalations (escalation_id, subject, body, urgency, kind, \
                     concern_id, commitment_id, blocking_action, proposed_resolution, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        escalation_id,
                        subject,
                        body,
                        urgency,
                        kind,
                        concern_id,
                        commitment_id,
                        blocking_action,
                        proposed_resolution,
                        now,
                    ],
                );
                if let Err(e) = result {
                    eprintln!("phyl-tool-session: failed to write escalation to DB: {e}");
                }
            }
            Err(e) => {
                eprintln!("phyl-tool-session: failed to open psyche.db: {e}");
            }
        }
    }

    let output = serde_json::json!({
        "escalation_id": escalation_id,
        "subject": subject,
        "kind": kind,
        "urgency": urgency,
        "notify_operator": kind == "blocked" || kind == "decision_required",
    });

    ServerResponse {
        id: req.id.clone(),
        output: Some(serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())),
        error: None,
        signal: None,
    }
}

/// Get the path to psyche.db if PHYLACTERY_HOME is set.
fn psyche_db_path() -> Option<std::path::PathBuf> {
    std::env::var("PHYLACTERY_HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join("psyche.db"))
        .filter(|p| p.exists())
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
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].name, "ask_human");
        assert_eq!(specs[0].mode, ToolMode::Server);
        assert_eq!(specs[1].name, "escalate");
        assert_eq!(specs[1].mode, ToolMode::Server);
        assert_eq!(specs[2].name, "done");
        assert_eq!(specs[2].mode, ToolMode::Server);
    }

    #[test]
    fn test_spec_serialization() {
        let specs = tool_specs();
        let json = serde_json::to_string_pretty(&specs).unwrap();
        assert!(json.contains("ask_human"));
        assert!(json.contains("escalate"));
        assert!(json.contains("done"));
        assert!(json.contains("End the current session"));

        // Verify it round-trips.
        let parsed: Vec<ToolSpec> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn test_escalate_missing_params() {
        let req = ServerRequest {
            id: "1".to_string(),
            name: "escalate".to_string(),
            arguments: serde_json::json!({}),
        };
        let resp = handle_escalate(&req);
        assert!(resp.error.is_some());
        assert!(resp.error.unwrap().contains("subject"));
    }

    #[test]
    fn test_escalate_invalid_kind() {
        let req = ServerRequest {
            id: "1".to_string(),
            name: "escalate".to_string(),
            arguments: serde_json::json!({
                "subject": "test",
                "body": "test body",
                "kind": "invalid"
            }),
        };
        let resp = handle_escalate(&req);
        assert!(resp.error.is_some());
    }

    #[test]
    fn test_escalate_valid_fyi() {
        let req = ServerRequest {
            id: "1".to_string(),
            name: "escalate".to_string(),
            arguments: serde_json::json!({
                "subject": "Status update",
                "body": "Everything is going well",
                "kind": "fyi"
            }),
        };
        let resp = handle_escalate(&req);
        assert!(resp.error.is_none());
        let output: serde_json::Value =
            serde_json::from_str(resp.output.as_ref().unwrap()).unwrap();
        assert!(output.get("escalation_id").is_some());
        assert_eq!(output["kind"], "fyi");
        assert_eq!(output["notify_operator"], false);
    }

    #[test]
    fn test_escalate_blocked_notifies() {
        let req = ServerRequest {
            id: "1".to_string(),
            name: "escalate".to_string(),
            arguments: serde_json::json!({
                "subject": "Can't deploy",
                "body": "Need access to production cluster",
                "kind": "blocked",
                "urgency": "high"
            }),
        };
        let resp = handle_escalate(&req);
        assert!(resp.error.is_none());
        let output: serde_json::Value =
            serde_json::from_str(resp.output.as_ref().unwrap()).unwrap();
        assert_eq!(output["notify_operator"], true);
        assert_eq!(output["urgency"], "high");
    }
}

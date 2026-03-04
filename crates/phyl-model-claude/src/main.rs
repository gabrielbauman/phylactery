use phyl_core::{Message, ModelRequest, ModelResponse, Role, ToolCall, ToolSpec};
use serde::Deserialize;
use std::io::{self, Read, Write};
use std::process::Command;
use uuid::Uuid;

/// Tag markers for structured tool calls in model output.
const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";

fn main() {
    // Read ModelRequest from stdin.
    let mut input_str = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input_str) {
        eprintln!("phyl-model-claude: failed to read stdin: {e}");
        std::process::exit(1);
    }

    let request: ModelRequest = match serde_json::from_str(&input_str) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("phyl-model-claude: invalid ModelRequest JSON: {e}");
            std::process::exit(1);
        }
    };

    let response = run_model(&request);
    match serde_json::to_string(&response) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("phyl-model-claude: failed to serialize response: {e}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt construction
// ---------------------------------------------------------------------------

/// Build the system prompt from system messages and tool definitions.
fn build_system_prompt(request: &ModelRequest) -> String {
    let mut prompt = String::new();

    // Collect system messages.
    for msg in &request.messages {
        if msg.role == Role::System {
            if !prompt.is_empty() {
                prompt.push_str("\n\n");
            }
            prompt.push_str(&msg.content);
        }
    }

    // Append tool definitions if any tools are provided.
    if !request.tools.is_empty() {
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(&format_tool_definitions(&request.tools));
    }

    prompt
}

/// Format tool definitions as instructions for the model.
///
/// Tools are described in a structured format with explicit constraints and
/// instructions for how the model should express tool calls (using `<tool_call>`
/// XML tags). The prompt is deliberately firm to prevent the model from drifting
/// toward native tool-calling patterns or hallucinating tools.
fn format_tool_definitions(tools: &[ToolSpec]) -> String {
    let mut s = String::new();
    s.push_str("# Tools\n\n");
    s.push_str(
        "You have access to ONLY the tools listed below. \
         Do not attempt to perform actions by any other means. \
         Do not invent or hallucinate tool names — only call tools defined here.\n\n",
    );
    s.push_str("To call a tool, output this exact XML format:\n\n");
    s.push_str("<tool_call>\n");
    s.push_str("{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n");
    s.push_str("</tool_call>\n\n");
    s.push_str("Rules:\n");
    s.push_str("- Use ONLY the <tool_call> XML format shown above. No other format is accepted.\n");
    s.push_str(
        "- You may call multiple tools in one response \
         by including multiple <tool_call> blocks.\n",
    );
    s.push_str(
        "- When you call a tool, STOP and wait for results. \
         Do not predict, fabricate, or assume tool output.\n",
    );
    s.push_str("- Include brief reasoning before tool calls, not after.\n");
    s.push_str(
        "- If a task cannot be done with these tools, \
         say so instead of attempting workarounds.\n\n",
    );

    for tool in tools {
        s.push_str(&format!("## {}\n\n", tool.name));
        s.push_str(&format!("{}\n\n", tool.description));
        s.push_str("Parameters:\n```json\n");
        s.push_str(
            &serde_json::to_string_pretty(&tool.parameters).unwrap_or_else(|_| "{}".to_string()),
        );
        s.push_str("\n```\n\n");
    }

    s
}

/// Format conversation history (non-system messages) as a user prompt.
///
/// If there is only one non-system message, its content is returned directly.
/// For multi-turn conversations, earlier messages are wrapped in
/// `<conversation_history>` tags and the final message is appended as the
/// current turn.
fn build_user_prompt(messages: &[Message]) -> String {
    let conversation: Vec<&Message> = messages.iter().filter(|m| m.role != Role::System).collect();

    if conversation.is_empty() {
        return String::new();
    }

    // Single message: return its content directly.
    if conversation.len() == 1 {
        return conversation[0].content.clone();
    }

    let mut prompt = String::new();
    let last_idx = conversation.len() - 1;

    // Format history (all messages except the last).
    prompt.push_str("<conversation_history>\n");
    for msg in &conversation[..last_idx] {
        format_message(&mut prompt, msg);
    }
    prompt.push_str("</conversation_history>\n\n");

    // The last message is the current turn.
    prompt.push_str(&conversation[last_idx].content);

    prompt
}

/// Append a formatted representation of a message to `out`.
fn format_message(out: &mut String, msg: &Message) {
    match msg.role {
        Role::User => {
            out.push_str(&format!("[User]: {}\n\n", msg.content));
        }
        Role::Assistant => {
            if !msg.content.is_empty() {
                out.push_str(&format!("[Assistant]: {}\n\n", msg.content));
            }
            for tc in &msg.tool_calls {
                out.push_str(&format!(
                    "[Assistant called tool: {}({})]\n\n",
                    tc.name,
                    serde_json::to_string(&tc.arguments).unwrap_or_default()
                ));
            }
        }
        Role::Tool => {
            out.push_str(&format!(
                "[Tool result (id: {})]: {}\n\n",
                msg.tool_call_id.as_deref().unwrap_or("?"),
                msg.content
            ));
        }
        Role::System => {} // Already handled in build_system_prompt.
    }
}

// ---------------------------------------------------------------------------
// Claude CLI invocation
// ---------------------------------------------------------------------------

/// Response JSON from `claude --print --output-format json`.
///
/// The claude CLI returns a JSON object with these fields. We only require
/// `result`; everything else is optional metadata.
#[derive(Debug, Deserialize)]
struct ClaudeCliResponse {
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    is_error: bool,
    // Below fields are captured for potential future use / logging.
    #[serde(default)]
    #[allow(dead_code)]
    session_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    num_turns: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    cost_usd: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    duration_ms: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    duration_api_ms: Option<u64>,
}

/// Invoke the claude CLI and return a `ModelResponse`.
///
/// Environment variables:
/// - `PHYL_CLAUDE_CLI`: path to the claude binary (default: `"claude"`)
/// - `PHYL_CLAUDE_MODEL`: model name to pass via `--model` (optional)
fn run_model(request: &ModelRequest) -> ModelResponse {
    let cli = std::env::var("PHYL_CLAUDE_CLI").unwrap_or_else(|_| "claude".to_string());
    let system_prompt = build_system_prompt(request);
    let user_prompt = build_user_prompt(&request.messages);

    let mut cmd = Command::new(&cli);
    cmd.arg("--print")
        .arg("--output-format")
        .arg("json")
        .arg("--no-session-persistence")
        .arg("--tools")
        .arg("") // Disable all built-in tools; ours are in the system prompt.
        .arg("--strict-mcp-config") // Ignore user's MCP servers; only our prompt-defined tools.
        .arg("--disable-slash-commands") // Disable skills that could interfere.
        .arg("--system-prompt")
        .arg(&system_prompt);

    // Optional model override.
    if let Ok(model) = std::env::var("PHYL_CLAUDE_MODEL") {
        cmd.arg("--model").arg(&model);
    }

    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Unset CLAUDECODE env var to avoid the nested-session guard when the
    // model adapter is invoked from within a Claude Code session.
    cmd.env_remove("CLAUDECODE");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("phyl-model-claude: failed to spawn claude CLI ({cli}): {e}");
            return ModelResponse {
                content: format!("Error: failed to spawn claude CLI: {e}"),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    // Write user prompt to stdin, then drop the handle to close the pipe.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(user_prompt.as_bytes());
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("phyl-model-claude: failed to wait for claude CLI: {e}");
            return ModelResponse {
                content: format!("Error: failed to wait for claude CLI: {e}"),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    // Log stderr for debugging.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        eprintln!("phyl-model-claude: claude CLI stderr: {stderr}");
    }

    if !output.status.success() {
        return ModelResponse {
            content: format!(
                "Error: claude CLI exited with status {}. stderr: {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ),
            tool_calls: vec![],
            usage: None,
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_claude_response(&stdout)
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Parse the claude CLI JSON response into a `ModelResponse`.
fn parse_claude_response(raw: &str) -> ModelResponse {
    let cli_resp: ClaudeCliResponse = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return ModelResponse {
                content: format!(
                    "Error: failed to parse claude CLI response: {e}\nRaw output: {raw}"
                ),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    if cli_resp.is_error {
        return ModelResponse {
            content: cli_resp
                .result
                .unwrap_or_else(|| "Unknown error from claude CLI".to_string()),
            tool_calls: vec![],
            usage: None,
        };
    }

    let result_text = cli_resp.result.unwrap_or_default();
    let (content, tool_calls) = extract_tool_calls(&result_text);

    ModelResponse {
        content,
        tool_calls,
        usage: None, // The claude CLI JSON output doesn't expose token counts.
    }
}

/// Extract `<tool_call>` blocks from the model's response text.
///
/// Returns the text content (with tool_call blocks removed) and a list of
/// parsed `ToolCall` values. Each tool call gets a unique UUID-based ID.
///
/// When tool calls are extracted, any hallucinated `<tool_result>` or
/// `<tool_output>` blocks are also stripped from the content, since the
/// model sometimes fabricates results instead of waiting for execution.
fn extract_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    let mut content = String::new();
    let mut tool_calls = Vec::new();

    let mut remaining = text;
    while let Some(start) = remaining.find(TOOL_CALL_OPEN) {
        // Text before the opening tag is content.
        content.push_str(&remaining[..start]);

        let after_open = &remaining[start + TOOL_CALL_OPEN.len()..];
        if let Some(end) = after_open.find(TOOL_CALL_CLOSE) {
            let call_json = after_open[..end].trim();

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(call_json)
                && let (Some(name), Some(arguments)) = (
                    parsed.get("name").and_then(|v| v.as_str()),
                    parsed.get("arguments"),
                )
            {
                tool_calls.push(ToolCall {
                    id: format!("tc_{}", Uuid::new_v4()),
                    name: name.to_string(),
                    arguments: arguments.clone(),
                });
            }

            remaining = &after_open[end + TOOL_CALL_CLOSE.len()..];
        } else {
            // Malformed: no closing tag. Keep the rest as content.
            content.push_str(&remaining[start..]);
            remaining = "";
        }
    }
    content.push_str(remaining);

    let mut content = content.trim().to_string();

    // Strip hallucinated tool result/output blocks when tool calls were found.
    if !tool_calls.is_empty() {
        content = strip_xml_blocks(&content, "<tool_result>", "</tool_result>");
        content = strip_xml_blocks(&content, "<tool_output>", "</tool_output>");
        content = content.trim().to_string();
    }

    (content, tool_calls)
}

/// Remove all occurrences of `<open_tag>...</close_tag>` from `text`.
fn strip_xml_blocks(text: &str, open_tag: &str, close_tag: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find(open_tag) {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start + open_tag.len()..];
        if let Some(end) = after_open.find(close_tag) {
            remaining = &after_open[end + close_tag.len()..];
        } else {
            // No closing tag — keep the rest.
            result.push_str(&remaining[start..]);
            remaining = "";
        }
    }
    result.push_str(remaining);
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phyl_core::{ModelRequest, ToolMode, ToolSpec};

    // -- extract_tool_calls --

    #[test]
    fn extract_no_calls() {
        let (content, calls) = extract_tool_calls("Just some plain text.");
        assert_eq!(content, "Just some plain text.");
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_single_call() {
        let text = "Let me check.\n<tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>\nDone.";
        let (content, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments["command"], "ls");
        assert!(calls[0].id.starts_with("tc_"));
        assert!(content.contains("Let me check."));
        assert!(content.contains("Done."));
        assert!(!content.contains("tool_call"));
    }

    #[test]
    fn extract_multiple_calls() {
        let text = "I'll do two things.\n\
            <tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool_call>\n\
            <tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"foo.txt\"}}</tool_call>";
        let (content, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[1].name, "read_file");
        assert!(content.starts_with("I'll do two things."));
    }

    #[test]
    fn extract_only_tool_calls() {
        let text =
            "<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"pwd\"}}</tool_call>";
        let (content, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert!(content.is_empty());
    }

    #[test]
    fn extract_malformed_no_closing_tag() {
        let text = "Before <tool_call>{\"name\": \"bash\"} and after";
        let (content, calls) = extract_tool_calls(text);
        assert!(calls.is_empty());
        // The malformed tag and everything after it becomes content.
        assert!(content.contains("<tool_call>"));
    }

    #[test]
    fn extract_invalid_json_in_tag() {
        let text = "<tool_call>not valid json</tool_call> rest";
        let (content, calls) = extract_tool_calls(text);
        assert!(calls.is_empty());
        // The tag is consumed but the invalid call is dropped.
        assert!(content.contains("rest"));
    }

    #[test]
    fn extract_missing_fields_in_json() {
        // Valid JSON but missing required fields.
        let text = "<tool_call>{\"foo\": \"bar\"}</tool_call>";
        let (content, calls) = extract_tool_calls(text);
        assert!(calls.is_empty());
        assert!(content.is_empty());
    }

    #[test]
    fn extract_strips_hallucinated_tool_result() {
        let text = "Let me check.\n\
            <tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"df -h\"}}</tool_call>\n\
            <tool_result>\nFake output here\n</tool_result>\n\
            You have 100GB free.";
        let (content, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert!(!content.contains("tool_result"));
        assert!(!content.contains("Fake output"));
        assert!(content.contains("Let me check."));
        assert!(content.contains("You have 100GB free."));
    }

    #[test]
    fn extract_strips_hallucinated_tool_output() {
        let text = "<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool_call>\n\
            <tool_output>file1.txt\nfile2.txt</tool_output>";
        let (content, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert!(!content.contains("tool_output"));
        assert!(!content.contains("file1.txt"));
    }

    #[test]
    fn extract_no_strip_without_tool_calls() {
        // tool_result without any tool_call should NOT be stripped.
        let text = "<tool_result>some data</tool_result>";
        let (content, calls) = extract_tool_calls(text);
        assert!(calls.is_empty());
        assert!(content.contains("<tool_result>"));
    }

    // -- build_system_prompt --

    #[test]
    fn system_prompt_no_tools() {
        let request = ModelRequest {
            messages: vec![Message {
                role: Role::System,
                content: "You are helpful.".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![],
        };
        let prompt = build_system_prompt(&request);
        assert_eq!(prompt, "You are helpful.");
    }

    #[test]
    fn system_prompt_with_tools() {
        let request = ModelRequest {
            messages: vec![Message {
                role: Role::System,
                content: "You are helpful.".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![ToolSpec {
                name: "bash".to_string(),
                description: "Execute a shell command".to_string(),
                mode: ToolMode::Oneshot,
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }),
                sandbox: None,
            }],
        };
        let prompt = build_system_prompt(&request);
        assert!(prompt.contains("You are helpful."));
        assert!(prompt.contains("# Tools"));
        assert!(prompt.contains("ONLY the tools listed below"));
        assert!(prompt.contains("Do not invent or hallucinate tool names"));
        assert!(prompt.contains("No other format is accepted"));
        assert!(prompt.contains("## bash"));
        assert!(prompt.contains("Execute a shell command"));
        assert!(prompt.contains("<tool_call>"));
    }

    #[test]
    fn system_prompt_multiple_system_messages() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "First system message.".to_string(),
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
                    role: Role::System,
                    content: "Second system message.".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![],
        };
        let prompt = build_system_prompt(&request);
        assert!(prompt.contains("First system message."));
        assert!(prompt.contains("Second system message."));
        assert!(!prompt.contains("Hello"));
    }

    // -- build_user_prompt --

    #[test]
    fn user_prompt_single_message() {
        let messages = vec![Message {
            role: Role::User,
            content: "What is 2+2?".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        }];
        let prompt = build_user_prompt(&messages);
        assert_eq!(prompt, "What is 2+2?");
    }

    #[test]
    fn user_prompt_skips_system() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "System prompt.".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: "Hello".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];
        let prompt = build_user_prompt(&messages);
        assert_eq!(prompt, "Hello");
        assert!(!prompt.contains("System"));
    }

    #[test]
    fn user_prompt_multi_turn() {
        let messages = vec![
            Message {
                role: Role::User,
                content: "Hello".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::Assistant,
                content: "Hi there!".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: "What is 2+2?".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];
        let prompt = build_user_prompt(&messages);
        assert!(prompt.contains("<conversation_history>"));
        assert!(prompt.contains("[User]: Hello"));
        assert!(prompt.contains("[Assistant]: Hi there!"));
        assert!(prompt.contains("</conversation_history>"));
        assert!(prompt.ends_with("What is 2+2?"));
    }

    #[test]
    fn user_prompt_with_tool_calls_and_results() {
        let messages = vec![
            Message {
                role: Role::User,
                content: "List files".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            Message {
                role: Role::Assistant,
                content: "Let me check.".to_string(),
                tool_calls: vec![ToolCall {
                    id: "tc_1".to_string(),
                    name: "bash".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }],
                tool_call_id: None,
            },
            Message {
                role: Role::Tool,
                content: "file1.txt\nfile2.txt".to_string(),
                tool_calls: vec![],
                tool_call_id: Some("tc_1".to_string()),
            },
            Message {
                role: Role::User,
                content: "Now what?".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];
        let prompt = build_user_prompt(&messages);
        assert!(prompt.contains("[User]: List files"));
        assert!(prompt.contains("[Assistant]: Let me check."));
        assert!(prompt.contains("[Assistant called tool: bash("));
        assert!(prompt.contains("[Tool result (id: tc_1)]: file1.txt"));
        assert!(prompt.ends_with("Now what?"));
    }

    #[test]
    fn user_prompt_empty() {
        let messages: Vec<Message> = vec![];
        let prompt = build_user_prompt(&messages);
        assert!(prompt.is_empty());
    }

    // -- parse_claude_response --

    #[test]
    fn parse_success_no_tools() {
        let raw = r#"{"result": "Hello there!", "is_error": false}"#;
        let resp = parse_claude_response(raw);
        assert_eq!(resp.content, "Hello there!");
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn parse_success_with_tool_calls() {
        let raw = r#"{"result": "Let me check.\n<tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>", "is_error": false}"#;
        let resp = parse_claude_response(raw);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "bash");
        assert!(resp.content.contains("Let me check."));
    }

    #[test]
    fn parse_error_response() {
        let raw = r#"{"result": "Something went wrong", "is_error": true}"#;
        let resp = parse_claude_response(raw);
        assert_eq!(resp.content, "Something went wrong");
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn parse_invalid_json() {
        let raw = "not json at all";
        let resp = parse_claude_response(raw);
        assert!(resp.content.contains("Error: failed to parse"));
    }

    #[test]
    fn parse_missing_result() {
        let raw = r#"{"is_error": false}"#;
        let resp = parse_claude_response(raw);
        assert!(resp.content.is_empty());
        assert!(resp.tool_calls.is_empty());
    }
}

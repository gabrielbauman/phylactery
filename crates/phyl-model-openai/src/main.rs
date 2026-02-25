use phyl_core::{ModelRequest, ModelResponse, Role, ToolCall, ToolSpec, Usage};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use uuid::Uuid;

/// Tag markers for structured tool calls when using XML mode.
const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";

fn main() {
    let mut input_str = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input_str) {
        eprintln!("phyl-model-openai: failed to read stdin: {e}");
        std::process::exit(1);
    }

    let request: ModelRequest = match serde_json::from_str(&input_str) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("phyl-model-openai: invalid ModelRequest JSON: {e}");
            std::process::exit(1);
        }
    };

    let response = run_model(&request);
    match serde_json::to_string(&response) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("phyl-model-openai: failed to serialize response: {e}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration from environment
// ---------------------------------------------------------------------------

struct AdapterConfig {
    /// Base URL of the OpenAI-compatible API (no trailing slash).
    base_url: String,
    /// Model name to request.
    model: String,
    /// API key (empty string if unset).
    api_key: String,
    /// Whether to use native OpenAI tool calling vs XML-in-prompt.
    native_tools: bool,
    /// Request timeout in seconds.
    timeout_secs: u64,
}

fn load_config() -> AdapterConfig {
    let base_url = std::env::var("PHYL_OPENAI_URL")
        .unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
    let model = std::env::var("PHYL_OPENAI_MODEL").unwrap_or_else(|_| "gemma3n".to_string());
    let api_key = std::env::var("PHYL_OPENAI_API_KEY").unwrap_or_default();
    let native_tools = std::env::var("PHYL_OPENAI_TOOL_MODE")
        .map(|v| v == "native")
        .unwrap_or(false);
    let timeout_secs = std::env::var("PHYL_OPENAI_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);

    AdapterConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        model,
        api_key,
        native_tools,
        timeout_secs,
    }
}

// ---------------------------------------------------------------------------
// OpenAI API types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ChatFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// Build chat messages for native OpenAI tool-calling mode.
///
/// Messages map directly to OpenAI roles. Tool definitions are passed via the
/// `tools` parameter rather than embedded in the system prompt.
fn build_native_messages(request: &ModelRequest) -> (Vec<ChatMessage>, Vec<ChatTool>) {
    let mut messages = Vec::new();

    // Collect system messages into one.
    let system_text: String = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    if !system_text.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system_text),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    // Convert conversation messages.
    for msg in &request.messages {
        match msg.role {
            Role::System => {} // Already handled above.
            Role::User => {
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(msg.content.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            Role::Assistant => {
                let tool_calls = if msg.tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        msg.tool_calls
                            .iter()
                            .map(|tc| ChatToolCall {
                                id: tc.id.clone(),
                                call_type: "function".to_string(),
                                function: ChatFunctionCall {
                                    name: tc.name.clone(),
                                    arguments: serde_json::to_string(&tc.arguments)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                },
                            })
                            .collect(),
                    )
                };
                let content = if msg.content.is_empty() {
                    None
                } else {
                    Some(msg.content.clone())
                };
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content,
                    tool_calls,
                    tool_call_id: None,
                });
            }
            Role::Tool => {
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(msg.content.clone()),
                    tool_calls: None,
                    tool_call_id: msg.tool_call_id.clone(),
                });
            }
        }
    }

    // Build tool definitions.
    let tools: Vec<ChatTool> = request
        .tools
        .iter()
        .map(|t| ChatTool {
            tool_type: "function".to_string(),
            function: ChatFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect();

    (messages, tools)
}

/// Build chat messages for XML tool-calling mode.
///
/// Tool definitions are embedded in the system prompt as instructions. All
/// messages are sent as user/assistant roles (tool results are formatted
/// inline). This works with any model, even those without native tool support.
fn build_xml_messages(request: &ModelRequest) -> Vec<ChatMessage> {
    let mut messages = Vec::new();

    // Build system prompt with tool definitions baked in.
    let mut system_text: String = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    if !request.tools.is_empty() {
        if !system_text.is_empty() {
            system_text.push_str("\n\n");
        }
        system_text.push_str(&format_tool_definitions(&request.tools));
    }

    if !system_text.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system_text),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    // Convert conversation messages, flattening tool calls/results into text.
    for msg in &request.messages {
        match msg.role {
            Role::System => {} // Already in the system message.
            Role::User => {
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(msg.content.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            Role::Assistant => {
                let mut text = msg.content.clone();
                for tc in &msg.tool_calls {
                    text.push_str(&format!(
                        "\n{TOOL_CALL_OPEN}\n{}\n{TOOL_CALL_CLOSE}",
                        serde_json::json!({"name": tc.name, "arguments": tc.arguments})
                    ));
                }
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            Role::Tool => {
                // Tool results become user messages with context.
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(format!(
                        "[Tool result (id: {})]: {}",
                        msg.tool_call_id.as_deref().unwrap_or("?"),
                        msg.content
                    )),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }
    }

    messages
}

/// Format tool definitions as instructions for XML mode.
fn format_tool_definitions(tools: &[ToolSpec]) -> String {
    let mut s = String::new();
    s.push_str("# Available Tools\n\n");
    s.push_str("You have access to the following tools. ");
    s.push_str("To call a tool, use this exact XML format:\n\n");
    s.push_str("<tool_call>\n");
    s.push_str("{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n");
    s.push_str("</tool_call>\n\n");
    s.push_str("You may call multiple tools in a single response. ");
    s.push_str("Include your reasoning as plain text outside the tool_call tags.\n\n");

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

// ---------------------------------------------------------------------------
// API invocation
// ---------------------------------------------------------------------------

fn run_model(request: &ModelRequest) -> ModelResponse {
    let config = load_config();
    let url = format!("{}/chat/completions", config.base_url);

    let chat_request = if config.native_tools && !request.tools.is_empty() {
        let (messages, tools) = build_native_messages(request);
        ChatRequest {
            model: config.model.clone(),
            messages,
            tools: Some(tools),
            temperature: None,
        }
    } else {
        ChatRequest {
            model: config.model.clone(),
            messages: build_xml_messages(request),
            tools: None,
            temperature: None,
        }
    };

    eprintln!(
        "phyl-model-openai: POST {} (model={}, native_tools={}, messages={})",
        url,
        config.model,
        config.native_tools,
        chat_request.messages.len()
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_read(std::time::Duration::from_secs(config.timeout_secs))
        .timeout_write(std::time::Duration::from_secs(30))
        .build();

    let mut req = agent.post(&url).set("Content-Type", "application/json");

    if !config.api_key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", config.api_key));
    }

    let body = match serde_json::to_string(&chat_request) {
        Ok(b) => b,
        Err(e) => {
            return ModelResponse {
                content: format!("Error: failed to serialize request: {e}"),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    let resp = match req.send_string(&body) {
        Ok(r) => r,
        Err(e) => {
            return ModelResponse {
                content: format!("Error: API request failed: {e}"),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    let resp_body = match resp.into_string() {
        Ok(s) => s,
        Err(e) => {
            return ModelResponse {
                content: format!("Error: failed to read response body: {e}"),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    let chat_resp: ChatResponse = match serde_json::from_str(&resp_body) {
        Ok(r) => r,
        Err(e) => {
            return ModelResponse {
                content: format!(
                    "Error: failed to parse API response: {e}\nRaw: {}",
                    truncate(&resp_body, 500)
                ),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    parse_response(&chat_resp, config.native_tools)
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

fn parse_response(resp: &ChatResponse, native_tools: bool) -> ModelResponse {
    let choice = match resp.choices.first() {
        Some(c) => c,
        None => {
            return ModelResponse {
                content: "Error: API returned no choices".to_string(),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    let usage = resp.usage.as_ref().map(|u| Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
    });

    let raw_content = choice.message.content.clone().unwrap_or_default();

    if native_tools {
        // Extract tool calls from the native API response.
        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|tcs| {
                tcs.iter()
                    .filter_map(|tc| {
                        let args: serde_json::Value =
                            serde_json::from_str(&tc.function.arguments).ok()?;
                        Some(ToolCall {
                            id: if tc.id.is_empty() {
                                format!("tc_{}", Uuid::new_v4())
                            } else {
                                tc.id.clone()
                            },
                            name: tc.function.name.clone(),
                            arguments: args,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        ModelResponse {
            content: raw_content,
            tool_calls,
            usage,
        }
    } else {
        // XML mode: parse <tool_call> tags from the content.
        let (content, tool_calls) = extract_tool_calls(&raw_content);
        ModelResponse {
            content,
            tool_calls,
            usage,
        }
    }
}

/// Extract `<tool_call>` blocks from model text output.
///
/// Returns the cleaned text content and parsed tool calls.
fn extract_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    let mut content = String::new();
    let mut tool_calls = Vec::new();

    let mut remaining = text;
    while let Some(start) = remaining.find(TOOL_CALL_OPEN) {
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
            content.push_str(&remaining[start..]);
            remaining = "";
        }
    }
    content.push_str(remaining);

    (content.trim().to_string(), tool_calls)
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phyl_core::{Message, ModelRequest, ToolMode, ToolSpec};

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
    }

    #[test]
    fn extract_multiple_calls() {
        let text = "I'll do two things.\n\
            <tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool_call>\n\
            <tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"foo.txt\"}}</tool_call>";
        let (_content, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[1].name, "read_file");
    }

    // -- build_xml_messages --

    #[test]
    fn xml_messages_include_tools_in_system() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "You are helpful.".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::User,
                    content: "Hello".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![ToolSpec {
                name: "bash".to_string(),
                description: "Execute a shell command".to_string(),
                mode: ToolMode::Oneshot,
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    }
                }),
                sandbox: None,
            }],
        };
        let messages = build_xml_messages(&request);
        assert_eq!(messages.len(), 2); // system + user
        let sys = messages[0].content.as_deref().unwrap();
        assert!(sys.contains("You are helpful."));
        assert!(sys.contains("# Available Tools"));
        assert!(sys.contains("## bash"));
        assert!(sys.contains("<tool_call>"));
    }

    #[test]
    fn xml_messages_flatten_tool_results() {
        let request = ModelRequest {
            messages: vec![
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
                    content: "file1.txt".to_string(),
                    tool_calls: vec![],
                    tool_call_id: Some("tc_1".to_string()),
                },
            ],
            tools: vec![],
        };
        let messages = build_xml_messages(&request);
        assert_eq!(messages.len(), 3); // user, assistant, user (tool result)

        // Assistant message should include the tool call XML.
        let assistant = messages[1].content.as_deref().unwrap();
        assert!(assistant.contains("Let me check."));
        assert!(assistant.contains("<tool_call>"));
        assert!(assistant.contains("bash"));

        // Tool result becomes a user message.
        assert_eq!(messages[2].role, "user");
        assert!(
            messages[2]
                .content
                .as_deref()
                .unwrap()
                .contains("file1.txt")
        );
        assert!(
            messages[2]
                .content
                .as_deref()
                .unwrap()
                .contains("Tool result")
        );
    }

    // -- build_native_messages --

    #[test]
    fn native_messages_structure() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "You are helpful.".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::User,
                    content: "Hello".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![ToolSpec {
                name: "bash".to_string(),
                description: "Execute a shell command".to_string(),
                mode: ToolMode::Oneshot,
                parameters: serde_json::json!({}),
                sandbox: None,
            }],
        };
        let (messages, tools) = build_native_messages(&request);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "bash");
    }

    #[test]
    fn native_messages_preserve_tool_history() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::User,
                    content: "Do it".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Assistant,
                    content: "".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "tc_1".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": "ls"}),
                    }],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Tool,
                    content: "output".to_string(),
                    tool_calls: vec![],
                    tool_call_id: Some("tc_1".to_string()),
                },
            ],
            tools: vec![],
        };
        let (messages, _) = build_native_messages(&request);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, "assistant");
        assert!(messages[1].tool_calls.is_some());
        assert_eq!(messages[2].role, "tool");
        assert_eq!(messages[2].tool_call_id, Some("tc_1".to_string()));
    }

    // -- parse_response --

    #[test]
    fn parse_xml_response() {
        let resp = ChatResponse {
            choices: vec![ChatChoice {
                message: ChatResponseMessage {
                    content: Some(
                        "Let me check.\n<tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>"
                            .to_string(),
                    ),
                    tool_calls: None,
                },
            }],
            usage: Some(ChatUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
            }),
        };

        let model_resp = parse_response(&resp, false);
        assert!(model_resp.content.contains("Let me check."));
        assert_eq!(model_resp.tool_calls.len(), 1);
        assert_eq!(model_resp.tool_calls[0].name, "bash");
        assert_eq!(model_resp.usage.as_ref().unwrap().input_tokens, 100);
        assert_eq!(model_resp.usage.as_ref().unwrap().output_tokens, 50);
    }

    #[test]
    fn parse_native_response() {
        let resp = ChatResponse {
            choices: vec![ChatChoice {
                message: ChatResponseMessage {
                    content: Some("I'll list the files.".to_string()),
                    tool_calls: Some(vec![ChatToolCall {
                        id: "call_123".to_string(),
                        call_type: "function".to_string(),
                        function: ChatFunctionCall {
                            name: "bash".to_string(),
                            arguments: "{\"command\": \"ls\"}".to_string(),
                        },
                    }]),
                },
            }],
            usage: None,
        };

        let model_resp = parse_response(&resp, true);
        assert_eq!(model_resp.content, "I'll list the files.");
        assert_eq!(model_resp.tool_calls.len(), 1);
        assert_eq!(model_resp.tool_calls[0].id, "call_123");
        assert_eq!(model_resp.tool_calls[0].name, "bash");
        assert_eq!(model_resp.tool_calls[0].arguments["command"], "ls");
        assert!(model_resp.usage.is_none());
    }

    #[test]
    fn parse_empty_choices() {
        let resp = ChatResponse {
            choices: vec![],
            usage: None,
        };
        let model_resp = parse_response(&resp, false);
        assert!(model_resp.content.contains("Error"));
    }
}

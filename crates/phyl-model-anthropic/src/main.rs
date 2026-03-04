use phyl_core::{ModelRequest, ModelResponse, Role, ToolCall, Usage};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};

fn main() {
    let mut input_str = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input_str) {
        eprintln!("phyl-model-anthropic: failed to read stdin: {e}");
        std::process::exit(1);
    }

    let request: ModelRequest = match serde_json::from_str(&input_str) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("phyl-model-anthropic: invalid ModelRequest JSON: {e}");
            std::process::exit(1);
        }
    };

    let response = run_model(&request);
    match serde_json::to_string(&response) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("phyl-model-anthropic: failed to serialize response: {e}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration from environment
// ---------------------------------------------------------------------------

struct AdapterConfig {
    base_url: String,
    model: String,
    api_key: String,
    max_tokens: u64,
    timeout_secs: u64,
}

fn load_config() -> AdapterConfig {
    let base_url = std::env::var("PHYL_ANTHROPIC_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
    let model =
        std::env::var("PHYL_ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
    let api_key = std::env::var("PHYL_ANTHROPIC_API_KEY").unwrap_or_default();
    let max_tokens = std::env::var("PHYL_ANTHROPIC_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8192);
    let timeout_secs = std::env::var("PHYL_ANTHROPIC_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);

    AdapterConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        model,
        api_key,
        max_tokens,
        timeout_secs,
    }
}

// ---------------------------------------------------------------------------
// Anthropic API types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ResponseContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct AnthropicError {
    #[serde(default)]
    error: Option<AnthropicErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    #[serde(default)]
    message: String,
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

fn build_request(request: &ModelRequest, config: &AdapterConfig) -> AnthropicRequest {
    // Extract system messages into the top-level system field.
    let system_text: String = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let system = if system_text.is_empty() {
        None
    } else {
        Some(system_text)
    };

    // Convert conversation messages, merging adjacent same-role messages.
    let mut messages: Vec<AnthropicMessage> = Vec::new();

    for msg in &request.messages {
        match msg.role {
            Role::System => {} // Handled above.
            Role::User => {
                let block = ContentBlock::Text {
                    text: msg.content.clone(),
                };
                merge_or_push(&mut messages, "user", block);
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                if !msg.content.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: msg.content.clone(),
                    });
                }
                for tc in &msg.tool_calls {
                    blocks.push(ContentBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input: tc.arguments.clone(),
                    });
                }
                if blocks.is_empty() {
                    // Empty assistant message — use empty text to avoid protocol error.
                    blocks.push(ContentBlock::Text {
                        text: String::new(),
                    });
                }
                merge_or_push_blocks(&mut messages, "assistant", blocks);
            }
            Role::Tool => {
                let block = ContentBlock::ToolResult {
                    tool_use_id: msg.tool_call_id.clone().unwrap_or_default(),
                    content: msg.content.clone(),
                };
                merge_or_push(&mut messages, "user", block);
            }
        }
    }

    // Build tool definitions.
    let tools: Vec<AnthropicTool> = request
        .tools
        .iter()
        .map(|t| AnthropicTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        })
        .collect();

    AnthropicRequest {
        model: config.model.clone(),
        max_tokens: config.max_tokens,
        system,
        messages,
        tools,
    }
}

/// Merge a single content block into the last message if it has the same role,
/// or push a new message.
fn merge_or_push(messages: &mut Vec<AnthropicMessage>, role: &str, block: ContentBlock) {
    if let Some(last) = messages.last_mut()
        && last.role == role
    {
        match &mut last.content {
            AnthropicContent::Blocks(blocks) => blocks.push(block),
            AnthropicContent::Text(text) => {
                let existing = ContentBlock::Text {
                    text: std::mem::take(text),
                };
                last.content = AnthropicContent::Blocks(vec![existing, block]);
            }
        }
    } else {
        messages.push(AnthropicMessage {
            role: role.to_string(),
            content: AnthropicContent::Blocks(vec![block]),
        });
    }
}

/// Merge multiple content blocks into the last message if it has the same role,
/// or push a new message.
fn merge_or_push_blocks(
    messages: &mut Vec<AnthropicMessage>,
    role: &str,
    blocks: Vec<ContentBlock>,
) {
    if let Some(last) = messages.last_mut()
        && last.role == role
    {
        match &mut last.content {
            AnthropicContent::Blocks(existing) => existing.extend(blocks),
            AnthropicContent::Text(text) => {
                let mut new_blocks = vec![ContentBlock::Text {
                    text: std::mem::take(text),
                }];
                new_blocks.extend(blocks);
                last.content = AnthropicContent::Blocks(new_blocks);
            }
        }
    } else {
        messages.push(AnthropicMessage {
            role: role.to_string(),
            content: AnthropicContent::Blocks(blocks),
        });
    }
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

fn parse_response(resp: &AnthropicResponse) -> ModelResponse {
    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in &resp.content {
        match block {
            ResponseContentBlock::Text { text } => {
                content_parts.push(text.clone());
            }
            ResponseContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: input.clone(),
                });
            }
        }
    }

    let usage = resp.usage.as_ref().map(|u| Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
    });

    ModelResponse {
        content: content_parts.join(""),
        tool_calls,
        usage,
    }
}

// ---------------------------------------------------------------------------
// API invocation
// ---------------------------------------------------------------------------

fn run_model(request: &ModelRequest) -> ModelResponse {
    let config = load_config();

    if config.api_key.is_empty() {
        return ModelResponse {
            content: "Error: PHYL_ANTHROPIC_API_KEY is not set".to_string(),
            tool_calls: vec![],
            usage: None,
        };
    }

    let url = format!("{}/v1/messages", config.base_url);
    let api_request = build_request(request, &config);

    eprintln!(
        "phyl-model-anthropic: POST {} (model={}, messages={}, tools={})",
        url,
        config.model,
        api_request.messages.len(),
        api_request.tools.len()
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_read(std::time::Duration::from_secs(config.timeout_secs))
        .timeout_write(std::time::Duration::from_secs(30))
        .build();

    let body = match serde_json::to_string(&api_request) {
        Ok(b) => b,
        Err(e) => {
            return ModelResponse {
                content: format!("Error: failed to serialize request: {e}"),
                tool_calls: vec![],
                usage: None,
            };
        }
    };

    let resp = match agent
        .post(&url)
        .set("content-type", "application/json")
        .set("x-api-key", &config.api_key)
        .set("anthropic-version", "2023-06-01")
        .send_string(&body)
    {
        Ok(r) => r,
        Err(ureq::Error::Status(_code, resp)) => {
            let err_body = resp.into_string().unwrap_or_default();
            let detail = serde_json::from_str::<AnthropicError>(&err_body)
                .ok()
                .and_then(|e| e.error)
                .map(|d| d.message)
                .unwrap_or_else(|| truncate(&err_body, 500).to_string());
            return ModelResponse {
                content: format!("Error: Anthropic API error: {detail}"),
                tool_calls: vec![],
                usage: None,
            };
        }
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

    let api_resp: AnthropicResponse = match serde_json::from_str(&resp_body) {
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

    parse_response(&api_resp)
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

    fn make_tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: format!("The {name} tool"),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            sandbox: None,
        }
    }

    // -- build_request: system extraction --

    #[test]
    fn system_messages_extracted_to_top_level() {
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
                    content: "Hi".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        assert_eq!(api_req.system.as_deref(), Some("You are helpful."));
        assert_eq!(api_req.messages.len(), 1);
        assert_eq!(api_req.messages[0].role, "user");
    }

    #[test]
    fn multiple_system_messages_concatenated() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "First.".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::System,
                    content: "Second.".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::User,
                    content: "Hi".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        assert_eq!(api_req.system.as_deref(), Some("First.\n\nSecond."));
    }

    #[test]
    fn no_system_messages_yields_none() {
        let request = ModelRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hi".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        assert!(api_req.system.is_none());
    }

    // -- build_request: role alternation and merging --

    #[test]
    fn tool_results_merged_into_user_message() {
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
                    tool_calls: vec![
                        ToolCall {
                            id: "toolu_1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "ls"}),
                        },
                        ToolCall {
                            id: "toolu_2".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "pwd"}),
                        },
                    ],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Tool,
                    content: "file1.txt".to_string(),
                    tool_calls: vec![],
                    tool_call_id: Some("toolu_1".to_string()),
                },
                Message {
                    role: Role::Tool,
                    content: "/home".to_string(),
                    tool_calls: vec![],
                    tool_call_id: Some("toolu_2".to_string()),
                },
            ],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        // user, assistant, user (merged tool results)
        assert_eq!(api_req.messages.len(), 3);
        assert_eq!(api_req.messages[2].role, "user");
        if let AnthropicContent::Blocks(blocks) = &api_req.messages[2].content {
            assert_eq!(blocks.len(), 2);
            assert!(
                matches!(&blocks[0], ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "toolu_1")
            );
            assert!(
                matches!(&blocks[1], ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "toolu_2")
            );
        } else {
            panic!("expected Blocks content");
        }
    }

    #[test]
    fn tool_result_followed_by_user_merged() {
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
                        id: "toolu_1".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({}),
                    }],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Tool,
                    content: "done".to_string(),
                    tool_calls: vec![],
                    tool_call_id: Some("toolu_1".to_string()),
                },
                Message {
                    role: Role::User,
                    content: "Now do more".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        // user, assistant, user (tool_result + user text merged)
        assert_eq!(api_req.messages.len(), 3);
        assert_eq!(api_req.messages[2].role, "user");
        if let AnthropicContent::Blocks(blocks) = &api_req.messages[2].content {
            assert_eq!(blocks.len(), 2);
            assert!(matches!(&blocks[0], ContentBlock::ToolResult { .. }));
            assert!(matches!(&blocks[1], ContentBlock::Text { .. }));
        } else {
            panic!("expected Blocks content");
        }
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_blocks() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::User,
                    content: "Hi".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Assistant,
                    content: "Let me check.".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "toolu_abc".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": "ls"}),
                    }],
                    tool_call_id: None,
                },
            ],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        assert_eq!(api_req.messages[1].role, "assistant");
        if let AnthropicContent::Blocks(blocks) = &api_req.messages[1].content {
            assert_eq!(blocks.len(), 2);
            assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Let me check."));
            assert!(
                matches!(&blocks[1], ContentBlock::ToolUse { id, name, .. } if id == "toolu_abc" && name == "bash")
            );
        } else {
            panic!("expected Blocks content");
        }
    }

    #[test]
    fn empty_assistant_content_gets_empty_text_block() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::User,
                    content: "Hi".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::Assistant,
                    content: "".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        if let AnthropicContent::Blocks(blocks) = &api_req.messages[1].content {
            assert_eq!(blocks.len(), 1);
            assert!(matches!(&blocks[0], ContentBlock::Text { text } if text.is_empty()));
        } else {
            panic!("expected Blocks content");
        }
    }

    // -- build_request: tools --

    #[test]
    fn tools_use_input_schema_field() {
        let request = ModelRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hi".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![make_tool("bash")],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        assert_eq!(api_req.tools.len(), 1);
        assert_eq!(api_req.tools[0].name, "bash");

        // Verify serialization uses `input_schema` not `parameters`.
        let json = serde_json::to_value(&api_req.tools[0]).unwrap();
        assert!(json.get("input_schema").is_some());
        assert!(json.get("parameters").is_none());
    }

    #[test]
    fn empty_tools_not_serialized() {
        let request = ModelRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hi".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "test".to_string(),
            api_key: String::new(),
            max_tokens: 1024,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        let json = serde_json::to_value(&api_req).unwrap();
        // tools field should be absent when empty (skip_serializing_if)
        assert!(json.get("tools").is_none());
    }

    // -- parse_response --

    #[test]
    fn parse_text_only_response() {
        let resp = AnthropicResponse {
            content: vec![ResponseContentBlock::Text {
                text: "Hello!".to_string(),
            }],
            usage: Some(AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
            }),
        };
        let model_resp = parse_response(&resp);
        assert_eq!(model_resp.content, "Hello!");
        assert!(model_resp.tool_calls.is_empty());
        assert_eq!(model_resp.usage.as_ref().unwrap().input_tokens, 10);
        assert_eq!(model_resp.usage.as_ref().unwrap().output_tokens, 5);
    }

    #[test]
    fn parse_tool_use_response() {
        let resp = AnthropicResponse {
            content: vec![
                ResponseContentBlock::Text {
                    text: "Let me check.".to_string(),
                },
                ResponseContentBlock::ToolUse {
                    id: "toolu_abc".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                },
            ],
            usage: None,
        };
        let model_resp = parse_response(&resp);
        assert_eq!(model_resp.content, "Let me check.");
        assert_eq!(model_resp.tool_calls.len(), 1);
        assert_eq!(model_resp.tool_calls[0].id, "toolu_abc");
        assert_eq!(model_resp.tool_calls[0].name, "bash");
        assert_eq!(model_resp.tool_calls[0].arguments["command"], "ls");
        assert!(model_resp.usage.is_none());
    }

    #[test]
    fn parse_multiple_tool_uses() {
        let resp = AnthropicResponse {
            content: vec![
                ResponseContentBlock::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                },
                ResponseContentBlock::ToolUse {
                    id: "toolu_2".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "foo.txt"}),
                },
            ],
            usage: Some(AnthropicUsage {
                input_tokens: 100,
                output_tokens: 50,
            }),
        };
        let model_resp = parse_response(&resp);
        assert!(model_resp.content.is_empty());
        assert_eq!(model_resp.tool_calls.len(), 2);
        assert_eq!(model_resp.tool_calls[0].name, "bash");
        assert_eq!(model_resp.tool_calls[1].name, "read_file");
    }

    #[test]
    fn parse_empty_content() {
        let resp = AnthropicResponse {
            content: vec![],
            usage: None,
        };
        let model_resp = parse_response(&resp);
        assert!(model_resp.content.is_empty());
        assert!(model_resp.tool_calls.is_empty());
        assert!(model_resp.usage.is_none());
    }

    #[test]
    fn parse_tool_use_only_no_text() {
        let resp = AnthropicResponse {
            content: vec![ResponseContentBlock::ToolUse {
                id: "toolu_x".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
            }],
            usage: None,
        };
        let model_resp = parse_response(&resp);
        assert!(model_resp.content.is_empty());
        assert_eq!(model_resp.tool_calls.len(), 1);
    }

    // -- serialization roundtrip --

    #[test]
    fn request_serialization_matches_api_format() {
        let request = ModelRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "Be helpful.".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                Message {
                    role: Role::User,
                    content: "What is 2+2?".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![make_tool("calculator")],
        };
        let config = AdapterConfig {
            base_url: String::new(),
            model: "claude-sonnet-4-6".to_string(),
            api_key: String::new(),
            max_tokens: 4096,
            timeout_secs: 30,
        };
        let api_req = build_request(&request, &config);
        let json = serde_json::to_value(&api_req).unwrap();

        assert_eq!(json["model"], "claude-sonnet-4-6");
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["system"], "Be helpful.");
        assert!(json["messages"].is_array());
        assert_eq!(json["messages"].as_array().unwrap().len(), 1);
        assert_eq!(json["messages"][0]["role"], "user");
    }

    #[test]
    fn response_deserialization_from_api_json() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "The answer is 4."},
                {"type": "tool_use", "id": "toolu_123", "name": "calculator", "input": {"expr": "2+2"}}
            ],
            "usage": {"input_tokens": 50, "output_tokens": 25}
        });
        let resp: AnthropicResponse = serde_json::from_value(json).unwrap();
        let model_resp = parse_response(&resp);
        assert_eq!(model_resp.content, "The answer is 4.");
        assert_eq!(model_resp.tool_calls.len(), 1);
        assert_eq!(model_resp.tool_calls[0].id, "toolu_123");
        assert_eq!(model_resp.usage.as_ref().unwrap().input_tokens, 50);
    }
}

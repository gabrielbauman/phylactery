use phyl_core::{Config, McpServerConfig, ServerRequest, ServerResponse, ToolMode, ToolSpec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

// ---------------------------------------------------------------------------
// MCP JSON-RPC 2.0 protocol types
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request sent to an MCP server.
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 response received from an MCP server.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<u64>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

/// Tool definition as returned by MCP `tools/list`.
#[derive(Debug, Deserialize)]
struct McpToolDef {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    input_schema: Option<serde_json::Value>,
}

/// Result content item from MCP `tools/call`.
#[derive(Debug, Deserialize)]
struct McpContentItem {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default)]
    text: Option<String>,
}

// ---------------------------------------------------------------------------
// MCP server handle — wraps a running MCP server child process
// ---------------------------------------------------------------------------

struct McpServer {
    name: String,
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
    /// Tool names this server provides.
    tool_names: Vec<String>,
}

impl McpServer {
    /// Spawn an MCP server from config and perform the initialize handshake.
    fn start(config: &McpServerConfig) -> Result<Self, String> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        for (k, v) in &config.env {
            // Expand $VAR references from the process environment.
            let expanded = if let Some(var_name) = v.strip_prefix('$') {
                std::env::var(var_name).unwrap_or_default()
            } else {
                v.clone()
            };
            cmd.env(k, expanded);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn MCP server '{}': {e}", config.name))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("no stdin for MCP server '{}'", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("no stdout for MCP server '{}'", config.name))?;

        let mut server = McpServer {
            name: config.name.clone(),
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
            tool_names: Vec::new(),
        };

        server.initialize()?;
        Ok(server)
    }

    /// Send the MCP `initialize` request and `initialized` notification.
    fn initialize(&mut self) -> Result<(), String> {
        let resp = self.call(
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "phylactery",
                    "version": "0.1.0"
                }
            })),
        )?;

        // Verify the server responded with capabilities.
        if let Some(result) = &resp.result {
            eprintln!(
                "phyl-tool-mcp: MCP server '{}' initialized (protocol: {})",
                self.name,
                result
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            );
        }

        // Send `initialized` notification (no id, no response expected).
        self.notify("notifications/initialized", None)?;

        Ok(())
    }

    /// List tools provided by this MCP server.
    fn list_tools(&mut self) -> Result<Vec<McpToolDef>, String> {
        let resp = self.call("tools/list", Some(serde_json::json!({})))?;

        if let Some(err) = resp.error {
            return Err(format!(
                "MCP server '{}' tools/list error: {}",
                self.name, err.message
            ));
        }

        let result = resp.result.ok_or_else(|| {
            format!(
                "MCP server '{}': no result in tools/list response",
                self.name
            )
        })?;

        let tools: Vec<McpToolDef> = serde_json::from_value(
            result
                .get("tools")
                .cloned()
                .unwrap_or(serde_json::Value::Array(vec![])),
        )
        .map_err(|e| format!("MCP server '{}': failed to parse tools: {e}", self.name))?;

        Ok(tools)
    }

    /// Call a tool on this MCP server.
    fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, String> {
        let resp = self.call(
            "tools/call",
            Some(serde_json::json!({
                "name": tool_name,
                "arguments": arguments
            })),
        )?;

        if let Some(err) = resp.error {
            return Err(err.message);
        }

        let result = resp.result.ok_or("no result in tools/call response")?;

        // Extract text content from the MCP response content array.
        let content_items: Vec<McpContentItem> = serde_json::from_value(
            result
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Array(vec![])),
        )
        .unwrap_or_default();

        let text_parts: Vec<&str> = content_items
            .iter()
            .filter(|c| c.content_type == "text")
            .filter_map(|c| c.text.as_deref())
            .collect();

        if text_parts.is_empty() {
            // Check if there's an isError flag.
            if result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Err("MCP tool returned an error with no text content".to_string());
            }
            Ok(String::new())
        } else {
            Ok(text_parts.join("\n"))
        }
    }

    /// Send a JSON-RPC request and read the response.
    fn call(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<JsonRpcResponse, String> {
        let id = self.next_id;
        self.next_id += 1;

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let mut json = serde_json::to_string(&request)
            .map_err(|e| format!("failed to serialize JSON-RPC request: {e}"))?;
        json.push('\n');

        self.stdin
            .write_all(json.as_bytes())
            .map_err(|e| format!("failed to write to MCP server '{}': {e}", self.name))?;
        self.stdin
            .flush()
            .map_err(|e| format!("failed to flush MCP server '{}': {e}", self.name))?;

        // Read lines until we get a response with our id.
        // MCP servers may send notifications (no id) that we skip.
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| format!("failed to read from MCP server '{}': {e}", self.name))?;

            if n == 0 {
                return Err(format!(
                    "MCP server '{}' closed stdout unexpectedly",
                    self.name
                ));
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Try to parse as a JSON-RPC response.
            match serde_json::from_str::<JsonRpcResponse>(trimmed) {
                Ok(resp) => {
                    if resp.id == Some(id) {
                        return Ok(resp);
                    }
                    // Response for a different id or a notification — skip.
                    eprintln!(
                        "phyl-tool-mcp: skipping message with id {:?} (expected {id})",
                        resp.id
                    );
                }
                Err(_) => {
                    // Not a valid JSON-RPC response — possibly a notification, skip.
                    eprintln!(
                        "phyl-tool-mcp: skipping non-JSON-RPC line from '{}': {}",
                        self.name,
                        &trimmed[..trimmed.len().min(80)]
                    );
                }
            }
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    fn notify(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<(), String> {
        #[derive(Serialize)]
        struct JsonRpcNotification {
            jsonrpc: &'static str,
            method: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            params: Option<serde_json::Value>,
        }

        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };

        let mut json = serde_json::to_string(&notification)
            .map_err(|e| format!("failed to serialize notification: {e}"))?;
        json.push('\n');

        self.stdin
            .write_all(json.as_bytes())
            .map_err(|e| format!("failed to write notification to '{}': {e}", self.name))?;
        self.stdin
            .flush()
            .map_err(|e| format!("failed to flush '{}': {e}", self.name))?;

        Ok(())
    }

    /// Shut down the MCP server cleanly.
    fn shutdown(mut self) {
        // Drop stdin to signal EOF.
        drop(self.stdin);
        // Give it a moment to exit, then kill.
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

fn load_mcp_configs() -> Result<Vec<McpServerConfig>, String> {
    let home = phyl_core::home_dir();
    let config_path = home.join("config.toml");

    if !config_path.exists() {
        return Ok(Vec::new());
    }

    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read config.toml: {e}"))?;
    let config: Config =
        toml::from_str(&contents).map_err(|e| format!("failed to parse config.toml: {e}"))?;

    Ok(config.mcp)
}

// ---------------------------------------------------------------------------
// --spec mode: discover tools from all configured MCP servers
// ---------------------------------------------------------------------------

fn run_spec() -> Result<(), String> {
    let configs = load_mcp_configs()?;

    if configs.is_empty() {
        // No MCP servers configured — return empty array.
        println!("[]");
        return Ok(());
    }

    let mut all_specs: Vec<ToolSpec> = Vec::new();

    for config in &configs {
        match McpServer::start(config) {
            Ok(mut server) => {
                match server.list_tools() {
                    Ok(tools) => {
                        for tool in tools {
                            // Prefix tool name with server name to avoid collisions.
                            let prefixed_name = format!("{}_{}", config.name, tool.name);

                            let description = tool.description.unwrap_or_else(|| {
                                format!("{} (via MCP server '{}')", tool.name, config.name)
                            });

                            let parameters = tool.input_schema.unwrap_or_else(|| {
                                serde_json::json!({
                                    "type": "object",
                                    "properties": {}
                                })
                            });

                            all_specs.push(ToolSpec {
                                name: prefixed_name,
                                description,
                                mode: ToolMode::Server,
                                parameters,
                                sandbox: None,
                            });

                            server.tool_names.push(tool.name);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "phyl-tool-mcp: failed to list tools from '{}': {e}",
                            config.name
                        );
                    }
                }
                server.shutdown();
            }
            Err(e) => {
                eprintln!(
                    "phyl-tool-mcp: failed to start MCP server '{}': {e}",
                    config.name
                );
            }
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&all_specs)
            .map_err(|e| format!("failed to serialize specs: {e}"))?
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// --serve mode: long-running NDJSON bridge
// ---------------------------------------------------------------------------

fn run_serve() -> Result<(), String> {
    let configs = load_mcp_configs()?;

    // Start all configured MCP servers.
    let mut servers: Vec<McpServer> = Vec::new();
    // Map from prefixed tool name → (server index, original tool name).
    let mut tool_routing: HashMap<String, (usize, String)> = HashMap::new();

    for config in &configs {
        match McpServer::start(config) {
            Ok(mut server) => {
                match server.list_tools() {
                    Ok(tools) => {
                        let server_idx = servers.len();
                        for tool in tools {
                            let prefixed_name = format!("{}_{}", config.name, tool.name);
                            tool_routing.insert(prefixed_name, (server_idx, tool.name.clone()));
                            server.tool_names.push(tool.name);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "phyl-tool-mcp: failed to list tools from '{}': {e}",
                            config.name
                        );
                    }
                }
                servers.push(server);
            }
            Err(e) => {
                eprintln!(
                    "phyl-tool-mcp: failed to start MCP server '{}': {e}",
                    config.name
                );
            }
        }
    }

    eprintln!(
        "phyl-tool-mcp: serving {} tools from {} MCP servers",
        tool_routing.len(),
        servers.len()
    );

    // NDJSON server loop — read ServerRequest lines, dispatch, respond.
    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = stdin.lock();
    let mut writer = stdout.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("phyl-tool-mcp: stdin read error: {e}");
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
                eprintln!("phyl-tool-mcp: ignoring unrecognized input: {trimmed}");
                continue;
            }
        };

        let response = match tool_routing.get(&req.name) {
            Some((server_idx, original_name)) => match servers.get_mut(*server_idx) {
                Some(server) => match server.call_tool(original_name, &req.arguments) {
                    Ok(output) => ServerResponse {
                        id: req.id,
                        output: Some(output),
                        error: None,
                        signal: None,
                    },
                    Err(e) => ServerResponse {
                        id: req.id,
                        output: None,
                        error: Some(e),
                        signal: None,
                    },
                },
                None => ServerResponse {
                    id: req.id,
                    output: None,
                    error: Some(format!("MCP server index {} out of range", server_idx)),
                    signal: None,
                },
            },
            None => ServerResponse {
                id: req.id,
                output: None,
                error: Some(format!("unknown tool: {}", req.name)),
                signal: None,
            },
        };

        write_response(&mut writer, &response);
    }

    // stdin closed — shut down all MCP servers.
    eprintln!("phyl-tool-mcp: stdin closed, shutting down MCP servers");
    for server in servers {
        server.shutdown();
    }

    Ok(())
}

fn write_response(writer: &mut impl Write, response: &ServerResponse) {
    let mut json = serde_json::to_string(response).expect("failed to serialize response");
    json.push('\n');
    let _ = writer.write_all(json.as_bytes());
    let _ = writer.flush();
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--spec") {
        if let Err(e) = run_spec() {
            eprintln!("phyl-tool-mcp: --spec error: {e}");
            std::process::exit(1);
        }
        return;
    }

    if args.iter().any(|a| a == "--serve") {
        if let Err(e) = run_serve() {
            eprintln!("phyl-tool-mcp: --serve error: {e}");
            std::process::exit(1);
        }
        return;
    }

    eprintln!("phyl-tool-mcp: use --spec or --serve");
    std::process::exit(1);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "tools/list".to_string(),
            params: Some(serde_json::json!({})),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"tools/list\""));
    }

    #[test]
    fn test_json_rpc_request_without_params() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 42,
            method: "test".to_string(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("params"));
    }

    #[test]
    fn test_json_rpc_response_success() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_json_rpc_response_error() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().message, "Method not found");
    }

    #[test]
    fn test_json_rpc_response_notification() {
        // Notifications have no id.
        let json = r#"{"jsonrpc":"2.0","method":"notification","params":{}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, None);
    }

    #[test]
    fn test_mcp_tool_def_parsing() {
        let json = r#"{
            "name": "read_file",
            "description": "Read a file from disk",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }
        }"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.description.unwrap(), "Read a file from disk");
        assert!(tool.input_schema.is_some());
    }

    #[test]
    fn test_mcp_tool_def_minimal() {
        let json = r#"{"name": "ping"}"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "ping");
        assert!(tool.description.is_none());
        assert!(tool.input_schema.is_none());
    }

    #[test]
    fn test_mcp_content_item() {
        let json = r#"{"type": "text", "text": "Hello, world!"}"#;
        let item: McpContentItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.content_type, "text");
        assert_eq!(item.text.unwrap(), "Hello, world!");
    }

    #[test]
    fn test_tool_name_prefixing() {
        let server_name = "filesystem";
        let tool_name = "read_file";
        let prefixed = format!("{}_{}", server_name, tool_name);
        assert_eq!(prefixed, "filesystem_read_file");
    }

    #[test]
    fn test_tool_routing_lookup() {
        let mut routing: HashMap<String, (usize, String)> = HashMap::new();
        routing.insert(
            "filesystem_read_file".to_string(),
            (0, "read_file".to_string()),
        );
        routing.insert("brave-search_search".to_string(), (1, "search".to_string()));

        let (idx, original) = routing.get("filesystem_read_file").unwrap();
        assert_eq!(*idx, 0);
        assert_eq!(original, "read_file");

        let (idx, original) = routing.get("brave-search_search").unwrap();
        assert_eq!(*idx, 1);
        assert_eq!(original, "search");

        assert!(!routing.contains_key("unknown_tool"));
    }

    #[test]
    fn test_server_response_serialization() {
        let resp = ServerResponse {
            id: "tc_1".to_string(),
            output: Some("result text".to_string()),
            error: None,
            signal: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"id\":\"tc_1\""));
        assert!(json.contains("\"output\":\"result text\""));
        assert!(!json.contains("error"));
        assert!(!json.contains("signal"));
    }

    #[test]
    fn test_server_response_with_error() {
        let resp = ServerResponse {
            id: "tc_2".to_string(),
            output: None,
            error: Some("something went wrong".to_string()),
            signal: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\":\"something went wrong\""));
        assert!(!json.contains("output"));
    }

    #[test]
    fn test_spec_output_format() {
        // Verify that ToolSpec with server mode serializes correctly.
        let spec = ToolSpec {
            name: "fs_read_file".to_string(),
            description: "Read a file".to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
            sandbox: None,
        };
        let json = serde_json::to_string_pretty(&[&spec]).unwrap();
        assert!(json.contains("\"mode\": \"server\""));
        assert!(json.contains("fs_read_file"));
    }

    #[test]
    fn test_env_var_expansion() {
        // Test the $VAR expansion logic.
        let v = "$HOME";
        let expanded = if let Some(var_name) = v.strip_prefix('$') {
            std::env::var(var_name).unwrap_or_default()
        } else {
            v.to_string()
        };
        // $HOME should be set in any test environment.
        assert!(!expanded.is_empty() || std::env::var("HOME").is_err());
    }

    #[test]
    fn test_load_mcp_configs_missing_home() {
        // If PHYLACTERY_HOME points somewhere without config.toml,
        // we should get an empty list.
        let old = std::env::var("PHYLACTERY_HOME").ok();
        unsafe {
            std::env::set_var("PHYLACTERY_HOME", "/tmp/nonexistent-phylactery-test");
        }
        let result = load_mcp_configs();
        // Restore.
        unsafe {
            match old {
                Some(v) => std::env::set_var("PHYLACTERY_HOME", v),
                None => std::env::remove_var("PHYLACTERY_HOME"),
            }
        }
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_mcp_content_extraction() {
        // Simulate extracting text from MCP tools/call response.
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "Line 1"},
                {"type": "image", "data": "..."},
                {"type": "text", "text": "Line 2"}
            ]
        });
        let content_items: Vec<McpContentItem> =
            serde_json::from_value(result.get("content").cloned().unwrap()).unwrap();
        let text_parts: Vec<&str> = content_items
            .iter()
            .filter(|c| c.content_type == "text")
            .filter_map(|c| c.text.as_deref())
            .collect();
        assert_eq!(text_parts, vec!["Line 1", "Line 2"]);
        assert_eq!(text_parts.join("\n"), "Line 1\nLine 2");
    }

    #[test]
    fn test_initialize_params() {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "phylactery",
                "version": "0.1.0"
            }
        });
        assert_eq!(params["protocolVersion"].as_str().unwrap(), "2024-11-05");
        assert_eq!(params["clientInfo"]["name"].as_str().unwrap(), "phylactery");
    }
}

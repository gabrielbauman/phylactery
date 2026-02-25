use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// --- Message types for the model adapter protocol ---

/// Role of a message participant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in a conversation.
///
/// For assistant messages, `tool_calls` may contain requested tool invocations.
/// For tool messages, `tool_call_id` ties the result back to the originating call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

// --- Tool call types ---

/// A tool invocation requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

// --- Tool specification types ---

/// Mode a tool operates in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolMode {
    #[default]
    Oneshot,
    Server,
}

/// Sandbox configuration declared by a tool in its spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSpec {
    #[serde(default)]
    pub paths_rw: Vec<String>,
    #[serde(default)]
    pub paths_ro: Vec<String>,
    #[serde(default = "default_true")]
    pub net: bool,
    #[serde(default)]
    pub max_cpu_seconds: Option<u64>,
    #[serde(default)]
    pub max_file_bytes: Option<u64>,
    #[serde(default)]
    pub max_procs: Option<u64>,
    #[serde(default)]
    pub max_fds: Option<u64>,
}

fn default_true() -> bool {
    true
}

/// A tool's self-description, returned by `phyl-tool-X --spec`.
///
/// A single executable may return one `ToolSpec` or an array of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub mode: ToolMode,
    pub parameters: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSpec>,
}

// --- Model adapter protocol ---

/// Request sent to a model adapter on stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

/// Token usage information from a model invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Response received from a model adapter on stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

// --- Tool I/O types (one-shot mode) ---

/// Input sent to a one-shot tool on stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInput {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Output received from a one-shot tool on stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// --- Server-mode tool protocol (NDJSON) ---

/// Request sent to a server-mode tool (one JSON line on stdin).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerRequest {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Response from a server-mode tool (one JSON line on stdout).
///
/// The optional `signal` field communicates out-of-band information
/// to the session runner (e.g., `"end_session"` from the `done` tool).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerResponse {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

// --- Event log types ---

/// Type of an event in the session log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogEntryType {
    System,
    User,
    Assistant,
    ToolResult,
    Question,
    Answer,
    Done,
    Error,
}

/// A single entry in a session's `log.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: DateTime<Utc>,
    #[serde(rename = "type")]
    pub entry_type: LogEntryType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

// --- Configuration types ---

/// Top-level agent configuration from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub mcp: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_socket_path")]
    pub socket: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket: default_socket_path(),
        }
    }
}

fn default_socket_path() -> String {
    std::env::var("XDG_RUNTIME_DIR")
        .map(|dir| format!("{}/phylactery.sock", dir))
        .unwrap_or_else(|_| "/tmp/phylactery.sock".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_timeout_minutes")]
    pub timeout_minutes: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default = "default_model")]
    pub model: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            timeout_minutes: default_timeout_minutes(),
            max_concurrent: default_max_concurrent(),
            model: default_model(),
        }
    }
}

fn default_timeout_minutes() -> u64 {
    60
}
fn default_max_concurrent() -> u32 {
    4
}
fn default_model() -> String {
    "phyl-model-claude".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default = "default_compress_at")]
    pub compress_at: f64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            context_window: default_context_window(),
            compress_at: default_compress_at(),
        }
    }
}

fn default_context_window() -> u64 {
    200_000
}
fn default_compress_at() -> f64 {
    0.8
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            auto_commit: true,
            remote: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

// --- Session status (used by the daemon API) ---

/// Status of a session as reported by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Done,
    Crashed,
    TimedOut,
}

/// Summary info for a session (returned by `GET /sessions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

// --- Utility: resolve PHYLACTERY_HOME ---

/// Returns the path to the agent's home directory.
///
/// Uses `$PHYLACTERY_HOME` if set, otherwise defaults to `~/.phylactery`.
pub fn home_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("PHYLACTERY_HOME") {
        std::path::PathBuf::from(dir)
    } else {
        dirs_or_default()
    }
}

fn dirs_or_default() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".phylactery"))
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/.phylactery"))
}

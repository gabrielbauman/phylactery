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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeConfig>,
    #[serde(default)]
    pub poll: Vec<PollConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<ListenConfig>,
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

// --- Bridge configuration ---

/// Bridge configuration section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<SignalBridgeConfig>,
}

/// Signal Messenger bridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalBridgeConfig {
    /// Agent's Signal phone number (registered with signal-cli).
    pub phone: String,
    /// Owner's Signal phone number (only accept messages from this number).
    pub owner: String,
    /// Path to signal-cli binary.
    #[serde(default = "default_signal_cli")]
    pub signal_cli: String,
}

fn default_signal_cli() -> String {
    "signal-cli".to_string()
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

// --- Poll configuration (used by phyl-poll) ---

/// Configuration for a single poll rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub interval: u64,
    pub prompt: String,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub shell: bool,
    #[serde(default = "default_poll_timeout")]
    pub timeout: u64,
}

fn default_poll_interval() -> u64 {
    300
}

fn default_poll_timeout() -> u64 {
    30
}

// --- Listen configuration (used by phyl-listen) ---

/// Top-level listen configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenConfig {
    #[serde(default = "default_listen_bind")]
    pub bind: String,
    #[serde(default)]
    pub hook: Vec<ListenHookConfig>,
    #[serde(default)]
    pub sse: Vec<ListenSseConfig>,
    #[serde(default)]
    pub watch: Vec<ListenWatchConfig>,
}

fn default_listen_bind() -> String {
    "127.0.0.1:7890".to_string()
}

/// Configuration for a webhook listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenHookConfig {
    pub name: String,
    pub path: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_header: Option<String>,
    #[serde(default)]
    pub filter_values: Vec<String>,
    #[serde(default = "default_rate_limit")]
    pub rate_limit: u32,
    #[serde(default = "default_dedup_header")]
    pub dedup_header: String,
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_header: Option<String>,
    #[serde(default)]
    pub routes: std::collections::HashMap<String, String>,
}

fn default_rate_limit() -> u32 {
    10
}

fn default_dedup_header() -> String {
    "X-Request-Id".to_string()
}

fn default_max_body_size() -> usize {
    1_048_576 // 1 MB
}

/// Configuration for an SSE subscription listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenSseConfig {
    pub name: String,
    pub url: String,
    pub prompt: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub route_event: bool,
    #[serde(default)]
    pub routes: std::collections::HashMap<String, String>,
    #[serde(default = "default_rate_limit")]
    pub rate_limit: u32,
}

/// Configuration for a file watch listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenWatchConfig {
    pub name: String,
    pub path: String,
    pub prompt: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(default = "default_debounce")]
    pub debounce: u64,
    #[serde(default = "default_rate_limit")]
    pub rate_limit: u32,
}

fn default_debounce() -> u64 {
    2
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
/// Resolution order:
/// 1. `$PHYLACTERY_HOME` if set
/// 2. `$XDG_DATA_HOME/phylactery` (typically `~/.local/share/phylactery`)
/// 3. `~/.phylactery` (legacy path, still supported)
/// 4. `/tmp/.phylactery` (fallback when `$HOME` is unset)
pub fn home_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("PHYLACTERY_HOME") {
        return std::path::PathBuf::from(dir);
    }

    // Try XDG data directory first.
    let xdg_path = if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        Some(std::path::PathBuf::from(xdg).join("phylactery"))
    } else if let Ok(home) = std::env::var("HOME") {
        Some(std::path::PathBuf::from(home).join(".local/share/phylactery"))
    } else {
        None
    };

    if let Some(ref xdg) = xdg_path
        && xdg.exists()
    {
        return xdg.clone();
    }

    // Legacy path.
    if let Ok(home) = std::env::var("HOME") {
        let legacy = std::path::PathBuf::from(&home).join(".phylactery");
        if legacy.exists() {
            return legacy;
        }
        // Neither exists yet — prefer XDG for new installations.
        return xdg_path.unwrap_or_else(|| std::path::PathBuf::from(home).join(".phylactery"));
    }

    std::path::PathBuf::from("/tmp/.phylactery")
}

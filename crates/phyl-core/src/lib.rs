use chrono::{DateTime, Duration, Utc};
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
    #[serde(default)]
    pub psyche: PsycheConfig,
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
    // XDG_RUNTIME_DIR is typical on Linux; fall back to /tmp on all platforms.
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return format!("{dir}/phylactery.sock");
    }

    // macOS: use $TMPDIR (per-user, e.g. /var/folders/…/T/) when available.
    #[cfg(target_os = "macos")]
    if let Ok(dir) = std::env::var("TMPDIR") {
        let dir = dir.trim_end_matches('/');
        return format!("{dir}/phylactery.sock");
    }

    "/tmp/phylactery.sock".to_string()
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
/// 2. Platform-specific data directory:
///    - Linux: `$XDG_DATA_HOME/phylactery` (typically `~/.local/share/phylactery`)
///    - macOS: `~/Library/Application Support/phylactery`
/// 3. `~/.phylactery` (legacy path, still supported on all platforms)
/// 4. `/tmp/.phylactery` (fallback when `$HOME` is unset)
pub fn home_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("PHYLACTERY_HOME") {
        return std::path::PathBuf::from(dir);
    }

    // Try platform-specific data directory first.
    let data_path = platform_data_dir();

    if let Some(ref dp) = data_path
        && dp.exists()
    {
        return dp.clone();
    }

    // Legacy path.
    if let Ok(home) = std::env::var("HOME") {
        let legacy = std::path::PathBuf::from(&home).join(".phylactery");
        if legacy.exists() {
            return legacy;
        }
        // Neither exists yet — prefer the platform-specific path for new installations.
        return data_path.unwrap_or_else(|| std::path::PathBuf::from(home).join(".phylactery"));
    }

    std::path::PathBuf::from("/tmp/.phylactery")
}

// --- Schedule types (used by phyl-tool-self and phyl-sched) ---

/// A scheduled session entry stored as a JSON file in `$PHYLACTERY_HOME/schedule/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub id: Uuid,
    pub prompt: String,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Parse a time specification into an absolute `DateTime<Utc>`.
///
/// Accepts two formats:
/// - **Datetime**: ISO 8601 timestamp (e.g. `2026-03-04T10:00:00Z`). Rejects past times.
/// - **Interval**: relative offset from now (e.g. `30s`, `5m`, `2h`, `1d`, `1w`). Rejects zero/negative.
pub fn parse_time_spec(spec: &str) -> Result<DateTime<Utc>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty time spec".to_string());
    }

    // Try relative interval first: digits followed by a single unit letter
    if let Some((num_str, unit)) = split_interval(spec) {
        let n: i64 = num_str
            .parse()
            .map_err(|_| format!("invalid number in interval: {num_str}"))?;
        if n <= 0 {
            return Err("interval must be positive".to_string());
        }
        let duration = match unit {
            's' => Duration::seconds(n),
            'm' => Duration::minutes(n),
            'h' => Duration::hours(n),
            'd' => Duration::days(n),
            'w' => Duration::weeks(n),
            _ => return Err(format!("unknown interval unit: {unit}")),
        };
        return Ok(Utc::now() + duration);
    }

    // Try ISO 8601 datetime
    let dt: DateTime<Utc> = spec
        .parse()
        .map_err(|e| format!("invalid time spec '{spec}': {e}"))?;
    if dt <= Utc::now() {
        return Err("scheduled time is in the past".to_string());
    }
    Ok(dt)
}

/// Split an interval string like "30s" or "2h" into (number_str, unit_char).
fn split_interval(s: &str) -> Option<(&str, char)> {
    let unit = s.chars().last()?;
    if !"smhdw".contains(unit) {
        return None;
    }
    let num_str = &s[..s.len() - 1];
    if num_str.is_empty() || !num_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((num_str, unit))
}

// --- Psyche system types ---

/// Type of a concern in the psyche system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConcernType {
    /// Wants knowledge — a question, an uncertainty, a gap in understanding.
    Epistemic,
    /// Wants experience or possession — something desired.
    Appetitive,
    /// Wants action or change — something to do about a specific tension.
    Conative,
}

/// State of a concern in its lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConcernState {
    Open,
    Committed,
    Resolved,
    Abandoned,
}

/// State of a commitment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentState {
    Pending,
    Fulfilled,
    Broken,
}

/// Urgency level for escalations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    High,
}

/// Kind of escalation to the operator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EscalationKind {
    Blocked,
    DecisionRequired,
    Fyi,
    RequestCapability,
}

/// A concern — the core primitive of the psyche system.
///
/// Represents something the agent cares about: a question, a desire, or an
/// intention to act. Concerns accumulate investment through touches and decay
/// through neglect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concern {
    pub concern_id: String,
    pub description: String,
    #[serde(rename = "type")]
    pub concern_type: ConcernType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tension: Option<String>,
    pub state: ConcernState,
    pub salience: f64,
    #[serde(default)]
    pub tags: Vec<String>,
    pub origin: String,
    pub touch_count: u32,
    pub created_session: u64,
    pub touched_session: u64,
    pub created_at: DateTime<Utc>,
    pub touched_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandoned_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandon_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_from: Option<String>,
    #[serde(default)]
    pub spawned: Vec<String>,
}

/// A commitment — a concrete action the agent has declared it will take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    pub commitment_id: String,
    pub concern_id: String,
    pub action: String,
    pub scheduled_for: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    pub state: CommitmentState,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub spawned_concerns: Vec<String>,
}

/// An escalation to the operator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Escalation {
    pub escalation_id: String,
    pub subject: String,
    pub body: String,
    pub urgency: Urgency,
    pub kind: EscalationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concern_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commitment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_resolution: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responded_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
}

/// A structured knowledge base record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbRecord {
    pub record_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concern_id: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_reason: Option<String>,
}

/// The briefing — the continuity artifact produced by the subconscious pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Briefing {
    pub generated_at: DateTime<Utc>,
    pub session_number: u64,
    pub elapsed_wall_time_seconds: u64,
    pub sessions_since_last_active: u64,
    pub top_concerns: Vec<Concern>,
    pub pending_commitments: Vec<Commitment>,
    pub broken_commitments: Vec<Commitment>,
    pub flagged_for_abandonment: Vec<Concern>,
    #[serde(default)]
    pub suggested_tensions: Vec<String>,
    #[serde(default)]
    pub open_escalations: Vec<Escalation>,
}

/// Configuration for the psyche system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PsycheConfig {
    #[serde(default = "default_half_life_sessions")]
    pub half_life_sessions: u32,
    #[serde(default = "default_abandonment_threshold")]
    pub abandonment_threshold: f64,
    #[serde(default = "default_briefing_top_n")]
    pub briefing_top_n: usize,
}

impl Default for PsycheConfig {
    fn default() -> Self {
        Self {
            half_life_sessions: default_half_life_sessions(),
            abandonment_threshold: default_abandonment_threshold(),
            briefing_top_n: default_briefing_top_n(),
        }
    }
}

fn default_half_life_sessions() -> u32 {
    10
}

fn default_abandonment_threshold() -> f64 {
    0.05
}

fn default_briefing_top_n() -> usize {
    5
}

/// Returns the platform-specific data directory for new installations.
fn platform_data_dir() -> Option<std::path::PathBuf> {
    // Honour explicit XDG_DATA_HOME on any platform.
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return Some(std::path::PathBuf::from(xdg).join("phylactery"));
    }

    let home = std::env::var("HOME").ok()?;

    #[cfg(target_os = "macos")]
    {
        Some(std::path::PathBuf::from(&home).join("Library/Application Support/phylactery"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Some(std::path::PathBuf::from(&home).join(".local/share/phylactery"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_time_spec_interval_seconds() {
        let dt = parse_time_spec("30s").unwrap();
        let diff = dt - Utc::now();
        assert!(diff.num_seconds() >= 29 && diff.num_seconds() <= 31);
    }

    #[test]
    fn test_parse_time_spec_interval_minutes() {
        let dt = parse_time_spec("5m").unwrap();
        let diff = dt - Utc::now();
        assert!(diff.num_minutes() >= 4 && diff.num_minutes() <= 5);
    }

    #[test]
    fn test_parse_time_spec_interval_hours() {
        let dt = parse_time_spec("2h").unwrap();
        let diff = dt - Utc::now();
        assert!(diff.num_hours() >= 1 && diff.num_hours() <= 2);
    }

    #[test]
    fn test_parse_time_spec_interval_days() {
        let dt = parse_time_spec("1d").unwrap();
        let diff = dt - Utc::now();
        assert!(diff.num_hours() >= 23 && diff.num_hours() <= 24);
    }

    #[test]
    fn test_parse_time_spec_interval_weeks() {
        let dt = parse_time_spec("1w").unwrap();
        let diff = dt - Utc::now();
        assert!(diff.num_days() >= 6 && diff.num_days() <= 7);
    }

    #[test]
    fn test_parse_time_spec_zero_rejected() {
        assert!(parse_time_spec("0s").is_err());
    }

    #[test]
    fn test_parse_time_spec_empty_rejected() {
        assert!(parse_time_spec("").is_err());
    }

    #[test]
    fn test_parse_time_spec_invalid_unit() {
        assert!(parse_time_spec("10x").is_err());
    }

    #[test]
    fn test_parse_time_spec_iso8601_future() {
        // Use a date far in the future
        let dt = parse_time_spec("2099-01-01T00:00:00Z").unwrap();
        assert!(dt > Utc::now());
    }

    #[test]
    fn test_parse_time_spec_iso8601_past_rejected() {
        assert!(parse_time_spec("2020-01-01T00:00:00Z").is_err());
    }

    #[test]
    fn test_parse_time_spec_whitespace_trimmed() {
        let dt = parse_time_spec("  30s  ").unwrap();
        let diff = dt - Utc::now();
        assert!(diff.num_seconds() >= 29 && diff.num_seconds() <= 31);
    }

    #[test]
    fn test_schedule_entry_roundtrip() {
        let entry = ScheduleEntry {
            id: Uuid::new_v4(),
            prompt: "test prompt".to_string(),
            at: Utc::now(),
            created_by: Some("session-123".to_string()),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ScheduleEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, entry.id);
        assert_eq!(parsed.prompt, entry.prompt);
        assert_eq!(parsed.created_by, entry.created_by);
    }
}

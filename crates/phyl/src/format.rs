//! Display formatting for log entries and session output.

use phyl_core::{LogEntry, LogEntryType};

/// Print a single log entry to stdout with type-appropriate formatting.
pub fn format_log_entry(entry: &LogEntry) {
    for line in format_log_entry_lines(entry) {
        println!("{line}");
    }
}

/// Format a log entry into display lines (testable).
pub fn format_log_entry_lines(entry: &LogEntry) -> Vec<String> {
    let ts = entry.ts.format("%H:%M:%S");
    let mut lines = Vec::new();

    match entry.entry_type {
        LogEntryType::User => {
            if let Some(ref content) = entry.content {
                lines.push(format!("[{ts}] user: {content}"));
            }
        }
        LogEntryType::Assistant => {
            if let Some(ref content) = entry.content {
                lines.push(format!("[{ts}] assistant: {content}"));
            }
            for tc in &entry.tool_calls {
                lines.push(format!("[{ts}]   -> tool_call: {}({})", tc.name, tc.id));
            }
        }
        LogEntryType::ToolResult => {
            let id = entry.tool_call_id.as_deref().unwrap_or("?");
            if let Some(ref content) = entry.content {
                let display: String = if content.len() > 200 {
                    format!("{}...", &content[..200])
                } else {
                    content.clone()
                };
                lines.push(format!("[{ts}]   <- tool[{id}]: {display}"));
            }
        }
        LogEntryType::Question => {
            let qid = entry.id.as_deref().unwrap_or("?");
            let text = entry.content.as_deref().unwrap_or("");
            lines.push(format!("[{ts}] ? QUESTION [{qid}]: {text}"));
            for (i, opt) in entry.options.iter().enumerate() {
                lines.push(format!("         {}: {opt}", i + 1));
            }
        }
        LogEntryType::Answer => {
            let qid = entry.question_id.as_deref().unwrap_or("?");
            let text = entry.content.as_deref().unwrap_or("");
            lines.push(format!("[{ts}]   answer[{qid}]: {text}"));
        }
        LogEntryType::Done => {
            let summary = entry
                .summary
                .as_deref()
                .or(entry.content.as_deref())
                .unwrap_or("(no summary)");
            lines.push(format!("[{ts}] DONE: {summary}"));
        }
        LogEntryType::Error => {
            let msg = entry.content.as_deref().unwrap_or("unknown error");
            lines.push(format!("[{ts}] ERROR: {msg}"));
        }
        LogEntryType::System => {
            if let Some(ref content) = entry.content {
                lines.push(format!("[{ts}] system: {content}"));
            }
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use phyl_core::ToolCall;

    fn make_entry(entry_type: LogEntryType) -> LogEntry {
        LogEntry {
            ts: Utc::now(),
            entry_type,
            content: None,
            summary: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            id: None,
            question_id: None,
            options: Vec::new(),
        }
    }

    #[test]
    fn test_format_user_message() {
        let mut entry = make_entry(LogEntryType::User);
        entry.content = Some("hello world".to_string());
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("user: hello world"));
    }

    #[test]
    fn test_format_assistant_with_tool_calls() {
        let mut entry = make_entry(LogEntryType::Assistant);
        entry.content = Some("thinking...".to_string());
        entry.tool_calls = vec![ToolCall {
            id: "tc_1".to_string(),
            name: "bash".to_string(),
            arguments: serde_json::json!({"command": "ls"}),
        }];
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("assistant: thinking..."));
        assert!(lines[1].contains("tool_call: bash(tc_1)"));
    }

    #[test]
    fn test_format_tool_result_truncation() {
        let mut entry = make_entry(LogEntryType::ToolResult);
        entry.tool_call_id = Some("tc_1".to_string());
        entry.content = Some("x".repeat(300));
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("..."));
        // Should be truncated to 200 chars + "..."
        assert!(lines[0].len() < 300);
    }

    #[test]
    fn test_format_question_with_options() {
        let mut entry = make_entry(LogEntryType::Question);
        entry.id = Some("q_42".to_string());
        entry.content = Some("Pick one:".to_string());
        entry.options = vec!["yes".to_string(), "no".to_string()];
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("QUESTION [q_42]: Pick one:"));
        assert!(lines[1].contains("1: yes"));
        assert!(lines[2].contains("2: no"));
    }

    #[test]
    fn test_format_done_with_summary() {
        let mut entry = make_entry(LogEntryType::Done);
        entry.summary = Some("All tasks complete".to_string());
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("DONE: All tasks complete"));
    }

    #[test]
    fn test_format_done_falls_back_to_content() {
        let mut entry = make_entry(LogEntryType::Done);
        entry.content = Some("finished".to_string());
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("DONE: finished"));
    }

    #[test]
    fn test_format_done_no_summary_or_content() {
        let entry = make_entry(LogEntryType::Done);
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("DONE: (no summary)"));
    }

    #[test]
    fn test_format_error() {
        let mut entry = make_entry(LogEntryType::Error);
        entry.content = Some("connection timeout".to_string());
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("ERROR: connection timeout"));
    }

    #[test]
    fn test_format_system_message() {
        let mut entry = make_entry(LogEntryType::System);
        entry.content = Some("session started".to_string());
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("system: session started"));
    }

    #[test]
    fn test_format_answer() {
        let mut entry = make_entry(LogEntryType::Answer);
        entry.question_id = Some("q_1".to_string());
        entry.content = Some("yes".to_string());
        let lines = format_log_entry_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("answer[q_1]: yes"));
    }
}

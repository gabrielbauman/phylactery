//! `phyl watch` — live feed of all sessions via SSE, with inline question answering.

use anyhow::bail;
use crate::client;
use http_body_util::BodyExt;
use phyl_core::LogEntry;
use std::io::{self, BufRead, Write};

pub async fn run() -> anyhow::Result<()> {
    let socket = client::socket_path();

    eprintln!("Watching all sessions (Ctrl-C to quit)...");

    let (status, body) = client::get_stream(&socket, "/feed").await?;

    if !status.is_success() {
        bail!("HTTP {}: feed endpoint returned {}", status.as_u16(), status);
    }

    // Read SSE stream frame by frame.
    let mut body = body;
    let mut buf = String::new();

    loop {
        let frame = match body.frame().await {
            Some(Ok(f)) => f,
            Some(Err(e)) => {
                eprintln!("watch: stream error: {e}");
                break;
            }
            None => break,
        };

        if let Some(data) = frame.data_ref() {
            let chunk = String::from_utf8_lossy(data);
            buf.push_str(&chunk);

            // Process complete SSE messages (separated by double newlines).
            while let Some(pos) = buf.find("\n\n") {
                let msg = buf[..pos].to_string();
                buf = buf[pos + 2..].to_string();
                process_sse_message(&msg, &socket).await;
            }
        }
    }

    Ok(())
}

/// Process a single SSE message block.
async fn process_sse_message(msg: &str, socket: &str) {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in msg.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data = rest.trim().to_string();
        }
    }

    if data.is_empty() {
        return;
    }

    // Parse the event data JSON.
    let val: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return,
    };

    let session_id = val
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");

    // Parse the embedded log entry.
    let entry: LogEntry = match serde_json::from_value(
        val.get("entry").cloned().unwrap_or(serde_json::Value::Null),
    ) {
        Ok(e) => e,
        Err(_) => return,
    };

    let ts = entry.ts.format("%H:%M:%S");
    let short_id = &session_id[..8.min(session_id.len())];

    match event_type.as_str() {
        "question" => {
            let qid = entry.id.as_deref().unwrap_or("?");
            let text = entry.content.as_deref().unwrap_or("");
            println!("[{ts}] [{short_id}] QUESTION [{qid}]: {text}");
            if !entry.options.is_empty() {
                for (i, opt) in entry.options.iter().enumerate() {
                    println!("  {}: {opt}", i + 1);
                }
            }

            // Prompt for answer inline.
            print!("  > answer: ");
            let _ = io::stdout().flush();

            let stdin = io::stdin();
            let mut answer = String::new();
            if stdin.lock().read_line(&mut answer).is_ok() {
                let answer = answer.trim();
                if !answer.is_empty() {
                    let body = serde_json::json!({
                        "question_id": qid,
                        "content": answer,
                    })
                    .to_string();
                    let path = format!("/sessions/{session_id}/events");
                    match client::post(socket, &path, &body).await {
                        Ok(_) => eprintln!("  (answer sent)"),
                        Err(e) => eprintln!("  (failed to send answer: {e})"),
                    }
                }
            }
        }
        "done" => {
            let summary = entry
                .summary
                .as_deref()
                .or(entry.content.as_deref())
                .unwrap_or("(no summary)");
            println!("[{ts}] [{short_id}] DONE: {summary}");
        }
        "error" => {
            let msg = entry.content.as_deref().unwrap_or("unknown error");
            println!("[{ts}] [{short_id}] ERROR: {msg}");
        }
        _ => {
            if let Some(ref content) = entry.content {
                println!(
                    "[{ts}] [{short_id}] {}: {content}",
                    format!("{:?}", entry.entry_type).to_lowercase()
                );
            }
        }
    }
}

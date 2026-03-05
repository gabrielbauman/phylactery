//! `phyl-tool-psyche` — Server-mode tool for structured internal state management.
//!
//! Tools:
//! - `open_concern` — Open a new concern with type, description, salience, tags
//! - `touch_concern` — Return to a concern, record what's new, adjust salience
//! - `resolve_concern` — Close the gap, record outcome
//! - `abandon_concern` — Explicitly drop a concern with a reason
//! - `surface_concerns` — Query top concerns by salience, type, state, tags
//! - `commit_to` — Declare a concrete action on a conative concern with a deadline
//! - `report_commitment` — Report a commitment as fulfilled or broken
//! - `kb_record` — Store a structured fact
//! - `kb_retrieve` — Query structured facts
//! - `kb_invalidate` — Mark a fact as invalid
//! - `escalate` — Escalate to operator with structured metadata

use chrono::Utc;
use phyl_core::{ServerRequest, ServerResponse, ToolMode, ToolSpec};
use rusqlite::Connection;
use std::io::{self, BufRead, Write};
use uuid::Uuid;

mod db;

fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "open_concern".to_string(),
            description: "Open a new concern. Use when you notice something with genuine pull \
                — a question you keep returning to, something you want, something you intend \
                to do. Do not open a concern as a placeholder. The test is whether the gap \
                described would change your behavior if it closed. If type is 'conative', \
                tension is required: state specifically what is wrong or incomplete about now."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "What this concern is about"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["epistemic", "appetitive", "conative"],
                        "description": "epistemic = wants knowledge, appetitive = wants experience/possession, conative = wants action/change"
                    },
                    "tension": {
                        "type": "string",
                        "description": "Required for conative concerns: what specifically is wrong or incomplete about now"
                    },
                    "salience": {
                        "type": "number",
                        "description": "Initial salience 0-1 (default 0.5)"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tags for categorization and clustering"
                    },
                    "spawned_from": {
                        "type": "string",
                        "description": "concern_id of the concern that led to this one"
                    }
                },
                "required": ["description", "type"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "touch_concern".to_string(),
            description: "Return to a concern. Call whenever you reference it, notice its \
                urgency shifting, or have something new to say about it. The note field is not \
                a summary — write what is new: what you noticed, what changed, what you now \
                think that you did not before."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "concern_id": {
                        "type": "string",
                        "description": "ID of the concern to touch"
                    },
                    "note": {
                        "type": "string",
                        "description": "What is new — what you noticed, what changed"
                    },
                    "salience_delta": {
                        "type": "number",
                        "description": "Adjustment to salience (-1 to 1). Use when something external changes the stakes."
                    }
                },
                "required": ["concern_id"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "resolve_concern".to_string(),
            description: "Close a concern because the gap is genuinely closed. Do not resolve \
                because you stopped thinking about it — that is abandonment. Outcome must state \
                what actually changed, not restate the original description."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "concern_id": {
                        "type": "string",
                        "description": "ID of the concern to resolve"
                    },
                    "outcome": {
                        "type": "string",
                        "description": "What actually changed — not just 'completed'"
                    },
                    "spawned_concerns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "IDs of new concerns opened as a result of resolving this one"
                    }
                },
                "required": ["concern_id", "outcome"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "abandon_concern".to_string(),
            description: "Explicitly drop a concern. Not because salience decayed passively — \
                the subconscious pass handles that. This is for deliberate closure. The reason \
                field is required and not a formality."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "concern_id": {
                        "type": "string",
                        "description": "ID of the concern to abandon"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Specific reason for abandoning this concern"
                    }
                },
                "required": ["concern_id", "reason"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "surface_concerns".to_string(),
            description: "Query top concerns by salience. Use at session start to orient, or \
                during a session to check whether current work connects to something previously \
                flagged. The combination of touch_count and age tells you something salience \
                alone does not."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "n": {
                        "type": "integer",
                        "description": "Number of concerns to return (default 5)"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["epistemic", "appetitive", "conative"],
                        "description": "Filter by concern type"
                    },
                    "min_salience": {
                        "type": "number",
                        "description": "Minimum salience threshold"
                    },
                    "include_states": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["open", "committed", "resolved", "abandoned"] },
                        "description": "States to include (default: open, committed)"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter by tags (any match)"
                    }
                },
                "required": []
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "commit_to".to_string(),
            description: "Declare a concrete action on a conative concern with a deadline. \
                Action must be concrete enough that a future you can evaluate unambiguously \
                whether it was done. 'Look into X' is not an action. 'Make a GET request to X \
                and record available endpoints' is."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "concern_id": {
                        "type": "string",
                        "description": "ID of the conative concern to commit on"
                    },
                    "action": {
                        "type": "string",
                        "description": "The concrete action to take"
                    },
                    "scheduled_for": {
                        "type": "string",
                        "description": "When to do it: ISO 8601 datetime or relative interval (e.g. '2h', '1d')"
                    },
                    "fallback": {
                        "type": "string",
                        "description": "What to do if blocked"
                    }
                },
                "required": ["concern_id", "action", "scheduled_for"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "report_commitment".to_string(),
            description: "Report a commitment as fulfilled or broken. Must be called for every \
                commitment that came due, regardless of outcome. For fulfilled: write what \
                actually happened. For broken: write specifically why."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "commitment_id": {
                        "type": "string",
                        "description": "ID of the commitment to report"
                    },
                    "state": {
                        "type": "string",
                        "enum": ["fulfilled", "broken"],
                        "description": "Whether the commitment was fulfilled or broken"
                    },
                    "note": {
                        "type": "string",
                        "description": "What happened (required for both states)"
                    },
                    "spawned_concerns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "IDs of concerns opened as a result"
                    }
                },
                "required": ["commitment_id", "state", "note"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "kb_record".to_string(),
            description: "Store a structured fact. Use for things with clear provenance, a \
                subject, and a confidence level — not for raw notes or speculation."
                .to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "subject": {
                        "type": "string",
                        "description": "The entity this fact is about"
                    },
                    "predicate": {
                        "type": "string",
                        "description": "The relationship or property"
                    },
                    "object": {
                        "type": "string",
                        "description": "The value or target"
                    },
                    "confidence": {
                        "type": "number",
                        "description": "Confidence 0-1"
                    },
                    "source": {
                        "type": "string",
                        "description": "Where this fact came from (url, session_id, observation, inference)"
                    },
                    "concern_id": {
                        "type": "string",
                        "description": "Link to the concern that generated this"
                    },
                    "expires_at": {
                        "type": "string",
                        "description": "ISO 8601 expiry for time-sensitive facts"
                    }
                },
                "required": ["subject", "predicate", "object", "confidence", "source"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "kb_retrieve".to_string(),
            description: "Query structured facts from the knowledge base.".to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "subject": {
                        "type": "string",
                        "description": "Filter by subject"
                    },
                    "predicate": {
                        "type": "string",
                        "description": "Filter by predicate"
                    },
                    "min_confidence": {
                        "type": "number",
                        "description": "Minimum confidence threshold"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (default 10)"
                    }
                },
                "required": []
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "kb_invalidate".to_string(),
            description: "Mark a knowledge base record as invalid.".to_string(),
            mode: ToolMode::Server,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "record_id": {
                        "type": "string",
                        "description": "ID of the record to invalidate"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why this record is no longer valid"
                    }
                },
                "required": ["record_id", "reason"]
            }),
            sandbox: None,
        },
        ToolSpec {
            name: "escalate".to_string(),
            description: "Escalate something to the human operator with structured metadata. \
                Use for blocks, decisions needed, FYI notices, or capability requests. \
                For 'blocked' and 'decision_required' kinds, the response includes a \
                signal to notify the operator immediately. The escalation is recorded \
                in the psyche database for tracking."
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
    ]
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

    eprintln!("phyl-tool-psyche: use --spec or --serve");
    std::process::exit(1);
}

fn serve() {
    let home = phyl_core::home_dir();
    let db_path = home.join("psyche.db");

    let conn = match db::open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("phyl-tool-psyche: failed to open database: {e}");
            std::process::exit(1);
        }
    };

    let session_number: u64 = std::env::var("PHYLACTERY_SESSION_NUMBER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = stdin.lock();
    let mut writer = stdout.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("phyl-tool-psyche: stdin read error: {e}");
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
                eprintln!("phyl-tool-psyche: ignoring unrecognized input: {trimmed}");
                continue;
            }
        };

        let response = dispatch(&conn, &req, session_number);
        write_response(&mut writer, &response);
    }

    eprintln!("phyl-tool-psyche: stdin closed, exiting");
}

fn dispatch(conn: &Connection, req: &ServerRequest, session_number: u64) -> ServerResponse {
    match req.name.as_str() {
        "open_concern" => handle_open_concern(conn, req, session_number),
        "touch_concern" => handle_touch_concern(conn, req, session_number),
        "resolve_concern" => handle_resolve_concern(conn, req),
        "abandon_concern" => handle_abandon_concern(conn, req),
        "surface_concerns" => handle_surface_concerns(conn, req),
        "commit_to" => handle_commit_to(conn, req),
        "report_commitment" => handle_report_commitment(conn, req),
        "kb_record" => handle_kb_record(conn, req),
        "kb_retrieve" => handle_kb_retrieve(conn, req),
        "kb_invalidate" => handle_kb_invalidate(conn, req),
        "escalate" => handle_escalate(conn, req),
        other => ServerResponse {
            id: req.id.clone(),
            output: None,
            error: Some(format!("unknown tool: {other}")),
            signal: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Concern lifecycle handlers
// ---------------------------------------------------------------------------

fn handle_open_concern(
    conn: &Connection,
    req: &ServerRequest,
    session_number: u64,
) -> ServerResponse {
    let description = match req.arguments.get("description").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return error_response(&req.id, "missing required parameter: description"),
    };

    let concern_type = match req.arguments.get("type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return error_response(&req.id, "missing required parameter: type"),
    };

    // Validate concern type.
    if !matches!(concern_type, "epistemic" | "appetitive" | "conative") {
        return error_response(
            &req.id,
            "type must be 'epistemic', 'appetitive', or 'conative'",
        );
    }

    let tension = req.arguments.get("tension").and_then(|v| v.as_str());

    // Conative concerns require tension.
    if concern_type == "conative" && tension.is_none() {
        return error_response(
            &req.id,
            "conative concerns require a tension: state specifically what is wrong or incomplete",
        );
    }

    let salience = req
        .arguments
        .get("salience")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);

    let tags: Vec<String> = req
        .arguments
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let spawned_from = req.arguments.get("spawned_from").and_then(|v| v.as_str());

    let concern_id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());

    match conn.execute(
        "INSERT INTO concerns (concern_id, description, type, tension, state, salience, \
         tags, origin, touch_count, created_session, touched_session, created_at, \
         touched_at, spawned_from, spawned) \
         VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6, 'session', 0, ?7, ?7, ?8, ?8, ?9, '[]')",
        rusqlite::params![
            concern_id,
            description,
            concern_type,
            tension,
            salience,
            tags_json,
            session_number,
            now.to_rfc3339(),
            spawned_from,
        ],
    ) {
        Ok(_) => {
            // If spawned_from, update parent's spawned array.
            if let Some(parent_id) = spawned_from {
                let _ = db::append_to_json_array(
                    conn,
                    "concerns",
                    "spawned",
                    "concern_id",
                    parent_id,
                    &concern_id,
                );
            }

            ok_response(
                &req.id,
                &serde_json::json!({
                    "concern_id": concern_id,
                    "description": description,
                    "type": concern_type,
                    "salience": salience,
                }),
            )
        }
        Err(e) => error_response(&req.id, &format!("database error: {e}")),
    }
}

fn handle_touch_concern(
    conn: &Connection,
    req: &ServerRequest,
    session_number: u64,
) -> ServerResponse {
    let concern_id = match req.arguments.get("concern_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return error_response(&req.id, "missing required parameter: concern_id"),
    };

    let note = req.arguments.get("note").and_then(|v| v.as_str());
    let salience_delta = req
        .arguments
        .get("salience_delta")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let now = Utc::now();

    // Read current state.
    let row = conn.query_row(
        "SELECT state, salience, touch_count FROM concerns WHERE concern_id = ?1",
        [concern_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, u32>(2)?,
            ))
        },
    );

    let (state, current_salience, touch_count) = match row {
        Ok(r) => r,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return error_response(&req.id, &format!("concern not found: {concern_id}"));
        }
        Err(e) => return error_response(&req.id, &format!("database error: {e}")),
    };

    if state != "open" && state != "committed" {
        return error_response(
            &req.id,
            &format!("cannot touch concern in state '{state}' (must be open or committed)"),
        );
    }

    let new_salience = (current_salience + salience_delta).clamp(0.0, 1.0);
    let new_touch_count = touch_count + 1;

    let note_suffix = note.map(|n| format!(" Note: {n}")).unwrap_or_default();

    match conn.execute(
        "UPDATE concerns SET salience = ?1, touch_count = ?2, touched_session = ?3, \
         touched_at = ?4 WHERE concern_id = ?5",
        rusqlite::params![
            new_salience,
            new_touch_count,
            session_number,
            now.to_rfc3339(),
            concern_id,
        ],
    ) {
        Ok(_) => ok_response(
            &req.id,
            &serde_json::json!({
                "concern_id": concern_id,
                "new_salience": new_salience,
                "touch_count": new_touch_count,
                "note": format!("Touched concern.{note_suffix}"),
            }),
        ),
        Err(e) => error_response(&req.id, &format!("database error: {e}")),
    }
}

fn handle_resolve_concern(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let concern_id = match req.arguments.get("concern_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return error_response(&req.id, "missing required parameter: concern_id"),
    };

    let outcome = match req.arguments.get("outcome").and_then(|v| v.as_str()) {
        Some(o) => o,
        None => return error_response(&req.id, "missing required parameter: outcome"),
    };

    let spawned: Vec<String> = req
        .arguments
        .get("spawned_concerns")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Verify concern exists and is open or committed.
    let state = match conn.query_row(
        "SELECT state FROM concerns WHERE concern_id = ?1",
        [concern_id],
        |row| row.get::<_, String>(0),
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return error_response(&req.id, &format!("concern not found: {concern_id}"));
        }
        Err(e) => return error_response(&req.id, &format!("database error: {e}")),
    };

    if state != "open" && state != "committed" {
        return error_response(
            &req.id,
            &format!("cannot resolve concern in state '{state}'"),
        );
    }

    let now = Utc::now();
    let spawned_json = serde_json::to_string(&spawned).unwrap_or_else(|_| "[]".to_string());

    match conn.execute(
        "UPDATE concerns SET state = 'resolved', outcome = ?1, resolved_at = ?2, \
         spawned = ?3 WHERE concern_id = ?4",
        rusqlite::params![outcome, now.to_rfc3339(), spawned_json, concern_id],
    ) {
        Ok(_) => ok_response(
            &req.id,
            &serde_json::json!({
                "concern_id": concern_id,
                "resolved_at": now.to_rfc3339(),
                "outcome": outcome,
            }),
        ),
        Err(e) => error_response(&req.id, &format!("database error: {e}")),
    }
}

fn handle_abandon_concern(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let concern_id = match req.arguments.get("concern_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return error_response(&req.id, "missing required parameter: concern_id"),
    };

    let reason = match req.arguments.get("reason").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => return error_response(&req.id, "missing required parameter: reason"),
    };

    // Verify concern exists and is open or committed.
    let state = match conn.query_row(
        "SELECT state FROM concerns WHERE concern_id = ?1",
        [concern_id],
        |row| row.get::<_, String>(0),
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return error_response(&req.id, &format!("concern not found: {concern_id}"));
        }
        Err(e) => return error_response(&req.id, &format!("database error: {e}")),
    };

    if state != "open" && state != "committed" {
        return error_response(
            &req.id,
            &format!("cannot abandon concern in state '{state}'"),
        );
    }

    let now = Utc::now();

    match conn.execute(
        "UPDATE concerns SET state = 'abandoned', abandon_reason = ?1, abandoned_at = ?2 \
         WHERE concern_id = ?3",
        rusqlite::params![reason, now.to_rfc3339(), concern_id],
    ) {
        Ok(_) => ok_response(
            &req.id,
            &serde_json::json!({
                "concern_id": concern_id,
                "abandoned_at": now.to_rfc3339(),
            }),
        ),
        Err(e) => error_response(&req.id, &format!("database error: {e}")),
    }
}

fn handle_surface_concerns(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let n = req.arguments.get("n").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    let type_filter = req.arguments.get("type").and_then(|v| v.as_str());

    let min_salience = req
        .arguments
        .get("min_salience")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let include_states: Vec<String> = req
        .arguments
        .get("include_states")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| vec!["open".to_string(), "committed".to_string()]);

    let tag_filters: Vec<String> = req
        .arguments
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Build parameterized query.
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1_usize;

    // State filter with parameterized placeholders.
    let state_placeholders: Vec<String> = include_states
        .iter()
        .map(|s| {
            let ph = format!("?{param_idx}");
            param_idx += 1;
            params.push(Box::new(s.clone()));
            ph
        })
        .collect();
    let state_clause = format!("state IN ({})", state_placeholders.join(", "));

    // Salience filter.
    let salience_ph = format!("?{param_idx}");
    param_idx += 1;
    params.push(Box::new(min_salience));
    let mut conditions = vec![state_clause, format!("salience >= {salience_ph}")];

    // Type filter.
    if let Some(t) = type_filter {
        let ph = format!("?{param_idx}");
        param_idx += 1;
        params.push(Box::new(t.to_string()));
        conditions.push(format!("type = {ph}"));
    }

    // Tag filtering: check if any requested tag appears in the JSON array.
    for tag in &tag_filters {
        let ph = format!("?{param_idx}");
        param_idx += 1;
        params.push(Box::new(format!("%\"{tag}\"%")));
        conditions.push(format!("tags LIKE {ph}"));
    }

    // Limit.
    let limit_ph = format!("?{param_idx}");
    params.push(Box::new(n as i64));

    let where_clause = conditions.join(" AND ");
    let query = format!(
        "SELECT concern_id, description, type, tension, state, salience, tags, origin, \
         touch_count, created_session, touched_session, created_at, touched_at, \
         resolved_at, abandoned_at, outcome, abandon_reason, spawned_from, spawned \
         FROM concerns WHERE {where_clause} ORDER BY salience DESC LIMIT {limit_ph}"
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = match conn.prepare(&query) {
        Ok(s) => s,
        Err(e) => return error_response(&req.id, &format!("query error: {e}")),
    };

    let concerns: Vec<serde_json::Value> = match stmt.query_map(param_refs.as_slice(), |row| {
        Ok(serde_json::json!({
            "concern_id": row.get::<_, String>(0)?,
            "description": row.get::<_, String>(1)?,
            "type": row.get::<_, String>(2)?,
            "tension": row.get::<_, Option<String>>(3)?,
            "state": row.get::<_, String>(4)?,
            "salience": row.get::<_, f64>(5)?,
            "tags": serde_json::from_str::<serde_json::Value>(
                &row.get::<_, String>(6)?
            ).unwrap_or(serde_json::json!([])),
            "origin": row.get::<_, String>(7)?,
            "touch_count": row.get::<_, u32>(8)?,
            "created_session": row.get::<_, u64>(9)?,
            "touched_session": row.get::<_, u64>(10)?,
            "created_at": row.get::<_, String>(11)?,
            "touched_at": row.get::<_, String>(12)?,
            "resolved_at": row.get::<_, Option<String>>(13)?,
            "abandoned_at": row.get::<_, Option<String>>(14)?,
            "outcome": row.get::<_, Option<String>>(15)?,
            "abandon_reason": row.get::<_, Option<String>>(16)?,
            "spawned_from": row.get::<_, Option<String>>(17)?,
            "spawned": serde_json::from_str::<serde_json::Value>(
                &row.get::<_, String>(18)?
            ).unwrap_or(serde_json::json!([])),
        }))
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(e) => return error_response(&req.id, &format!("query error: {e}")),
    };

    ok_response(
        &req.id,
        &serde_json::json!({
            "count": concerns.len(),
            "concerns": concerns,
        }),
    )
}

// ---------------------------------------------------------------------------
// Commitment handlers
// ---------------------------------------------------------------------------

fn handle_commit_to(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let concern_id = match req.arguments.get("concern_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return error_response(&req.id, "missing required parameter: concern_id"),
    };

    let action = match req.arguments.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return error_response(&req.id, "missing required parameter: action"),
    };

    let scheduled_for_str = match req.arguments.get("scheduled_for").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response(&req.id, "missing required parameter: scheduled_for"),
    };

    let fallback = req.arguments.get("fallback").and_then(|v| v.as_str());

    // Parse scheduled_for.
    let scheduled_for = match phyl_core::parse_time_spec(scheduled_for_str) {
        Ok(dt) => dt,
        Err(e) => return error_response(&req.id, &format!("invalid scheduled_for: {e}")),
    };

    // Verify concern exists, is conative, and is open.
    let row = conn.query_row(
        "SELECT type, state FROM concerns WHERE concern_id = ?1",
        [concern_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    );

    match row {
        Ok((concern_type, state)) => {
            if concern_type != "conative" {
                return error_response(
                    &req.id,
                    &format!(
                        "can only commit to conative concerns, this concern is '{concern_type}'"
                    ),
                );
            }
            if state != "open" {
                return error_response(
                    &req.id,
                    &format!("can only commit to open concerns, this concern is '{state}'"),
                );
            }
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return error_response(&req.id, &format!("concern not found: {concern_id}"));
        }
        Err(e) => return error_response(&req.id, &format!("database error: {e}")),
    }

    let commitment_id = Uuid::new_v4().to_string();
    let now = Utc::now();

    // Insert commitment.
    if let Err(e) = conn.execute(
        "INSERT INTO commitments (commitment_id, concern_id, action, scheduled_for, \
         fallback, state, created_at, spawned_concerns) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, '[]')",
        rusqlite::params![
            commitment_id,
            concern_id,
            action,
            scheduled_for.to_rfc3339(),
            fallback,
            now.to_rfc3339(),
        ],
    ) {
        return error_response(&req.id, &format!("database error: {e}"));
    }

    // Move concern to committed state.
    let _ = conn.execute(
        "UPDATE concerns SET state = 'committed' WHERE concern_id = ?1",
        [concern_id],
    );

    ok_response(
        &req.id,
        &serde_json::json!({
            "commitment_id": commitment_id,
            "concern_id": concern_id,
            "action": action,
            "scheduled_for": scheduled_for.to_rfc3339(),
        }),
    )
}

fn handle_report_commitment(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let commitment_id = match req.arguments.get("commitment_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return error_response(&req.id, "missing required parameter: commitment_id"),
    };

    let state = match req.arguments.get("state").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response(&req.id, "missing required parameter: state"),
    };

    if !matches!(state, "fulfilled" | "broken") {
        return error_response(&req.id, "state must be 'fulfilled' or 'broken'");
    }

    let note = match req.arguments.get("note").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response(&req.id, "missing required parameter: note"),
    };

    let spawned: Vec<String> = req
        .arguments
        .get("spawned_concerns")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Verify commitment exists and is pending.
    let current_state = match conn.query_row(
        "SELECT state FROM commitments WHERE commitment_id = ?1",
        [commitment_id],
        |row| row.get::<_, String>(0),
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return error_response(&req.id, &format!("commitment not found: {commitment_id}"));
        }
        Err(e) => return error_response(&req.id, &format!("database error: {e}")),
    };

    if current_state != "pending" {
        return error_response(
            &req.id,
            &format!("commitment already reported as '{current_state}'"),
        );
    }

    let now = Utc::now();
    let spawned_json = serde_json::to_string(&spawned).unwrap_or_else(|_| "[]".to_string());

    match conn.execute(
        "UPDATE commitments SET state = ?1, note = ?2, reported_at = ?3, \
         spawned_concerns = ?4 WHERE commitment_id = ?5",
        rusqlite::params![state, note, now.to_rfc3339(), spawned_json, commitment_id,],
    ) {
        Ok(_) => ok_response(
            &req.id,
            &serde_json::json!({
                "commitment_id": commitment_id,
                "state": state,
                "reported_at": now.to_rfc3339(),
            }),
        ),
        Err(e) => error_response(&req.id, &format!("database error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// KB handlers
// ---------------------------------------------------------------------------

fn handle_kb_record(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let subject = match req.arguments.get("subject").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response(&req.id, "missing required parameter: subject"),
    };
    let predicate = match req.arguments.get("predicate").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return error_response(&req.id, "missing required parameter: predicate"),
    };
    let object = match req.arguments.get("object").and_then(|v| v.as_str()) {
        Some(o) => o,
        None => return error_response(&req.id, "missing required parameter: object"),
    };
    let confidence = match req.arguments.get("confidence").and_then(|v| v.as_f64()) {
        Some(c) => c.clamp(0.0, 1.0),
        None => return error_response(&req.id, "missing required parameter: confidence"),
    };
    let source = match req.arguments.get("source").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response(&req.id, "missing required parameter: source"),
    };

    let concern_id = req.arguments.get("concern_id").and_then(|v| v.as_str());
    let expires_at = req.arguments.get("expires_at").and_then(|v| v.as_str());

    let record_id = Uuid::new_v4().to_string();
    let now = Utc::now();

    match conn.execute(
        "INSERT INTO kb_records (record_id, subject, predicate, object, confidence, \
         source, concern_id, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            record_id,
            subject,
            predicate,
            object,
            confidence,
            source,
            concern_id,
            now.to_rfc3339(),
            expires_at,
        ],
    ) {
        Ok(_) => ok_response(&req.id, &serde_json::json!({ "record_id": record_id })),
        Err(e) => error_response(&req.id, &format!("database error: {e}")),
    }
}

fn handle_kb_retrieve(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let subject = req.arguments.get("subject").and_then(|v| v.as_str());
    let predicate = req.arguments.get("predicate").and_then(|v| v.as_str());
    let min_confidence = req
        .arguments
        .get("min_confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let limit = req
        .arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1_usize;

    // Confidence filter.
    let conf_ph = format!("?{param_idx}");
    param_idx += 1;
    params.push(Box::new(min_confidence));
    let mut conditions = vec![
        "invalidated_at IS NULL".to_string(),
        format!("confidence >= {conf_ph}"),
    ];

    if let Some(s) = subject {
        let ph = format!("?{param_idx}");
        param_idx += 1;
        params.push(Box::new(s.to_string()));
        conditions.push(format!("subject = {ph}"));
    }
    if let Some(p) = predicate {
        let ph = format!("?{param_idx}");
        param_idx += 1;
        params.push(Box::new(p.to_string()));
        conditions.push(format!("predicate = {ph}"));
    }

    // Exclude expired records.
    let now_ph = format!("?{param_idx}");
    param_idx += 1;
    params.push(Box::new(Utc::now().to_rfc3339()));
    conditions.push(format!("(expires_at IS NULL OR expires_at > {now_ph})"));

    // Limit.
    let limit_ph = format!("?{param_idx}");
    params.push(Box::new(limit as i64));

    let where_clause = conditions.join(" AND ");
    let query = format!(
        "SELECT record_id, subject, predicate, object, confidence, source, concern_id, \
         created_at, expires_at \
         FROM kb_records WHERE {where_clause} ORDER BY confidence DESC LIMIT {limit_ph}"
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = match conn.prepare(&query) {
        Ok(s) => s,
        Err(e) => return error_response(&req.id, &format!("query error: {e}")),
    };

    let records: Vec<serde_json::Value> = match stmt.query_map(param_refs.as_slice(), |row| {
        Ok(serde_json::json!({
            "record_id": row.get::<_, String>(0)?,
            "subject": row.get::<_, String>(1)?,
            "predicate": row.get::<_, String>(2)?,
            "object": row.get::<_, String>(3)?,
            "confidence": row.get::<_, f64>(4)?,
            "source": row.get::<_, String>(5)?,
            "concern_id": row.get::<_, Option<String>>(6)?,
            "created_at": row.get::<_, String>(7)?,
            "expires_at": row.get::<_, Option<String>>(8)?,
        }))
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(e) => return error_response(&req.id, &format!("query error: {e}")),
    };

    ok_response(
        &req.id,
        &serde_json::json!({
            "count": records.len(),
            "records": records,
        }),
    )
}

fn handle_kb_invalidate(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let record_id = match req.arguments.get("record_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return error_response(&req.id, "missing required parameter: record_id"),
    };
    let reason = match req.arguments.get("reason").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => return error_response(&req.id, "missing required parameter: reason"),
    };

    // Check record exists.
    match conn.query_row(
        "SELECT invalidated_at FROM kb_records WHERE record_id = ?1",
        [record_id],
        |row| row.get::<_, Option<String>>(0),
    ) {
        Ok(Some(_)) => return error_response(&req.id, "record already invalidated"),
        Ok(None) => {} // proceed
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return error_response(&req.id, &format!("record not found: {record_id}"));
        }
        Err(e) => return error_response(&req.id, &format!("database error: {e}")),
    }

    let now = Utc::now();

    match conn.execute(
        "UPDATE kb_records SET invalidated_at = ?1, invalidation_reason = ?2 \
         WHERE record_id = ?3",
        rusqlite::params![now.to_rfc3339(), reason, record_id],
    ) {
        Ok(_) => ok_response(
            &req.id,
            &serde_json::json!({
                "record_id": record_id,
                "invalidated_at": now.to_rfc3339(),
            }),
        ),
        Err(e) => error_response(&req.id, &format!("database error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Escalation handler
// ---------------------------------------------------------------------------

fn handle_escalate(conn: &Connection, req: &ServerRequest) -> ServerResponse {
    let subject = match req.arguments.get("subject").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response(&req.id, "missing required parameter: subject"),
    };
    let body = match req.arguments.get("body").and_then(|v| v.as_str()) {
        Some(b) => b,
        None => return error_response(&req.id, "missing required parameter: body"),
    };
    let kind = match req.arguments.get("kind").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return error_response(&req.id, "missing required parameter: kind"),
    };

    if !matches!(
        kind,
        "blocked" | "decision_required" | "fyi" | "request_capability"
    ) {
        return error_response(
            &req.id,
            "kind must be 'blocked', 'decision_required', 'fyi', or 'request_capability'",
        );
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

    if let Err(e) = conn.execute(
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
    ) {
        return error_response(&req.id, &format!("database error: {e}"));
    }

    let needs_notify = kind == "blocked" || kind == "decision_required";

    let output = serde_json::json!({
        "escalation_id": escalation_id,
        "subject": subject,
        "kind": kind,
        "urgency": urgency,
        "notify_operator": needs_notify,
    });

    // For blocking escalations, emit a signal so phyl-run can trigger ask_human.
    let signal = if needs_notify {
        Some(format!("escalation:{escalation_id}"))
    } else {
        None
    };

    ServerResponse {
        id: req.id.clone(),
        output: Some(serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())),
        error: None,
        signal,
    }
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn ok_response(id: &str, value: &serde_json::Value) -> ServerResponse {
    ServerResponse {
        id: id.to_string(),
        output: Some(serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())),
        error: None,
        signal: None,
    }
}

fn error_response(id: &str, message: &str) -> ServerResponse {
    ServerResponse {
        id: id.to_string(),
        output: None,
        error: Some(message.to_string()),
        signal: None,
    }
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
        assert_eq!(specs.len(), 11);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"open_concern"));
        assert!(names.contains(&"touch_concern"));
        assert!(names.contains(&"resolve_concern"));
        assert!(names.contains(&"abandon_concern"));
        assert!(names.contains(&"surface_concerns"));
        assert!(names.contains(&"commit_to"));
        assert!(names.contains(&"report_commitment"));
        assert!(names.contains(&"kb_record"));
        assert!(names.contains(&"kb_retrieve"));
        assert!(names.contains(&"kb_invalidate"));
        assert!(names.contains(&"escalate"));
        for spec in &specs {
            assert_eq!(spec.mode, ToolMode::Server);
        }
    }

    #[test]
    fn test_spec_serialization() {
        let specs = tool_specs();
        let json = serde_json::to_string_pretty(&specs).unwrap();
        let parsed: Vec<ToolSpec> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 11);
    }

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    fn make_request(id: &str, name: &str, args: serde_json::Value) -> ServerRequest {
        ServerRequest {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args,
        }
    }

    #[test]
    fn test_open_concern_epistemic() {
        let conn = test_db();
        let req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "How does salience decay work?",
                "type": "epistemic",
                "tags": ["psyche", "design"]
            }),
        );
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let output: serde_json::Value =
            serde_json::from_str(resp.output.as_ref().unwrap()).unwrap();
        assert!(output.get("concern_id").is_some());
        assert_eq!(output["type"], "epistemic");
    }

    #[test]
    fn test_open_concern_conative_requires_tension() {
        let conn = test_db();
        let req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "Improve the deploy pipeline",
                "type": "conative"
            }),
        );
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_some());
        assert!(resp.error.unwrap().contains("tension"));
    }

    #[test]
    fn test_open_concern_conative_with_tension() {
        let conn = test_db();
        let req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "Fix the deploy pipeline",
                "type": "conative",
                "tension": "The rollback path has no coverage and failures are silent"
            }),
        );
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    }

    #[test]
    fn test_touch_concern() {
        let conn = test_db();

        // Open a concern first.
        let open_req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "Test concern",
                "type": "epistemic",
                "salience": 0.5
            }),
        );
        let open_resp = dispatch(&conn, &open_req, 1);
        let output: serde_json::Value =
            serde_json::from_str(open_resp.output.as_ref().unwrap()).unwrap();
        let concern_id = output["concern_id"].as_str().unwrap();

        // Touch it.
        let touch_req = make_request(
            "2",
            "touch_concern",
            serde_json::json!({
                "concern_id": concern_id,
                "note": "Found a relevant paper",
                "salience_delta": 0.1
            }),
        );
        let touch_resp = dispatch(&conn, &touch_req, 2);
        assert!(touch_resp.error.is_none());
        let touch_output: serde_json::Value =
            serde_json::from_str(touch_resp.output.as_ref().unwrap()).unwrap();
        assert_eq!(touch_output["new_salience"], 0.6);
        assert_eq!(touch_output["touch_count"], 1);
    }

    #[test]
    fn test_resolve_concern() {
        let conn = test_db();

        let open_req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "What is X?",
                "type": "epistemic"
            }),
        );
        let open_resp = dispatch(&conn, &open_req, 1);
        let output: serde_json::Value =
            serde_json::from_str(open_resp.output.as_ref().unwrap()).unwrap();
        let concern_id = output["concern_id"].as_str().unwrap();

        let resolve_req = make_request(
            "2",
            "resolve_concern",
            serde_json::json!({
                "concern_id": concern_id,
                "outcome": "X is a framework for Y, confirmed via documentation"
            }),
        );
        let resolve_resp = dispatch(&conn, &resolve_req, 1);
        assert!(resolve_resp.error.is_none());

        // Cannot resolve again.
        let resolve2 = dispatch(&conn, &resolve_req, 1);
        assert!(resolve2.error.is_some());
    }

    #[test]
    fn test_abandon_concern() {
        let conn = test_db();

        let open_req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "Investigate Z",
                "type": "epistemic"
            }),
        );
        let open_resp = dispatch(&conn, &open_req, 1);
        let output: serde_json::Value =
            serde_json::from_str(open_resp.output.as_ref().unwrap()).unwrap();
        let concern_id = output["concern_id"].as_str().unwrap();

        let abandon_req = make_request(
            "2",
            "abandon_concern",
            serde_json::json!({
                "concern_id": concern_id,
                "reason": "Z turned out to be irrelevant to our actual problem"
            }),
        );
        let abandon_resp = dispatch(&conn, &abandon_req, 1);
        assert!(abandon_resp.error.is_none());
    }

    #[test]
    fn test_surface_concerns() {
        let conn = test_db();

        // Open several concerns with different salience.
        for (i, sal) in [0.9, 0.3, 0.7, 0.1].iter().enumerate() {
            let req = make_request(
                &format!("{i}"),
                "open_concern",
                serde_json::json!({
                    "description": format!("Concern {i}"),
                    "type": "epistemic",
                    "salience": sal
                }),
            );
            dispatch(&conn, &req, 1);
        }

        let surface_req = make_request("s", "surface_concerns", serde_json::json!({ "n": 2 }));
        let resp = dispatch(&conn, &surface_req, 1);
        assert!(resp.error.is_none());
        let output: serde_json::Value =
            serde_json::from_str(resp.output.as_ref().unwrap()).unwrap();
        assert_eq!(output["count"], 2);
        let concerns = output["concerns"].as_array().unwrap();
        // Should be sorted by salience desc.
        let sal0 = concerns[0]["salience"].as_f64().unwrap();
        let sal1 = concerns[1]["salience"].as_f64().unwrap();
        assert!(sal0 >= sal1);
        assert_eq!(sal0, 0.9);
    }

    #[test]
    fn test_commitment_lifecycle() {
        let conn = test_db();

        // Open a conative concern.
        let open_req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "Deploy the fix",
                "type": "conative",
                "tension": "Users are hitting this bug daily"
            }),
        );
        let open_resp = dispatch(&conn, &open_req, 1);
        let output: serde_json::Value =
            serde_json::from_str(open_resp.output.as_ref().unwrap()).unwrap();
        let concern_id = output["concern_id"].as_str().unwrap();

        // Commit to an action.
        let commit_req = make_request(
            "2",
            "commit_to",
            serde_json::json!({
                "concern_id": concern_id,
                "action": "Run the deploy script and verify the fix in production",
                "scheduled_for": "2h"
            }),
        );
        let commit_resp = dispatch(&conn, &commit_req, 1);
        assert!(
            commit_resp.error.is_none(),
            "unexpected error: {:?}",
            commit_resp.error
        );
        let commit_output: serde_json::Value =
            serde_json::from_str(commit_resp.output.as_ref().unwrap()).unwrap();
        let commitment_id = commit_output["commitment_id"].as_str().unwrap();

        // Report as fulfilled.
        let report_req = make_request(
            "3",
            "report_commitment",
            serde_json::json!({
                "commitment_id": commitment_id,
                "state": "fulfilled",
                "note": "Deployed v2.3.1, verified fix is live via smoke test"
            }),
        );
        let report_resp = dispatch(&conn, &report_req, 1);
        assert!(report_resp.error.is_none());

        // Cannot report again.
        let report2 = dispatch(&conn, &report_req, 1);
        assert!(report2.error.is_some());
    }

    #[test]
    fn test_kb_lifecycle() {
        let conn = test_db();

        // Record a fact.
        let record_req = make_request(
            "1",
            "kb_record",
            serde_json::json!({
                "subject": "deploy pipeline",
                "predicate": "uses",
                "object": "GitHub Actions",
                "confidence": 0.95,
                "source": "observation from repo"
            }),
        );
        let record_resp = dispatch(&conn, &record_req, 1);
        assert!(record_resp.error.is_none());
        let output: serde_json::Value =
            serde_json::from_str(record_resp.output.as_ref().unwrap()).unwrap();
        let record_id = output["record_id"].as_str().unwrap();

        // Retrieve it.
        let retrieve_req = make_request(
            "2",
            "kb_retrieve",
            serde_json::json!({ "subject": "deploy pipeline" }),
        );
        let retrieve_resp = dispatch(&conn, &retrieve_req, 1);
        assert!(retrieve_resp.error.is_none());
        let records: serde_json::Value =
            serde_json::from_str(retrieve_resp.output.as_ref().unwrap()).unwrap();
        assert_eq!(records["count"], 1);

        // Invalidate it.
        let invalidate_req = make_request(
            "3",
            "kb_invalidate",
            serde_json::json!({
                "record_id": record_id,
                "reason": "Migrated to GitLab CI"
            }),
        );
        let invalidate_resp = dispatch(&conn, &invalidate_req, 1);
        assert!(invalidate_resp.error.is_none());

        // Retrieve should now return empty.
        let retrieve2 = dispatch(&conn, &retrieve_req, 1);
        let records2: serde_json::Value =
            serde_json::from_str(retrieve2.output.as_ref().unwrap()).unwrap();
        assert_eq!(records2["count"], 0);
    }

    #[test]
    fn test_commit_to_rejects_non_conative() {
        let conn = test_db();

        let open_req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "What is X?",
                "type": "epistemic"
            }),
        );
        let open_resp = dispatch(&conn, &open_req, 1);
        let output: serde_json::Value =
            serde_json::from_str(open_resp.output.as_ref().unwrap()).unwrap();
        let concern_id = output["concern_id"].as_str().unwrap();

        let commit_req = make_request(
            "2",
            "commit_to",
            serde_json::json!({
                "concern_id": concern_id,
                "action": "do something",
                "scheduled_for": "1h"
            }),
        );
        let commit_resp = dispatch(&conn, &commit_req, 1);
        assert!(commit_resp.error.is_some());
        assert!(commit_resp.error.unwrap().contains("conative"));
    }

    #[test]
    fn test_spawned_from_tracking() {
        let conn = test_db();

        // Open parent concern.
        let parent_req = make_request(
            "1",
            "open_concern",
            serde_json::json!({
                "description": "Parent concern",
                "type": "epistemic"
            }),
        );
        let parent_resp = dispatch(&conn, &parent_req, 1);
        let parent_output: serde_json::Value =
            serde_json::from_str(parent_resp.output.as_ref().unwrap()).unwrap();
        let parent_id = parent_output["concern_id"].as_str().unwrap();

        // Open child concern.
        let child_req = make_request(
            "2",
            "open_concern",
            serde_json::json!({
                "description": "Child concern",
                "type": "epistemic",
                "spawned_from": parent_id
            }),
        );
        let child_resp = dispatch(&conn, &child_req, 1);
        assert!(child_resp.error.is_none());

        // Verify parent's spawned array was updated.
        let spawned: String = conn
            .query_row(
                "SELECT spawned FROM concerns WHERE concern_id = ?1",
                [parent_id],
                |row| row.get(0),
            )
            .unwrap();
        let spawned_arr: Vec<String> = serde_json::from_str(&spawned).unwrap();
        assert_eq!(spawned_arr.len(), 1);
    }

    // --- Escalate tests ---

    #[test]
    fn test_escalate_missing_params() {
        let conn = test_db();
        let req = make_request("1", "escalate", serde_json::json!({}));
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_some());
        assert!(resp.error.unwrap().contains("subject"));
    }

    #[test]
    fn test_escalate_invalid_kind() {
        let conn = test_db();
        let req = make_request(
            "1",
            "escalate",
            serde_json::json!({
                "subject": "test",
                "body": "test body",
                "kind": "invalid"
            }),
        );
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_some());
    }

    #[test]
    fn test_escalate_fyi() {
        let conn = test_db();
        let req = make_request(
            "1",
            "escalate",
            serde_json::json!({
                "subject": "Status update",
                "body": "Everything is going well",
                "kind": "fyi"
            }),
        );
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_none());
        assert!(resp.signal.is_none()); // FYI doesn't trigger notification
        let output: serde_json::Value =
            serde_json::from_str(resp.output.as_ref().unwrap()).unwrap();
        assert!(output.get("escalation_id").is_some());
        assert_eq!(output["kind"], "fyi");
        assert_eq!(output["notify_operator"], false);
    }

    #[test]
    fn test_escalate_blocked_emits_signal() {
        let conn = test_db();
        let req = make_request(
            "1",
            "escalate",
            serde_json::json!({
                "subject": "Can't deploy",
                "body": "Need access to production cluster",
                "kind": "blocked",
                "urgency": "high"
            }),
        );
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_none());
        assert!(resp.signal.is_some()); // Blocked triggers escalation signal
        assert!(resp.signal.unwrap().starts_with("escalation:"));
        let output: serde_json::Value =
            serde_json::from_str(resp.output.as_ref().unwrap()).unwrap();
        assert_eq!(output["notify_operator"], true);
        assert_eq!(output["urgency"], "high");
    }

    #[test]
    fn test_escalate_persists_to_db() {
        let conn = test_db();
        let req = make_request(
            "1",
            "escalate",
            serde_json::json!({
                "subject": "Need input",
                "body": "Which approach?",
                "kind": "decision_required"
            }),
        );
        let resp = dispatch(&conn, &req, 1);
        assert!(resp.error.is_none());

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM escalations",
                [],
                |r: &rusqlite::Row| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}

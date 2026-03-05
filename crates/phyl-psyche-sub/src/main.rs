//! `phyl-psyche-sub` — Subconscious pass binary.
//!
//! Invoked by phyl-run after SOUL.md finalization. Runs salience decay,
//! flags concerns below abandonment threshold, clusters suggested tensions,
//! and writes `briefing.json` for the next session.

use chrono::Utc;
use phyl_core::{
    Briefing, Commitment, CommitmentState, Concern, ConcernState, ConcernType, Escalation,
    PsycheConfig, Urgency,
};
use rusqlite::Connection;
use std::path::Path;
use uuid::Uuid;

fn main() {
    let home = phyl_core::home_dir();
    let db_path = home.join("psyche.db");
    let briefing_path = home.join("briefing.json");
    let config = load_config(&home);

    if let Err(e) = run(&db_path, &briefing_path, &config) {
        eprintln!("phyl-psyche-sub: error: {e}");
        std::process::exit(1);
    }
}

fn load_config(home: &Path) -> PsycheConfig {
    let config_path = home.join("config.toml");
    if let Ok(text) = std::fs::read_to_string(&config_path)
        && let Ok(config) = toml::from_str::<phyl_core::Config>(&text)
    {
        return config.psyche;
    }
    PsycheConfig::default()
}

/// Main entry point — open DB, run subconscious pass, write briefing.
pub fn run(db_path: &Path, briefing_path: &Path, config: &PsycheConfig) -> Result<(), String> {
    let conn = open_db(db_path)?;
    let session_number = increment_session_counter(&conn)?;

    eprintln!("phyl-psyche-sub: session {session_number} — running subconscious pass");

    // 1. Run salience decay on untouched open/committed concerns.
    let decayed = run_decay(&conn, session_number, config)?;
    eprintln!("phyl-psyche-sub: decayed {decayed} concerns");

    // 2. Flag concerns below abandonment threshold.
    let flagged_count = flag_for_abandonment(&conn, config)?;
    eprintln!("phyl-psyche-sub: flagged {flagged_count} concerns for abandonment");

    // 3. Assemble briefing.
    let briefing = assemble_briefing(&conn, session_number, config)?;

    // 4. Write briefing.json.
    let json = serde_json::to_string_pretty(&briefing)
        .map_err(|e| format!("failed to serialize briefing: {e}"))?;
    std::fs::write(briefing_path, &json)
        .map_err(|e| format!("failed to write briefing.json: {e}"))?;

    // 5. Store briefing in DB.
    store_briefing(&conn, session_number, &json)?;

    eprintln!(
        "phyl-psyche-sub: briefing written — {} top concerns, {} pending commitments, {} broken commitments",
        briefing.top_concerns.len(),
        briefing.pending_commitments.len(),
        briefing.broken_commitments.len(),
    );

    Ok(())
}

fn open_db(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("failed to open database: {e}"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .map_err(|e| format!("failed to set WAL mode: {e}"))?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(|e| format!("failed to enable foreign keys: {e}"))?;
    Ok(conn)
}

/// Increment the session counter and return the new session number.
fn increment_session_counter(conn: &Connection) -> Result<u64, String> {
    let session_id =
        std::env::var("PHYLACTERY_SESSION_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO sessions (session_id, began_at) VALUES (?1, ?2)",
        rusqlite::params![session_id, now],
    )
    .map_err(|e| format!("failed to insert session: {e}"))?;

    let session_number: u64 = conn
        .query_row("SELECT last_insert_rowid()", [], |row| row.get(0))
        .map_err(|e| format!("failed to get session number: {e}"))?;

    Ok(session_number)
}

/// Apply exponential decay to all open/committed concerns not touched this session.
///
/// Formula: `new_salience = salience * 0.5^(sessions_since_touch / half_life)`
pub fn run_decay(
    conn: &Connection,
    current_session: u64,
    config: &PsycheConfig,
) -> Result<usize, String> {
    let half_life = config.half_life_sessions as f64;

    let mut stmt = conn
        .prepare(
            "SELECT concern_id, salience, touched_session FROM concerns \
             WHERE state IN ('open', 'committed') AND touched_session < ?1",
        )
        .map_err(|e| format!("failed to prepare decay query: {e}"))?;

    let rows: Vec<(String, f64, u64)> = stmt
        .query_map([current_session], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })
        .map_err(|e| format!("failed to query concerns for decay: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for (concern_id, salience, touched_session) in &rows {
        let sessions_since = current_session.saturating_sub(*touched_session) as f64;
        let new_salience = decay(salience, sessions_since, half_life);

        conn.execute(
            "UPDATE concerns SET salience = ?1 WHERE concern_id = ?2",
            rusqlite::params![new_salience, concern_id],
        )
        .map_err(|e| format!("failed to update salience: {e}"))?;

        count += 1;
    }

    Ok(count)
}

/// Pure decay function for testability.
///
/// `new_salience = salience * 0.5^(sessions_since_touch / half_life)`
pub fn decay(salience: &f64, sessions_since: f64, half_life: f64) -> f64 {
    if half_life <= 0.0 {
        return *salience;
    }
    let factor = 0.5_f64.powf(sessions_since / half_life);
    (salience * factor).max(0.0)
}

/// Mark concerns below the abandonment threshold with a special tag.
///
/// Returns the number of concerns newly flagged. We don't automatically abandon —
/// the agent must explicitly disposition flagged concerns.
fn flag_for_abandonment(conn: &Connection, config: &PsycheConfig) -> Result<usize, String> {
    // Find open/committed concerns below threshold that aren't already tagged.
    let mut stmt = conn
        .prepare(
            "SELECT concern_id, tags FROM concerns \
             WHERE state IN ('open', 'committed') AND salience < ?1",
        )
        .map_err(|e| format!("failed to prepare abandonment query: {e}"))?;

    let rows: Vec<(String, String)> = stmt
        .query_map([config.abandonment_threshold], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("failed to query for abandonment: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for (concern_id, tags_json) in &rows {
        let mut tags: Vec<String> = serde_json::from_str(tags_json).unwrap_or_default();
        if tags.contains(&"flagged_for_abandonment".to_string()) {
            continue; // already flagged
        }
        tags.push("flagged_for_abandonment".to_string());
        let updated = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "UPDATE concerns SET tags = ?1 WHERE concern_id = ?2",
            rusqlite::params![updated, concern_id],
        )
        .map_err(|e| format!("failed to flag concern: {e}"))?;
        count += 1;
    }

    Ok(count)
}

/// Assemble the briefing from current DB state.
fn assemble_briefing(
    conn: &Connection,
    session_number: u64,
    config: &PsycheConfig,
) -> Result<Briefing, String> {
    let now = Utc::now();
    let top_concerns = query_top_concerns(conn, config.briefing_top_n)?;
    let pending_commitments = query_commitments(conn, "pending")?;
    let broken_commitments = query_commitments(conn, "broken")?;
    let flagged = query_flagged_concerns(conn)?;
    let suggested_tensions = compute_suggested_tensions(conn)?;
    let open_escalations = query_open_escalations(conn)?;

    // Compute sessions since last active (how many sessions ago was the last touch).
    let sessions_since = conn
        .query_row(
            "SELECT MIN(?1 - touched_session) FROM concerns \
             WHERE state IN ('open', 'committed') AND touched_session > 0",
            [session_number],
            |row| row.get::<_, Option<u64>>(0),
        )
        .unwrap_or(Some(0))
        .unwrap_or(0);

    Ok(Briefing {
        generated_at: now,
        session_number,
        elapsed_wall_time_seconds: 0, // filled by caller if needed
        sessions_since_last_active: sessions_since,
        top_concerns,
        pending_commitments,
        broken_commitments,
        flagged_for_abandonment: flagged,
        suggested_tensions,
        open_escalations,
    })
}

fn query_top_concerns(conn: &Connection, limit: usize) -> Result<Vec<Concern>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT concern_id, description, type, tension, state, salience, tags, origin, \
             touch_count, created_session, touched_session, created_at, touched_at, \
             resolved_at, abandoned_at, outcome, abandon_reason, spawned_from, spawned \
             FROM concerns WHERE state IN ('open', 'committed') \
             ORDER BY salience DESC LIMIT ?1",
        )
        .map_err(|e| format!("failed to prepare top concerns query: {e}"))?;

    let concerns = stmt
        .query_map([limit], |row| Ok(row_to_concern(row)))
        .map_err(|e| format!("failed to query top concerns: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(concerns)
}

fn query_commitments(conn: &Connection, state: &str) -> Result<Vec<Commitment>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT commitment_id, concern_id, action, scheduled_for, fallback, state, \
             created_at, reported_at, note, spawned_concerns \
             FROM commitments WHERE state = ?1 ORDER BY created_at",
        )
        .map_err(|e| format!("failed to prepare commitments query: {e}"))?;

    let commitments = stmt
        .query_map([state], |row| {
            Ok(Commitment {
                commitment_id: row.get(0)?,
                concern_id: row.get(1)?,
                action: row.get(2)?,
                scheduled_for: parse_dt(&row.get::<_, String>(3)?),
                fallback: row.get(4)?,
                state: match row.get::<_, String>(5)?.as_str() {
                    "fulfilled" => CommitmentState::Fulfilled,
                    "broken" => CommitmentState::Broken,
                    _ => CommitmentState::Pending,
                },
                created_at: parse_dt(&row.get::<_, String>(6)?),
                reported_at: row.get::<_, Option<String>>(7)?.map(|s| parse_dt(&s)),
                note: row.get(8)?,
                spawned_concerns: serde_json::from_str(
                    &row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string()),
                )
                .unwrap_or_default(),
            })
        })
        .map_err(|e| format!("failed to query commitments: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(commitments)
}

fn query_flagged_concerns(conn: &Connection) -> Result<Vec<Concern>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT concern_id, description, type, tension, state, salience, tags, origin, \
             touch_count, created_session, touched_session, created_at, touched_at, \
             resolved_at, abandoned_at, outcome, abandon_reason, spawned_from, spawned \
             FROM concerns WHERE state IN ('open', 'committed') \
             AND tags LIKE '%\"flagged_for_abandonment\"%' \
             ORDER BY salience ASC",
        )
        .map_err(|e| format!("failed to prepare flagged concerns query: {e}"))?;

    let concerns = stmt
        .query_map([], |row| Ok(row_to_concern(row)))
        .map_err(|e| format!("failed to query flagged concerns: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(concerns)
}

fn query_open_escalations(conn: &Connection) -> Result<Vec<Escalation>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT escalation_id, subject, body, urgency, kind, concern_id, commitment_id, \
             blocking_action, proposed_resolution, created_at, responded_at, response \
             FROM escalations WHERE responded_at IS NULL ORDER BY created_at",
        )
        .map_err(|e| format!("failed to prepare escalations query: {e}"))?;

    let escalations = stmt
        .query_map([], |row| {
            use phyl_core::EscalationKind;
            Ok(Escalation {
                escalation_id: row.get(0)?,
                subject: row.get(1)?,
                body: row.get(2)?,
                urgency: match row.get::<_, String>(3)?.as_str() {
                    "low" => Urgency::Low,
                    "high" => Urgency::High,
                    _ => Urgency::Normal,
                },
                kind: match row.get::<_, String>(4)?.as_str() {
                    "blocked" => EscalationKind::Blocked,
                    "decision_required" => EscalationKind::DecisionRequired,
                    "request_capability" => EscalationKind::RequestCapability,
                    _ => EscalationKind::Fyi,
                },
                concern_id: row.get(5)?,
                commitment_id: row.get(6)?,
                blocking_action: row.get(7)?,
                proposed_resolution: row.get(8)?,
                created_at: parse_dt(&row.get::<_, String>(9)?),
                responded_at: row.get::<_, Option<String>>(10)?.map(|s| parse_dt(&s)),
                response: row.get(11)?,
            })
        })
        .map_err(|e| format!("failed to query escalations: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(escalations)
}

/// Compute suggested tensions by finding tag overlaps between open concerns.
///
/// Simple heuristic: for each pair of open concerns sharing a tag, suggest
/// they might be related. Group by shared tag.
fn compute_suggested_tensions(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT concern_id, description, tags FROM concerns \
             WHERE state IN ('open', 'committed') AND tags != '[]' \
             ORDER BY salience DESC LIMIT 20",
        )
        .map_err(|e| format!("failed to prepare tensions query: {e}"))?;

    let rows: Vec<(String, String, Vec<String>)> = stmt
        .query_map([], |row| {
            let tags_str: String = row.get(2)?;
            let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, tags))
        })
        .map_err(|e| format!("failed to query for tensions: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    let mut tensions: Vec<String> = Vec::new();
    let mut seen_pairs = std::collections::HashSet::new();

    for i in 0..rows.len() {
        for j in (i + 1)..rows.len() {
            let shared: Vec<&String> = rows[i].2.iter().filter(|t| rows[j].2.contains(t)).collect();
            if !shared.is_empty() {
                let pair_key = if rows[i].0 < rows[j].0 {
                    format!("{}:{}", rows[i].0, rows[j].0)
                } else {
                    format!("{}:{}", rows[j].0, rows[i].0)
                };
                if seen_pairs.insert(pair_key) {
                    let shared_tags: Vec<&str> = shared.iter().map(|s| s.as_str()).collect();
                    tensions.push(format!(
                        "Concerns '{}' and '{}' share tags [{}] — might be related",
                        truncate(&rows[i].1, 50),
                        truncate(&rows[j].1, 50),
                        shared_tags.join(", "),
                    ));
                }
            }
        }
    }

    Ok(tensions)
}

fn store_briefing(conn: &Connection, session_number: u64, json: &str) -> Result<(), String> {
    let briefing_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO briefings (briefing_id, session_number, generated_at, content) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![briefing_id, session_number, now, json],
    )
    .map_err(|e| format!("failed to store briefing: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn row_to_concern(row: &rusqlite::Row) -> Concern {
    Concern {
        concern_id: row.get(0).unwrap_or_default(),
        description: row.get(1).unwrap_or_default(),
        concern_type: match row.get::<_, String>(2).unwrap_or_default().as_str() {
            "appetitive" => ConcernType::Appetitive,
            "conative" => ConcernType::Conative,
            _ => ConcernType::Epistemic,
        },
        tension: row.get(3).unwrap_or(None),
        state: match row.get::<_, String>(4).unwrap_or_default().as_str() {
            "committed" => ConcernState::Committed,
            "resolved" => ConcernState::Resolved,
            "abandoned" => ConcernState::Abandoned,
            _ => ConcernState::Open,
        },
        salience: row.get(5).unwrap_or(0.5),
        tags: serde_json::from_str(&row.get::<_, String>(6).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_default(),
        origin: row.get(7).unwrap_or_else(|_| "session".to_string()),
        touch_count: row.get(8).unwrap_or(0),
        created_session: row.get(9).unwrap_or(0),
        touched_session: row.get(10).unwrap_or(0),
        created_at: parse_dt(&row.get::<_, String>(11).unwrap_or_default()),
        touched_at: parse_dt(&row.get::<_, String>(12).unwrap_or_default()),
        resolved_at: row
            .get::<_, Option<String>>(13)
            .ok()
            .flatten()
            .map(|s| parse_dt(&s)),
        abandoned_at: row
            .get::<_, Option<String>>(14)
            .ok()
            .flatten()
            .map(|s| parse_dt(&s)),
        outcome: row.get(15).unwrap_or(None),
        abandon_reason: row.get(16).unwrap_or(None),
        spawned_from: row.get(17).unwrap_or(None),
        spawned: serde_json::from_str(
            &row.get::<_, String>(18)
                .unwrap_or_else(|_| "[]".to_string()),
        )
        .unwrap_or_default(),
    }
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
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

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // Re-use the schema from phyl-tool-psyche's db module.
        conn.execute_batch(include_str!("../../phyl-tool-psyche/src/schema.sql"))
            .unwrap();
        conn
    }

    fn insert_concern(
        conn: &Connection,
        id: &str,
        desc: &str,
        salience: f64,
        touched_session: u64,
        tags: &[&str],
    ) {
        let now = Utc::now().to_rfc3339();
        let tags_json =
            serde_json::to_string(&tags.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        conn.execute(
            "INSERT INTO concerns (concern_id, description, type, state, salience, tags, \
             origin, touch_count, created_session, touched_session, created_at, touched_at, spawned) \
             VALUES (?1, ?2, 'epistemic', 'open', ?3, ?4, 'session', 0, 1, ?5, ?6, ?6, '[]')",
            rusqlite::params![id, desc, salience, tags_json, touched_session, now],
        )
        .unwrap();
    }

    // --- Decay math tests ---

    #[test]
    fn test_decay_no_time_passed() {
        let result = decay(&0.8, 0.0, 10.0);
        assert!((result - 0.8).abs() < 1e-10);
    }

    #[test]
    fn test_decay_one_half_life() {
        let result = decay(&1.0, 10.0, 10.0);
        assert!((result - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_decay_two_half_lives() {
        let result = decay(&1.0, 20.0, 10.0);
        assert!((result - 0.25).abs() < 1e-10);
    }

    #[test]
    fn test_decay_partial_half_life() {
        let result = decay(&1.0, 5.0, 10.0);
        // 0.5^0.5 ≈ 0.7071
        assert!((result - 0.5_f64.sqrt()).abs() < 1e-10);
    }

    #[test]
    fn test_decay_zero_half_life_returns_unchanged() {
        let result = decay(&0.8, 5.0, 0.0);
        assert!((result - 0.8).abs() < 1e-10);
    }

    #[test]
    fn test_decay_never_negative() {
        let result = decay(&0.01, 100.0, 10.0);
        assert!(result >= 0.0);
    }

    // --- DB decay tests ---

    #[test]
    fn test_run_decay_skips_touched_concerns() {
        let conn = setup_db();
        // Concern touched this session (session 5) should not decay.
        insert_concern(&conn, "c1", "fresh", 0.8, 5, &[]);
        // Concern touched last session should decay.
        insert_concern(&conn, "c2", "stale", 0.8, 3, &[]);

        let config = PsycheConfig::default();
        let decayed = run_decay(&conn, 5, &config).unwrap();
        assert_eq!(decayed, 1); // only c2

        let fresh: f64 = conn
            .query_row(
                "SELECT salience FROM concerns WHERE concern_id = 'c1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((fresh - 0.8).abs() < 1e-10);

        let stale: f64 = conn
            .query_row(
                "SELECT salience FROM concerns WHERE concern_id = 'c2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // 2 sessions ago, half_life=10: 0.8 * 0.5^(2/10) ≈ 0.695
        assert!(stale < 0.8);
        assert!(stale > 0.6);
    }

    // --- Flag for abandonment tests ---

    #[test]
    fn test_flag_for_abandonment() {
        let conn = setup_db();
        insert_concern(&conn, "c1", "healthy", 0.5, 1, &[]);
        insert_concern(&conn, "c2", "dying", 0.01, 1, &[]);

        let config = PsycheConfig::default(); // threshold 0.05
        let flagged = flag_for_abandonment(&conn, &config).unwrap();
        assert_eq!(flagged, 1);

        let tags: String = conn
            .query_row(
                "SELECT tags FROM concerns WHERE concern_id = 'c2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tags.contains("flagged_for_abandonment"));
    }

    #[test]
    fn test_flag_idempotent() {
        let conn = setup_db();
        insert_concern(&conn, "c1", "dying", 0.01, 1, &["flagged_for_abandonment"]);

        let config = PsycheConfig::default();
        let flagged = flag_for_abandonment(&conn, &config).unwrap();
        assert_eq!(flagged, 0); // already flagged
    }

    // --- Suggested tensions tests ---

    #[test]
    fn test_suggested_tensions_shared_tags() {
        let conn = setup_db();
        insert_concern(&conn, "c1", "Learn about X", 0.8, 1, &["infrastructure"]);
        insert_concern(&conn, "c2", "Fix Y deployment", 0.7, 1, &["infrastructure"]);
        insert_concern(&conn, "c3", "Read about Z", 0.6, 1, &["reading"]);

        let tensions = compute_suggested_tensions(&conn).unwrap();
        assert_eq!(tensions.len(), 1);
        assert!(tensions[0].contains("infrastructure"));
    }

    // --- Briefing assembly tests ---

    #[test]
    fn test_assemble_briefing_empty_db() {
        let conn = setup_db();
        let config = PsycheConfig::default();
        let briefing = assemble_briefing(&conn, 1, &config).unwrap();
        assert_eq!(briefing.session_number, 1);
        assert!(briefing.top_concerns.is_empty());
        assert!(briefing.pending_commitments.is_empty());
        assert!(briefing.broken_commitments.is_empty());
    }

    #[test]
    fn test_assemble_briefing_with_concerns() {
        let conn = setup_db();
        insert_concern(&conn, "c1", "High priority", 0.9, 1, &[]);
        insert_concern(&conn, "c2", "Low priority", 0.2, 1, &[]);
        insert_concern(&conn, "c3", "Medium priority", 0.5, 1, &[]);

        let config = PsycheConfig {
            briefing_top_n: 2,
            ..PsycheConfig::default()
        };
        let briefing = assemble_briefing(&conn, 1, &config).unwrap();
        assert_eq!(briefing.top_concerns.len(), 2);
        // Highest salience first.
        assert!(briefing.top_concerns[0].salience >= briefing.top_concerns[1].salience);
    }

    #[test]
    fn test_briefing_serialization_roundtrip() {
        let conn = setup_db();
        insert_concern(&conn, "c1", "Test concern", 0.8, 1, &["tag1"]);

        let config = PsycheConfig::default();
        let briefing = assemble_briefing(&conn, 1, &config).unwrap();

        let json = serde_json::to_string_pretty(&briefing).unwrap();
        let parsed: Briefing = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_number, 1);
        assert_eq!(parsed.top_concerns.len(), 1);
        assert_eq!(parsed.top_concerns[0].concern_id, "c1");
    }

    #[test]
    fn test_full_subconscious_pass() {
        let conn = setup_db();

        // Set up some state.
        insert_concern(&conn, "c1", "Active concern", 0.9, 1, &["infra"]);
        insert_concern(&conn, "c2", "Stale concern", 0.3, 1, &["infra"]);
        insert_concern(&conn, "c3", "Nearly dead", 0.04, 1, &[]);

        // Insert a session so increment works.
        conn.execute(
            "INSERT INTO sessions (session_id, began_at) VALUES ('test', ?1)",
            [Utc::now().to_rfc3339()],
        )
        .unwrap();

        let config = PsycheConfig::default();
        let session = 5_u64;

        // Run decay.
        let decayed = run_decay(&conn, session, &config).unwrap();
        assert_eq!(decayed, 3);

        // Flag for abandonment.
        let flagged = flag_for_abandonment(&conn, &config).unwrap();
        // c3 was already at 0.04, after decay it's even lower — should be flagged.
        assert!(flagged >= 1);

        // Assemble briefing.
        let briefing = assemble_briefing(&conn, session, &config).unwrap();
        assert!(!briefing.top_concerns.is_empty());
        assert!(!briefing.flagged_for_abandonment.is_empty());
    }
}

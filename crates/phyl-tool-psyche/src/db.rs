//! Database schema and helpers for the psyche structured store.

use rusqlite::{Connection, Result};
use std::path::Path;

/// SQL statements to create the psyche schema.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS concerns (
    concern_id       TEXT PRIMARY KEY,
    description      TEXT NOT NULL,
    type             TEXT NOT NULL CHECK(type IN ('epistemic','appetitive','conative')),
    tension          TEXT,
    state            TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','committed','resolved','abandoned')),
    salience         REAL NOT NULL DEFAULT 0.5,
    tags             TEXT NOT NULL DEFAULT '[]',
    origin           TEXT NOT NULL DEFAULT 'session',
    touch_count      INTEGER NOT NULL DEFAULT 0,
    created_session  INTEGER NOT NULL,
    touched_session  INTEGER NOT NULL,
    created_at       TEXT NOT NULL,
    touched_at       TEXT NOT NULL,
    resolved_at      TEXT,
    abandoned_at     TEXT,
    outcome          TEXT,
    abandon_reason   TEXT,
    spawned_from     TEXT REFERENCES concerns(concern_id),
    spawned          TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS commitments (
    commitment_id    TEXT PRIMARY KEY,
    concern_id       TEXT NOT NULL REFERENCES concerns(concern_id),
    action           TEXT NOT NULL,
    scheduled_for    TEXT NOT NULL,
    fallback         TEXT,
    state            TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','fulfilled','broken')),
    created_at       TEXT NOT NULL,
    reported_at      TEXT,
    note             TEXT,
    spawned_concerns TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS escalations (
    escalation_id    TEXT PRIMARY KEY,
    subject          TEXT NOT NULL,
    body             TEXT NOT NULL,
    urgency          TEXT NOT NULL DEFAULT 'normal',
    kind             TEXT NOT NULL,
    concern_id       TEXT REFERENCES concerns(concern_id),
    commitment_id    TEXT REFERENCES commitments(commitment_id),
    blocking_action  TEXT,
    proposed_resolution TEXT,
    created_at       TEXT NOT NULL,
    responded_at     TEXT,
    response         TEXT
);

CREATE TABLE IF NOT EXISTS kb_records (
    record_id        TEXT PRIMARY KEY,
    subject          TEXT NOT NULL,
    predicate        TEXT NOT NULL,
    object           TEXT NOT NULL,
    confidence       REAL NOT NULL,
    source           TEXT NOT NULL,
    concern_id       TEXT REFERENCES concerns(concern_id),
    created_at       TEXT NOT NULL,
    expires_at       TEXT,
    invalidated_at   TEXT,
    invalidation_reason TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
    session_number   INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT NOT NULL,
    began_at         TEXT NOT NULL,
    closed_at        TEXT
);

CREATE TABLE IF NOT EXISTS briefings (
    briefing_id      TEXT PRIMARY KEY,
    session_number   INTEGER NOT NULL,
    generated_at     TEXT NOT NULL,
    content          TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_concerns_state ON concerns(state);
CREATE INDEX IF NOT EXISTS idx_concerns_salience ON concerns(salience DESC);
CREATE INDEX IF NOT EXISTS idx_commitments_state ON commitments(state);
CREATE INDEX IF NOT EXISTS idx_kb_records_subject ON kb_records(subject);
CREATE INDEX IF NOT EXISTS idx_escalations_responded ON escalations(responded_at);
"#;

/// Open (or create) the psyche database and run migrations.
pub fn open_and_migrate(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Apply the schema to an existing connection (used by tests with in-memory DBs).
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// Append a value to a JSON array column.
///
/// Reads the current JSON array from `column` where `pk_column = pk_value`,
/// appends `new_value`, and writes it back.
pub fn append_to_json_array(
    conn: &Connection,
    table: &str,
    column: &str,
    pk_column: &str,
    pk_value: &str,
    new_value: &str,
) -> Result<()> {
    let current: String = conn.query_row(
        &format!("SELECT {column} FROM {table} WHERE {pk_column} = ?1"),
        [pk_value],
        |row| row.get(0),
    )?;

    let mut arr: Vec<String> = serde_json::from_str(&current).unwrap_or_default();
    arr.push(new_value.to_string());
    let updated = serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string());

    conn.execute(
        &format!("UPDATE {table} SET {column} = ?1 WHERE {pk_column} = ?2"),
        rusqlite::params![updated, pk_value],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrate_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();

        // Verify tables exist by querying them.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM concerns", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM commitments", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM kb_records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_migrate_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // should not fail
    }

    #[test]
    fn test_append_to_json_array() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();

        // Insert a concern to test with.
        conn.execute(
            "INSERT INTO concerns (concern_id, description, type, state, salience, tags, \
             origin, touch_count, created_session, touched_session, created_at, touched_at, spawned) \
             VALUES ('c1', 'test', 'epistemic', 'open', 0.5, '[]', 'session', 0, 1, 1, \
             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '[]')",
            [],
        )
        .unwrap();

        append_to_json_array(&conn, "concerns", "spawned", "concern_id", "c1", "c2").unwrap();

        let spawned: String = conn
            .query_row(
                "SELECT spawned FROM concerns WHERE concern_id = 'c1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let arr: Vec<String> = serde_json::from_str(&spawned).unwrap();
        assert_eq!(arr, vec!["c2"]);

        // Append another.
        append_to_json_array(&conn, "concerns", "spawned", "concern_id", "c1", "c3").unwrap();
        let spawned: String = conn
            .query_row(
                "SELECT spawned FROM concerns WHERE concern_id = 'c1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let arr: Vec<String> = serde_json::from_str(&spawned).unwrap();
        assert_eq!(arr, vec!["c2", "c3"]);
    }
}

//! Database schema and helpers for the psyche structured store.

use rusqlite::{Connection, Result};
use std::path::Path;

/// SQL statements to create the psyche schema.
const SCHEMA: &str = include_str!("schema.sql");

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
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// Append a value to a JSON array column.
///
/// Reads the current JSON array from `column` where `pk_column = pk_value`,
/// appends `new_value`, and writes it back.
///
/// SAFETY: `table`, `column`, and `pk_column` are interpolated into SQL.
/// Only call with hardcoded identifiers — never with user input.
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

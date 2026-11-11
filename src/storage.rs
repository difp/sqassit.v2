//! SQLite initialization compatible with the Python application's schema.
//!
//! Each opened connection receives the same pragmas and search function. The
//! database is migrated in a transaction; this is not yet a production server.
use rusqlite::{functions::FunctionFlags, Connection, Result};
use std::path::Path;
use std::time::Duration;
use unicode_casefold::UnicodeCaseFold;

const SCHEMA: &str = include_str!("schema.sql");

pub fn search_norm(value: &str) -> String {
    value.case_fold().collect::<String>().replace('ё', "е")
}

pub fn open(path: impl AsRef<Path>) -> Result<Connection> {
    let mut connection = Connection::open(path)?;
    initialize(&mut connection)?;
    Ok(connection)
}

pub fn initialize(connection: &mut Connection) -> Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.create_scalar_function(
        "search_norm",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(search_norm(
                ctx.get::<Option<String>>(0)?.as_deref().unwrap_or(""),
            ))
        },
    )?;
    let tx = connection.transaction()?;
    tx.execute_batch(SCHEMA)?;
    let onboarding_present: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('users') WHERE name = 'onboarding_seen')",
        [],
        |row| row.get(0),
    )?;
    // Static identifiers and definitions only; never derived from user input.
    const ADDITIONS: &[(&str, &str, &str)] = &[
        ("trainings", "deleted_at", "INTEGER"),
        ("trainings", "deleted_by", "INTEGER"),
        ("trainings", "level", "TEXT NOT NULL DEFAULT ''"),
        ("trainings", "room_code", "TEXT"),
        ("matches", "room_code", "TEXT"),
        ("users", "bio", "TEXT NOT NULL DEFAULT ''"),
        ("users", "links", "TEXT NOT NULL DEFAULT '[]'"),
        ("users", "about", "TEXT NOT NULL DEFAULT ''"),
        ("users", "display_mode", "TEXT NOT NULL DEFAULT 'name'"),
        ("users", "show_username", "INTEGER NOT NULL DEFAULT 0"),
        ("users", "onboarding_seen", "INTEGER NOT NULL DEFAULT 0"),
        ("users", "notify_matches", "INTEGER NOT NULL DEFAULT 1"),
        ("users", "notify_trainings", "INTEGER NOT NULL DEFAULT 1"),
        ("users", "notify_messages", "INTEGER NOT NULL DEFAULT 1"),
        ("users", "phone", "TEXT NOT NULL DEFAULT ''"),
        ("coaches", "is_manager", "INTEGER NOT NULL DEFAULT 0"),
        ("match_players", "status", "TEXT NOT NULL DEFAULT 'booked'"),
        ("match_players", "decided_by", "INTEGER"),
        ("match_players", "decided_at", "INTEGER"),
        ("event_notifications", "delete_at", "INTEGER"),
        ("event_notifications", "phase", "TEXT NOT NULL DEFAULT ''"),
        (
            "event_notifications",
            "sent_at",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        ("event_notifications", "final_at", "INTEGER"),
    ];
    for (table, column, definition) in ADDITIONS {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
            [table, column],
            |row| row.get(0),
        )?;
        if !exists {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    tx.execute_batch("CREATE INDEX IF NOT EXISTS idx_event_notifications_delete ON event_notifications(delete_at);
        UPDATE event_notifications SET delete_at = updated_at + 86400 WHERE delete_at IS NULL AND kind IN ('training', 'match', 'match_feed');")?;
    if !onboarding_present {
        tx.execute("UPDATE users SET onboarding_seen = 1", [])?;
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlite_schema_and_search_match_legacy_contract() {
        let mut db = Connection::open_in_memory().unwrap();
        initialize(&mut db).unwrap();
        let result: String = db
            .query_row("SELECT search_norm(?1)", ["ЁЖ Straße"], |r| r.get(0))
            .unwrap();
        assert_eq!(result, "еж strasse");
        let result: String = db
            .query_row("SELECT search_norm(NULL)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(result, "");
        let foreign_keys: i64 = db
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        assert!(db
            .prepare("SELECT onboarding_seen, phone FROM users")
            .is_ok());
        initialize(&mut db).unwrap();
    }
    #[test]
    fn existing_users_do_not_see_new_onboarding() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE users (telegram_id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL); INSERT INTO users VALUES (42, 0);").unwrap();
        initialize(&mut db).unwrap();
        let seen: i64 = db
            .query_row(
                "SELECT onboarding_seen FROM users WHERE telegram_id = 42",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seen, 1);
        db.execute(
            "UPDATE users SET onboarding_seen=0 WHERE telegram_id=42",
            [],
        )
        .unwrap();
        initialize(&mut db).unwrap();
        let seen: i64 = db
            .query_row(
                "SELECT onboarding_seen FROM users WHERE telegram_id = 42",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seen, 0);
    }
    #[test]
    fn migrates_old_notification_records_idempotently() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db.execute_batch("INSERT INTO event_notifications (kind, entity_id, chat_id, updated_at) VALUES ('training', 9, 123, 100); ALTER TABLE event_notifications DROP COLUMN delete_at; ALTER TABLE event_notifications DROP COLUMN phase; ALTER TABLE event_notifications DROP COLUMN sent_at; ALTER TABLE event_notifications DROP COLUMN final_at;").unwrap();
        initialize(&mut db).unwrap();
        let expiry: i64 = db
            .query_row("SELECT delete_at FROM event_notifications", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(expiry, 86500);
        initialize(&mut db).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM event_notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}

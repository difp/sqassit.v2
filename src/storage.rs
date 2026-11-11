//! SQLite initialization compatible with the Python application's schema.
//!
//! Each opened connection receives the same pragmas and search function. The
//! database is migrated in a transaction; this is not yet a production server.
use rusqlite::{functions::FunctionFlags, Connection, Result};
use std::path::Path;
use std::time::Duration;
use unicode_casefold::UnicodeCaseFold;

const SCHEMA: &str = include_str!("schema.sql");

/// Case-fold mappings added after Unicode 9.0, the last version bundled with
/// `unicode-casefold 0.2`. The Python reference runs Unicode 15.0, so these
/// newer characters are folded here to keep search behavior identical.
const LATE_FOLDS: &[(char, &str)] = &[
    ('\u{1c90}', "\u{10d0}"),
    ('\u{1c91}', "\u{10d1}"),
    ('\u{1c92}', "\u{10d2}"),
    ('\u{1c93}', "\u{10d3}"),
    ('\u{1c94}', "\u{10d4}"),
    ('\u{1c95}', "\u{10d5}"),
    ('\u{1c96}', "\u{10d6}"),
    ('\u{1c97}', "\u{10d7}"),
    ('\u{1c98}', "\u{10d8}"),
    ('\u{1c99}', "\u{10d9}"),
    ('\u{1c9a}', "\u{10da}"),
    ('\u{1c9b}', "\u{10db}"),
    ('\u{1c9c}', "\u{10dc}"),
    ('\u{1c9d}', "\u{10dd}"),
    ('\u{1c9e}', "\u{10de}"),
    ('\u{1c9f}', "\u{10df}"),
    ('\u{1ca0}', "\u{10e0}"),
    ('\u{1ca1}', "\u{10e1}"),
    ('\u{1ca2}', "\u{10e2}"),
    ('\u{1ca3}', "\u{10e3}"),
    ('\u{1ca4}', "\u{10e4}"),
    ('\u{1ca5}', "\u{10e5}"),
    ('\u{1ca6}', "\u{10e6}"),
    ('\u{1ca7}', "\u{10e7}"),
    ('\u{1ca8}', "\u{10e8}"),
    ('\u{1ca9}', "\u{10e9}"),
    ('\u{1caa}', "\u{10ea}"),
    ('\u{1cab}', "\u{10eb}"),
    ('\u{1cac}', "\u{10ec}"),
    ('\u{1cad}', "\u{10ed}"),
    ('\u{1cae}', "\u{10ee}"),
    ('\u{1caf}', "\u{10ef}"),
    ('\u{1cb0}', "\u{10f0}"),
    ('\u{1cb1}', "\u{10f1}"),
    ('\u{1cb2}', "\u{10f2}"),
    ('\u{1cb3}', "\u{10f3}"),
    ('\u{1cb4}', "\u{10f4}"),
    ('\u{1cb5}', "\u{10f5}"),
    ('\u{1cb6}', "\u{10f6}"),
    ('\u{1cb7}', "\u{10f7}"),
    ('\u{1cb8}', "\u{10f8}"),
    ('\u{1cb9}', "\u{10f9}"),
    ('\u{1cba}', "\u{10fa}"),
    ('\u{1cbd}', "\u{10fd}"),
    ('\u{1cbe}', "\u{10fe}"),
    ('\u{1cbf}', "\u{10ff}"),
    ('\u{2c2f}', "\u{2c5f}"),
    ('\u{a7b8}', "\u{a7b9}"),
    ('\u{a7ba}', "\u{a7bb}"),
    ('\u{a7bc}', "\u{a7bd}"),
    ('\u{a7be}', "\u{a7bf}"),
    ('\u{a7c0}', "\u{a7c1}"),
    ('\u{a7c2}', "\u{a7c3}"),
    ('\u{a7c4}', "\u{a794}"),
    ('\u{a7c5}', "\u{282}"),
    ('\u{a7c6}', "\u{1d8e}"),
    ('\u{a7c7}', "\u{a7c8}"),
    ('\u{a7c9}', "\u{a7ca}"),
    ('\u{a7d0}', "\u{a7d1}"),
    ('\u{a7d6}', "\u{a7d7}"),
    ('\u{a7d8}', "\u{a7d9}"),
    ('\u{a7f5}', "\u{a7f6}"),
    ('\u{10570}', "\u{10597}"),
    ('\u{10571}', "\u{10598}"),
    ('\u{10572}', "\u{10599}"),
    ('\u{10573}', "\u{1059a}"),
    ('\u{10574}', "\u{1059b}"),
    ('\u{10575}', "\u{1059c}"),
    ('\u{10576}', "\u{1059d}"),
    ('\u{10577}', "\u{1059e}"),
    ('\u{10578}', "\u{1059f}"),
    ('\u{10579}', "\u{105a0}"),
    ('\u{1057a}', "\u{105a1}"),
    ('\u{1057c}', "\u{105a3}"),
    ('\u{1057d}', "\u{105a4}"),
    ('\u{1057e}', "\u{105a5}"),
    ('\u{1057f}', "\u{105a6}"),
    ('\u{10580}', "\u{105a7}"),
    ('\u{10581}', "\u{105a8}"),
    ('\u{10582}', "\u{105a9}"),
    ('\u{10583}', "\u{105aa}"),
    ('\u{10584}', "\u{105ab}"),
    ('\u{10585}', "\u{105ac}"),
    ('\u{10586}', "\u{105ad}"),
    ('\u{10587}', "\u{105ae}"),
    ('\u{10588}', "\u{105af}"),
    ('\u{10589}', "\u{105b0}"),
    ('\u{1058a}', "\u{105b1}"),
    ('\u{1058c}', "\u{105b3}"),
    ('\u{1058d}', "\u{105b4}"),
    ('\u{1058e}', "\u{105b5}"),
    ('\u{1058f}', "\u{105b6}"),
    ('\u{10590}', "\u{105b7}"),
    ('\u{10591}', "\u{105b8}"),
    ('\u{10592}', "\u{105b9}"),
    ('\u{10594}', "\u{105bb}"),
    ('\u{10595}', "\u{105bc}"),
    ('\u{16e40}', "\u{16e60}"),
    ('\u{16e41}', "\u{16e61}"),
    ('\u{16e42}', "\u{16e62}"),
    ('\u{16e43}', "\u{16e63}"),
    ('\u{16e44}', "\u{16e64}"),
    ('\u{16e45}', "\u{16e65}"),
    ('\u{16e46}', "\u{16e66}"),
    ('\u{16e47}', "\u{16e67}"),
    ('\u{16e48}', "\u{16e68}"),
    ('\u{16e49}', "\u{16e69}"),
    ('\u{16e4a}', "\u{16e6a}"),
    ('\u{16e4b}', "\u{16e6b}"),
    ('\u{16e4c}', "\u{16e6c}"),
    ('\u{16e4d}', "\u{16e6d}"),
    ('\u{16e4e}', "\u{16e6e}"),
    ('\u{16e4f}', "\u{16e6f}"),
    ('\u{16e50}', "\u{16e70}"),
    ('\u{16e51}', "\u{16e71}"),
    ('\u{16e52}', "\u{16e72}"),
    ('\u{16e53}', "\u{16e73}"),
    ('\u{16e54}', "\u{16e74}"),
    ('\u{16e55}', "\u{16e75}"),
    ('\u{16e56}', "\u{16e76}"),
    ('\u{16e57}', "\u{16e77}"),
    ('\u{16e58}', "\u{16e78}"),
    ('\u{16e59}', "\u{16e79}"),
    ('\u{16e5a}', "\u{16e7a}"),
    ('\u{16e5b}', "\u{16e7b}"),
    ('\u{16e5c}', "\u{16e7c}"),
    ('\u{16e5d}', "\u{16e7d}"),
    ('\u{16e5e}', "\u{16e7e}"),
    ('\u{16e5f}', "\u{16e7f}"),
];

fn late_fold(value: char) -> Option<&'static str> {
    LATE_FOLDS
        .binary_search_by_key(&value, |&(key, _)| key)
        .ok()
        .map(|index| LATE_FOLDS[index].1)
}

pub fn search_norm(value: &str) -> String {
    let folded = value.case_fold().collect::<String>().replace('ё', "е");
    // Fast path: almost all club data uses characters covered by the bundled tables.
    if folded.chars().all(|ch| ch < '\u{1c90}') {
        return folded;
    }
    let mut result = String::with_capacity(folded.len());
    for ch in folded.chars() {
        match late_fold(ch) {
            Some(replacement) => result.push_str(replacement),
            None => result.push(ch),
        }
    }
    result
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
    fn search_folds_characters_newer_than_the_bundled_tables() {
        assert!(
            LATE_FOLDS.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "the fallback table must stay sorted for binary search"
        );
        assert_eq!(search_norm("\u{1c90}\u{1cbf}"), "\u{10d0}\u{10ff}");
        assert_eq!(search_norm("\u{10570}"), "\u{10597}");
        assert_eq!(search_norm("\u{16e40}"), "\u{16e60}");
        assert_eq!(search_norm("\u{a7c4}"), "\u{a794}");
    }
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

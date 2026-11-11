//! Coach-facing player search and private notes.
use crate::{
    auth, coach, identity,
    profile::{ProfileError, Profiles},
    storage::search_norm,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct NotesInput {
    notes: String,
}
pub fn list(p: &Profiles, init: &str, now: i64, query: &str) -> Result<Value, ProfileError> {
    if query.chars().count() > 100 {
        return Err(ProfileError::Invalid("Слишком длинный поиск."));
    }
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    coach::coach(p, &db, init, now)?;
    let mut stmt=db.prepare("SELECT u.telegram_id,u.first_name,u.username,u.show_username,u.avatar_file_id,u.about,u.display_mode,u.notes,u.notifications_enabled FROM users u WHERE NOT EXISTS(SELECT 1 FROM coaches c WHERE c.telegram_id=u.telegram_id) ORDER BY u.first_name COLLATE NOCASE").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, bool>(8)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let query = query.trim();
    let needle = search_norm(query);
    let handle = search_norm(query.trim_start_matches('@'));
    let mut total = 0;
    let mut players = Vec::new();
    for row in rows {
        let (id, name, username, visible, file, about, mode, notes, enabled) =
            row.map_err(|_| ProfileError::Storage)?;
        total += 1;
        let anonymous = mode == "animal";
        let (emoji, animal) = identity::animal_identity(id);
        let matches = query.is_empty()
            || (!anonymous && search_norm(name.as_deref().unwrap_or("")).contains(&needle))
            || (anonymous && search_norm(animal).contains(&needle))
            || (!handle.is_empty()
                && search_norm(username.as_deref().unwrap_or("")).contains(&handle))
            || search_norm(&notes).contains(&needle);
        if !matches {
            continue;
        }
        let display = if anonymous {
            animal.to_owned()
        } else {
            name.filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Игрок".into())
        };
        let avatar = if anonymous {
            String::new()
        } else {
            p.avatar_url(id, file.as_deref(), false, now)
        };
        players.push(json!({"telegram_id":id,"first_name":display,"avatar_url":avatar,"avatar_emoji":if anonymous{emoji}else{""},"anonymous":anonymous,"profile_token":auth::issue_scoped_token(&p.bot_token,"profile",0,id,now,86400),"username":username.filter(|s|!s.is_empty()),"username_visible":visible,"about":about,"display_mode":mode,"notes":notes,"notifications_enabled":enabled}));
    }
    Ok(json!({"ok":true,"total":total,"matched":players.len(),"query":query,"players":players}))
}
pub fn count(p: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    coach::coach(p, &db, init, now)?;
    let total:i64=db.query_row("SELECT count(*) FROM users u WHERE NOT EXISTS(SELECT 1 FROM coaches c WHERE c.telegram_id=u.telegram_id)",[],|r|r.get(0)).map_err(|_|ProfileError::Storage)?;
    Ok(json!({"ok":true,"total":total}))
}
pub fn notes(
    p: &Profiles,
    init: &str,
    now: i64,
    id: i64,
    value: NotesInput,
) -> Result<Value, ProfileError> {
    if value.notes.chars().count() > 4000 {
        return Err(ProfileError::Invalid("Слишком длинная заметка."));
    }
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    coach::coach(p, &db, init, now)?;
    let changed = db
        .execute(
            "UPDATE users SET notes=?1 WHERE telegram_id=?2",
            params![value.notes, id],
        )
        .map_err(|_| ProfileError::Storage)?;
    if changed == 0 {
        return Err(ProfileError::Rule(404, "Игрок не найден"));
    }
    Ok(json!({"ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    const INIT:&str="auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    #[test]
    fn search_respects_anonymity_literal_characters_and_coach_privileges() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO coaches(telegram_id,created_at) VALUES(42,0),(101,0); INSERT INTO users(telegram_id,first_name,username,display_mode,notes,created_at) VALUES(99,'Пётр','petr','animal','',0),(100,'Пётр','other','name','Травма колена',0),(101,'Тренер','coach','name','',0);").unwrap();
        let mut p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "123456:TEST-TOKEN".into(),
            owner: 42,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let all = list(&p, INIT, 1_000_000, "").unwrap();
        assert_eq!(all["total"], 2);
        assert_eq!(count(&p, INIT, 1_000_000).unwrap()["total"], 2);
        let search = list(&p, INIT, 1_000_000, "пЁтр").unwrap();
        assert_eq!(search["matched"], 1);
        assert_eq!(search["players"][0]["telegram_id"], 100);
        let animal = identity::animal_identity(99).1;
        let search = list(&p, INIT, 1_000_000, animal).unwrap();
        assert_eq!(search["players"][0]["telegram_id"], 99);
        assert_eq!(search["players"][0]["anonymous"], true);
        assert_ne!(search["players"][0]["first_name"], "Пётр");
        assert_eq!(
            list(&p, INIT, 1_000_000, "@PETR").unwrap()["players"][0]["telegram_id"],
            99
        );
        assert_eq!(
            list(&p, INIT, 1_000_000, "колена").unwrap()["players"][0]["telegram_id"],
            100
        );
        for q in ["%", "_", "@"] {
            assert_eq!(list(&p, INIT, 1_000_000, q).unwrap()["matched"], 0);
        }
        notes(
            &p,
            INIT,
            1_000_000,
            99,
            serde_json::from_value(json!({"notes":"note"})).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            notes(
                &p,
                INIT,
                1_000_000,
                999,
                serde_json::from_value(json!({"notes":"x"})).unwrap()
            ),
            Err(ProfileError::Rule(404, _))
        ));
        assert!(matches!(
            list(&p, INIT, 1_000_000, &"x".repeat(101)),
            Err(ProfileError::Invalid(_))
        ));
        p.db.lock()
            .unwrap()
            .execute("DELETE FROM coaches WHERE telegram_id=42", [])
            .unwrap();
        p.owner = 999;
        assert!(matches!(
            list(&p, INIT, 1_000_000, ""),
            Err(ProfileError::Forbidden(_))
        ));
    }
}

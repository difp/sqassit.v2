//! Match coauthor management with the legacy API contract.
use crate::profile::{ProfileError, Profiles};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CoauthorInput {
    telegram_id: Option<i64>,
    username: Option<String>,
}
fn authorize(
    profiles: &Profiles,
    init: &str,
    now: i64,
    id: i64,
    db: &Connection,
) -> Result<(i64, i64), ProfileError> {
    let (actor, user) = profiles.user_id(init, now)?;
    profiles.register(db, actor, &user, now)?;
    let creator = db
        .query_row(
            "SELECT created_by FROM matches WHERE id=?1 AND (deleted_at IS NULL OR deleted_at=0)",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?
        .ok_or(ProfileError::Rule(404, "Матч не найден"))?;
    Ok((actor, creator))
}
pub fn list(profiles: &Profiles, init: &str, now: i64, id: i64) -> Result<Value, ProfileError> {
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let (actor, creator) = authorize(profiles, init, now, id, &db)?;
    let mut stmt=db.prepare("SELECT u.telegram_id,u.first_name,u.username,u.avatar_file_id FROM event_coauthors c JOIN users u ON u.telegram_id=c.user_id WHERE c.event_type='match' AND c.event_id=?1 ORDER BY c.created_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut result = Vec::new();
    for row in rows {
        let (user, name, username, file) = row.map_err(|_| ProfileError::Storage)?;
        result.push(json!({"telegram_id":user,"first_name":name.filter(|s|!s.is_empty()).unwrap_or_else(||"Игрок".into()),"username":username,"avatar_url":profiles.avatar_url(user,file.as_deref(),true,now)}));
    }
    Ok(json!({"ok":true,"can_manage":actor==creator,"coauthors":result}))
}
pub fn add(
    profiles: &Profiles,
    init: &str,
    now: i64,
    id: i64,
    value: CoauthorInput,
) -> Result<Value, ProfileError> {
    if value
        .username
        .as_ref()
        .is_some_and(|v| v.chars().count() > 64)
    {
        return Err(ProfileError::Invalid("Некорректный username."));
    }
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let (actor, creator) = authorize(profiles, init, now, id, &db)?;
    if actor != creator {
        return Err(ProfileError::Rule(
            403,
            "Добавлять соавторов может только организатор.",
        ));
    }
    let target = if let Some(target) = value.telegram_id {
        Some(target)
    } else if let Some(username) = value.username {
        db.query_row(
            "SELECT telegram_id FROM users WHERE username=?1 COLLATE NOCASE",
            [username.trim_start_matches('@')],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?
    } else {
        None
    };
    let target = target.ok_or(ProfileError::Rule(
        404,
        "Игрок не найден. Попросите его запустить бота.",
    ))?;
    if target == creator {
        return Err(ProfileError::Rule(400, "Организатор уже управляет матчем."));
    }
    let created=db.execute("INSERT OR IGNORE INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) VALUES('match',?1,?2,?3,?4)",params![id,target,actor,now]).map_err(|_|ProfileError::Storage)? != 0;
    Ok(json!({"ok":true,"created":created,"telegram_id":target}))
}
pub fn remove(
    profiles: &Profiles,
    init: &str,
    now: i64,
    id: i64,
    target: i64,
) -> Result<Value, ProfileError> {
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let (actor, creator) = authorize(profiles, init, now, id, &db)?;
    if actor != creator {
        return Err(ProfileError::Rule(
            403,
            "Снимать соавторов может только организатор.",
        ));
    }
    let removed = db
        .execute(
            "DELETE FROM event_coauthors WHERE event_type='match' AND event_id=?1 AND user_id=?2",
            params![id, target],
        )
        .map_err(|_| ProfileError::Storage)?
        != 0;
    Ok(json!({"ok":true,"removed":removed}))
}

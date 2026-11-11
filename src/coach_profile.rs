//! Public coach biography and training levels.
use crate::{
    coach,
    profile::{ProfileError, Profiles},
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct TrainerProfile {
    #[serde(default)]
    bio: String,
}
fn serialized(
    p: &Profiles,
    db: &rusqlite::Connection,
    id: i64,
    now: i64,
) -> Result<Value, ProfileError> {
    db.query_row("SELECT first_name,avatar_file_id,bio FROM users WHERE telegram_id=?1",[id],|r|{let name:Option<String>=r.get(0)?;let file:Option<String>=r.get(1)?;let bio:String=r.get(2)?;Ok(json!({"telegram_id":id,"first_name":name.filter(|s|!s.is_empty()).unwrap_or_else(||"Тренер".into()),"avatar_url":p.avatar_url(id,file.as_deref(),true,now),"bio":bio}))}).map_err(|_|ProfileError::Storage)
}
pub fn get(p: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    let id = coach::coach(p, &db, init, now)?;
    Ok(json!({"ok":true,"profile":serialized(p,&db,id,now)?}))
}
pub fn put(
    p: &Profiles,
    init: &str,
    now: i64,
    value: TrainerProfile,
) -> Result<Value, ProfileError> {
    if value.bio.chars().count() > 450 {
        return Err(ProfileError::Invalid("Описание слишком длинное."));
    }
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    let id = coach::coach(p, &db, init, now)?;
    db.execute(
        "UPDATE users SET bio=?1 WHERE telegram_id=?2",
        params![value.bio.trim(), id],
    )
    .map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true,"profile":serialized(p,&db,id,now)?}))
}
pub fn levels(p: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let (id, user) = p.user_id(init, now)?;
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    p.register(&db, id, &user, now)?;
    Ok(
        json!({"ok":true,"levels":["Новичок (Into)","Начинающий (Beginner)","Уверенный любитель (С)"]}),
    )
}

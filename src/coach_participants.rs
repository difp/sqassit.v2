//! Coach operations on training participants. The HTTP layer sends preview notifications.
use crate::{
    auth, booking, coach, identity,
    profile::{ProfileError, Profiles},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct ParticipantIn {
    pub user_id: i64,
}
#[derive(Deserialize)]
pub struct ReminderIn {
    pub enabled: bool,
    #[serde(default = "default_offset")]
    pub offset_min: i64,
}
fn default_offset() -> i64 {
    270
}
fn training(
    db: &Connection,
    id: i64,
    actor: i64,
    owner_only: bool,
) -> Result<(i64, i64), ProfileError> {
    type TrainingAccess = (i64, i64, Option<i64>, Option<i64>, bool);
    let row:Option<TrainingAccess>=db.query_row("SELECT t.start_at,t.capacity,t.created_by,t.deleted_at,EXISTS(SELECT 1 FROM event_coauthors c WHERE c.event_type='training' AND c.event_id=t.id AND c.user_id=?2) FROM trainings t WHERE t.id=?1",params![id,actor],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|_|ProfileError::Storage)?;
    let (start, capacity, owner, deleted, coauthor) =
        row.ok_or(ProfileError::Rule(404, "Тренировка не найдена"))?;
    if deleted.unwrap_or(0) != 0 || (owner != Some(actor) && (owner_only || !coauthor)) {
        return Err(ProfileError::Rule(404, "Тренировка не найдена"));
    }
    Ok((start, capacity))
}
pub fn list(profiles: &Profiles, init: &str, now: i64, id: i64) -> Result<Value, ProfileError> {
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let _ = coach::coach(profiles, &db, init, now)?;
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM trainings WHERE id=?1 AND (deleted_at IS NULL OR deleted_at=0))",[id],|r|r.get(0)).map_err(|_|ProfileError::Storage)?;
    if !exists {
        return Err(ProfileError::Rule(404, "Тренировка не найдена"));
    }
    let mut stmt=db.prepare("SELECT b.status,u.telegram_id,u.first_name,u.username,u.show_username,u.avatar_file_id,u.about,u.display_mode FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=?1 ORDER BY CASE b.status WHEN 'booked' THEN 0 WHEN 'waitlist' THEN 1 ELSE 2 END,b.position ASC,b.created_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut out = Vec::new();
    for row in rows {
        let (status, user_id, name, username, visible, file, about, mode) =
            row.map_err(|_| ProfileError::Storage)?;
        let anonymous = mode == "animal";
        let (emoji, animal) = identity::animal_identity(user_id);
        let (name, avatar, avatar_emoji) = if anonymous {
            (animal.to_owned(), String::new(), emoji)
        } else {
            (
                name.filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Игрок".into()),
                profiles.avatar_url(user_id, file.as_deref(), false, now),
                "",
            )
        };
        out.push(json!({"first_name":name,"avatar_url":avatar,"avatar_emoji":avatar_emoji,"anonymous":anonymous,"profile_token":auth::issue_scoped_token(&profiles.bot_token,"profile",0,user_id,now,86_400),"telegram_id":user_id,"username":username.filter(|s|!s.is_empty()),"username_visible":visible,"about":about,"status":status}));
    }
    Ok(json!({"ok":true,"participants":out}))
}
#[derive(Clone, Copy)]
pub enum Action {
    Add,
    Approve,
    Reject,
    Cancel,
    Restore,
}
pub fn change(
    profiles: &Profiles,
    init: &str,
    now: i64,
    id: i64,
    user_id: i64,
    action: Action,
) -> Result<Value, ProfileError> {
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let actor = coach::coach(profiles, &db, init, now)?;
    let (start, capacity) = training(
        &db,
        id,
        actor,
        matches!(action, Action::Cancel | Action::Restore),
    )?;
    let existing: Option<String> = db
        .query_row(
            "SELECT status FROM bookings WHERE training_id=?1 AND user_id=?2",
            params![id, user_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?;
    match action {
        Action::Add | Action::Restore => {
            if matches!(action, Action::Add) && start <= now {
                return Err(ProfileError::Rule(400, "Тренировка уже началась"));
            }
            let user_exists: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM users WHERE telegram_id=?1)",
                    [user_id],
                    |r| r.get(0),
                )
                .map_err(|_| ProfileError::Storage)?;
            if !user_exists {
                return Err(ProfileError::Rule(404, "Игрок не найден"));
            }
            if matches!(action, Action::Restore) && existing.as_deref() != Some("cancelled") {
                return Err(ProfileError::Rule(
                    400,
                    "Можно вернуть только снятого игрока",
                ));
            }
            if matches!(action, Action::Add)
                && matches!(existing.as_deref(), Some("pending" | "booked" | "waitlist"))
            {
                return Ok(json!({"ok":true,"status":existing,"already":true}));
            }
            let status = booking::book(&mut db, id, user_id, capacity, false, true, now)
                .map_err(|_| ProfileError::Storage)?;
            if matches!(action, Action::Add) {
                Ok(json!({"ok":true,"status":status,"already":false}))
            } else {
                Ok(json!({"ok":true,"status":status}))
            }
        }
        Action::Approve => {
            let status =
                booking::approve(&mut db, id, user_id).map_err(|_| ProfileError::Storage)?;
            if status.is_empty() {
                return Err(ProfileError::Rule(404, "Заявка не найдена"));
            }
            Ok(json!({"ok":true,"status":status}))
        }
        Action::Reject => {
            if !booking::reject(&mut db, id, user_id).map_err(|_| ProfileError::Storage)? {
                return Err(ProfileError::Rule(404, "Заявка не найдена"));
            }
            Ok(json!({"ok":true}))
        }
        Action::Cancel => {
            if !matches!(existing.as_deref(), Some("booked" | "waitlist")) {
                return Err(ProfileError::Rule(404, "Записи не существует"));
            }
            let promoted =
                booking::cancel(&mut db, id, user_id).map_err(|_| ProfileError::Storage)?;
            Ok(json!({"ok":true,"promoted":promoted.map(|x|x.to_string())}))
        }
    }
}
pub fn reminder(
    profiles: &Profiles,
    init: &str,
    now: i64,
    id: i64,
    payload: ReminderIn,
) -> Result<Value, ProfileError> {
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let actor = coach::coach(profiles, &db, init, now)?;
    if !(0..=2880).contains(&payload.offset_min) {
        return Err(ProfileError::Invalid("Некорректное время напоминания."));
    }
    training(&db, id, actor, true)?;
    let tx = db.transaction().map_err(|_| ProfileError::Storage)?;
    tx.execute("UPDATE trainings SET reminder_enabled=?1,reminder_offset_min=?2,reminder_sent=0 WHERE id=?3",params![payload.enabled,payload.offset_min,id]).map_err(|_|ProfileError::Storage)?;
    tx.execute("DELETE FROM reminder_deliveries WHERE training_id=?1", [id])
        .map_err(|_| ProfileError::Storage)?;
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true}))
}

//! Random tournament codes and training room access.
use crate::profile::{ProfileError, Profiles};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

pub(crate) fn new_code() -> Result<String, ProfileError> {
    let mut bytes = [0_u8; 8];
    getrandom::getrandom(&mut bytes).map_err(|_| ProfileError::Storage)?;
    const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    Ok(format!(
        "SQ-{}",
        bytes
            .iter()
            .map(|b| char::from(ALPHABET[(b & 31) as usize]))
            .collect::<String>()
    ))
}
pub fn training(profiles: &Profiles, init: &str, now: i64, id: i64) -> Result<Value, ProfileError> {
    let (actor, user) = profiles.user_id(init, now)?;
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    profiles.register(&db, actor, &user, now)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    let event:Option<(Option<i64>,Option<String>)>=tx.query_row("SELECT created_by,room_code FROM trainings WHERE id=?1 AND (deleted_at IS NULL OR deleted_at=0)",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|ProfileError::Storage)?;
    let (creator, existing) = event.ok_or(ProfileError::Rule(404, "Тренировка не найдена"))?;
    let allowed=creator==Some(actor) || tx.query_row("SELECT EXISTS(SELECT 1 FROM bookings WHERE training_id=?1 AND user_id=?2 AND status IN ('booked','pending','waitlist') UNION ALL SELECT 1 FROM event_coauthors WHERE event_type='training' AND event_id=?1 AND user_id=?2)",params![id,actor],|r|r.get::<_,bool>(0)).map_err(|_|ProfileError::Storage)?;
    if !allowed {
        return Err(ProfileError::Rule(
            403,
            "Турнир доступен участникам занятия",
        ));
    }
    let code = match existing {
        Some(code) => code,
        None => {
            let code = new_code()?;
            tx.execute(
                "UPDATE trainings SET room_code=?1 WHERE id=?2 AND room_code IS NULL",
                params![code, id],
            )
            .map_err(|_| ProfileError::Storage)?;
            code
        }
    };
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true,"code":code}))
}

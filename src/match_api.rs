//! Match attendance API, excluding Telegram notifications which are not migrated yet.
use crate::{
    match_booking,
    profile::{ProfileError, Profiles},
    rate_limit::RateLimiter,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct MatchApi {
    limiter: Arc<Mutex<RateLimiter>>,
    create_limiter: Arc<Mutex<RateLimiter>>,
}
impl Default for MatchApi {
    fn default() -> Self {
        Self {
            limiter: Arc::new(Mutex::new(RateLimiter::new(30, Duration::from_secs(60)))),
            create_limiter: Arc::new(Mutex::new(RateLimiter::new(3, Duration::from_secs(600)))),
        }
    }
}
#[derive(Clone, Copy)]
pub enum Action {
    Join,
    Leave,
    Approve(i64),
    Reject(i64),
}
fn default_title() -> String {
    "Матч".into()
}
fn default_duration() -> i64 {
    60
}
fn default_capacity() -> i64 {
    4
}
#[derive(serde::Deserialize)]
pub struct NewMatch {
    date: String,
    time: String,
    #[serde(default = "default_title")]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    location: String,
    #[serde(default = "default_duration")]
    duration_min: i64,
    #[serde(default = "default_capacity")]
    max_players: i64,
    #[serde(default)]
    level: String,
}
impl MatchApi {
    /// Only booked players, the organizer and coauthors may obtain the room code.
    pub fn room(
        &self,
        profiles: &Profiles,
        init: &str,
        now: i64,
        id: i64,
    ) -> Result<Value, ProfileError> {
        let (actor, user) = profiles.user_id(init, now)?;
        let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        profiles.register(&db, actor, &user, now)?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| ProfileError::Storage)?;
        let event: Option<(i64,Option<String>)>=tx.query_row("SELECT created_by,room_code FROM matches WHERE id=?1 AND (deleted_at IS NULL OR deleted_at=0)",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|ProfileError::Storage)?;
        let (creator, existing) = event.ok_or(ProfileError::Rule(404, "Матч не найден"))?;
        let allowed: bool=creator==actor || tx.query_row("SELECT EXISTS(SELECT 1 FROM match_players WHERE match_id=?1 AND user_id=?2 AND status='booked' UNION ALL SELECT 1 FROM event_coauthors WHERE event_type='match' AND event_id=?1 AND user_id=?2)",params![id,actor],|r|r.get(0)).map_err(|_|ProfileError::Storage)?;
        if !allowed {
            return Err(ProfileError::Rule(403, "Турнир доступен участникам матча"));
        }
        let code = if let Some(code) = existing {
            code
        } else {
            let mut bytes = [0_u8; 8];
            getrandom::getrandom(&mut bytes).map_err(|_| ProfileError::Storage)?;
            const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
            let code = format!(
                "SQ-{}",
                bytes
                    .iter()
                    .map(|b| char::from(ALPHABET[(b & 31) as usize]))
                    .collect::<String>()
            );
            tx.execute(
                "UPDATE matches SET room_code=?1 WHERE id=?2 AND room_code IS NULL",
                params![code, id],
            )
            .map_err(|_| ProfileError::Storage)?;
            code
        };
        tx.commit().map_err(|_| ProfileError::Storage)?;
        Ok(json!({"ok":true,"code":code}))
    }
    /// Soft-delete is reserved for the organizer. Event notifications are not yet migrated.
    pub fn delete(
        &self,
        profiles: &Profiles,
        init: &str,
        now: i64,
        id: i64,
    ) -> Result<Value, ProfileError> {
        let (actor, user) = profiles.user_id(init, now)?;
        let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        profiles.register(&db, actor, &user, now)?;
        let updated=db.execute("UPDATE matches SET deleted_at=?1 WHERE id=?2 AND created_by=?3 AND (deleted_at IS NULL OR deleted_at=0)",params![now,id,actor]).map_err(|_|ProfileError::Storage)?;
        if updated == 0 {
            return Err(ProfileError::Rule(404, "Матч не найден"));
        }
        Ok(json!({"ok":true}))
    }
    pub fn create(
        &self,
        profiles: &Profiles,
        init: &str,
        now: i64,
        value: NewMatch,
    ) -> Result<Value, ProfileError> {
        let (actor, user) = profiles.user_id(init, now)?;
        self.create_limiter
            .lock()
            .map_err(|_| ProfileError::Storage)?
            .check(&actor.to_string())
            .map_err(ProfileError::RateLimited)?;
        if !(1..=100).contains(&value.title.chars().count())
            || value.description.chars().count() > 450
            || value.location.chars().count() > 120
            || value.level.chars().count() > 60
            || !(15..=600).contains(&value.duration_min)
            || !(2..=20).contains(&value.max_players)
        {
            return Err(ProfileError::Invalid("Некорректные данные матча."));
        }
        let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        profiles.register(&db, actor, &user, now)?;
        if profiles.phone_required_from.is_some_and(|date| now >= date) {
            let phone: String = db
                .query_row(
                    "SELECT phone FROM users WHERE telegram_id=?1",
                    [actor],
                    |r| r.get(0),
                )
                .map_err(|_| ProfileError::Storage)?;
            if phone.is_empty() {
                return Err(ProfileError::Rule(
                    403,
                    "Укажите телефон в настройках профиля, чтобы создать матч.",
                ));
            }
        }
        let start = crate::coach::local_timestamp(&value.date, &value.time, profiles.timezone)?;
        if start <= now {
            return Err(ProfileError::Rule(400, "Нельзя создать матч в прошлом"));
        }
        let level = value.level.trim();
        if !level.is_empty()
            && ![
                "Новичок (Into)",
                "Начинающий (Beginner)",
                "Уверенный любитель (С)",
            ]
            .contains(&level)
        {
            return Err(ProfileError::Rule(422, "Неизвестный уровень игры"));
        }
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| ProfileError::Storage)?;
        tx.execute("INSERT INTO matches(title,description,location,start_at,duration_min,max_players,level,created_by,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![value.title.trim(),value.description.trim(),value.location.trim(),start,value.duration_min,value.max_players,level,actor,now]).map_err(|_|ProfileError::Storage)?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO match_players(match_id,user_id,status,created_at) VALUES(?1,?2,'booked',?3)",params![id,actor,now]).map_err(|_|ProfileError::Storage)?;
        tx.commit().map_err(|_| ProfileError::Storage)?;
        Ok(json!({"ok":true,"id":id}))
    }
    pub fn attendance(
        &self,
        profiles: &Profiles,
        init: &str,
        now: i64,
        match_id: i64,
        action: Action,
    ) -> Result<(Value, Option<i64>), ProfileError> {
        let (actor, user) = profiles.user_id(init, now)?;
        if matches!(action, Action::Join | Action::Leave) {
            self.limiter
                .lock()
                .map_err(|_| ProfileError::Storage)?
                .check(&actor.to_string())
                .map_err(ProfileError::RateLimited)?;
        }
        let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        profiles.register(&db, actor, &user, now)?;
        let event: Option<(i64, i64, Option<i64>)> = db
            .query_row(
                "SELECT start_at,created_by,deleted_at FROM matches WHERE id=?1",
                [match_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|_| ProfileError::Storage)?;
        let (start, creator, deleted) = event.ok_or(ProfileError::Rule(404, "Матч не найден"))?;
        if deleted.unwrap_or(0) != 0 {
            return Err(ProfileError::Rule(404, "Матч не найден"));
        }
        if matches!(action, Action::Join | Action::Leave) && start <= now {
            return Err(ProfileError::Rule(400, "Матч уже начался"));
        }
        if matches!(action, Action::Approve(_) | Action::Reject(_)) && creator != actor {
            let is_coauthor: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM event_coauthors WHERE event_type='match' AND event_id=?1 AND user_id=?2)",params![match_id,actor],|r| r.get(0)).map_err(|_|ProfileError::Storage)?;
            if !is_coauthor {
                return Err(ProfileError::Rule(404, "Матч не найден"));
            }
        }
        match action {
            Action::Join => {
                if profiles.phone_required_from.is_some_and(|date| now >= date) {
                    let phone: String = db
                        .query_row(
                            "SELECT phone FROM users WHERE telegram_id=?1",
                            [actor],
                            |r| r.get(0),
                        )
                        .map_err(|_| ProfileError::Storage)?;
                    if phone.is_empty() {
                        return Err(ProfileError::Rule(
                            403,
                            "Укажите телефон в настройках профиля, чтобы присоединиться.",
                        ));
                    }
                }
                let status = match_booking::join(
                    &mut db,
                    match_id,
                    actor,
                    profiles.require_event_approval,
                    now,
                )
                .map_err(|_| ProfileError::Storage)?;
                if status == "full" {
                    return Err(ProfileError::Rule(409, "Мест нет"));
                }
                Ok((json!({"ok":true,"status":status}), None))
            }
            Action::Leave => {
                let (removed, promoted) = match_booking::leave(&mut db, match_id, actor)
                    .map_err(|_| ProfileError::Storage)?;
                if !removed {
                    return Err(ProfileError::Rule(400, "Вы не участвуете в этом матче"));
                }
                Ok((json!({"ok":true}), promoted))
            }
            Action::Approve(target) => {
                let status = match_booking::approve(&mut db, match_id, target, actor, now)
                    .map_err(|_| ProfileError::Storage)?
                    .ok_or(ProfileError::Rule(404, "Заявка не найдена"))?;
                Ok((json!({"ok":true,"status":status}), None))
            }
            Action::Reject(target) => {
                if !match_booking::reject(&db, match_id, target, actor, now)
                    .map_err(|_| ProfileError::Storage)?
                {
                    return Err(ProfileError::Rule(404, "Заявка не найдена"));
                }
                Ok((json!({"ok":true}), None))
            }
        }
    }
}

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
}
impl Default for MatchApi {
    fn default() -> Self {
        Self {
            limiter: Arc::new(Mutex::new(RateLimiter::new(30, Duration::from_secs(60)))),
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
impl MatchApi {
    pub fn attendance(
        &self,
        profiles: &Profiles,
        init: &str,
        now: i64,
        match_id: i64,
        action: Action,
    ) -> Result<Value, ProfileError> {
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
                Ok(json!({"ok":true,"status":status}))
            }
            Action::Leave => {
                let (removed, _) = match_booking::leave(&mut db, match_id, actor)
                    .map_err(|_| ProfileError::Storage)?;
                if !removed {
                    return Err(ProfileError::Rule(400, "Вы не участвуете в этом матче"));
                }
                Ok(json!({"ok":true}))
            }
            Action::Approve(target) => {
                let status = match_booking::approve(&mut db, match_id, target, actor, now)
                    .map_err(|_| ProfileError::Storage)?
                    .ok_or(ProfileError::Rule(404, "Заявка не найдена"))?;
                Ok(json!({"ok":true,"status":status}))
            }
            Action::Reject(target) => {
                if !match_booking::reject(&db, match_id, target, actor, now)
                    .map_err(|_| ProfileError::Storage)?
                {
                    return Err(ProfileError::Rule(404, "Заявка не найдена"));
                }
                Ok(json!({"ok":true}))
            }
        }
    }
}

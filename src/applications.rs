//! Coach application workflows; Telegram notices are sent by the HTTP layer.
use crate::{
    auth, coach_registry, identity,
    profile::{ProfileError, Profiles},
    rate_limit::RateLimiter,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Applications {
    limiter: Arc<Mutex<RateLimiter>>,
}
impl Default for Applications {
    fn default() -> Self {
        Self {
            limiter: Arc::new(Mutex::new(RateLimiter::new(5, Duration::from_secs(300)))),
        }
    }
}
#[derive(Deserialize)]
pub struct ApplicationInput {
    #[serde(default)]
    message: String,
}
impl Applications {
    pub fn apply(
        &self,
        p: &Profiles,
        init: &str,
        now: i64,
        value: ApplicationInput,
    ) -> Result<Value, ProfileError> {
        let (id, user) = p.user_id(init, now)?;
        self.limiter
            .lock()
            .map_err(|_| ProfileError::Storage)?
            .check(&id.to_string())
            .map_err(ProfileError::RateLimited)?;
        if value.message.chars().count() > 1000 {
            return Err(ProfileError::Invalid("Сообщение слишком длинное."));
        }
        let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
        p.register(&db, id, &user, now)?;
        let is_coach: bool = id == p.owner
            || db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(|_| ProfileError::Storage)?;
        if is_coach {
            return Err(ProfileError::Rule(400, "Вы уже тренер."));
        }
        db.execute("INSERT INTO coach_applications(telegram_id,message,status,created_at) VALUES(?1,?2,'pending',?3) ON CONFLICT(telegram_id) DO UPDATE SET message=excluded.message,status='pending',created_at=excluded.created_at,decided_by=NULL,decided_at=NULL",params![id,value.message.trim(),now]).map_err(|_|ProfileError::Storage)?;
        Ok(json!({"ok":true}))
    }
}
pub fn status(p: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let (id, user) = p.user_id(init, now)?;
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    p.register(&db, id, &user, now)?;
    let is_coach: bool = id == p.owner
        || db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| ProfileError::Storage)?;
    let application: Option<(String, String)> = db
        .query_row(
            "SELECT status,message FROM coach_applications WHERE telegram_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?;
    Ok(
        json!({"ok":true,"is_coach":is_coach,"application":application.map(|(status,message)|json!({"status":status,"message":message}))}),
    )
}
pub fn list(p: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    coach_registry::manager(p, &db, init, now)?;
    let mut stmt=db.prepare("SELECT a.telegram_id,a.message,a.created_at,u.first_name,u.username,u.show_username,u.avatar_file_id,u.display_mode FROM coach_applications a LEFT JOIN users u ON u.telegram_id=a.telegram_id WHERE a.status='pending' ORDER BY a.created_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<bool>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut applications = Vec::new();
    for row in rows {
        let (id, message, created, name, username, visible, file, mode) =
            row.map_err(|_| ProfileError::Storage)?;
        let anonymous = mode.as_deref() == Some("animal");
        let (emoji, animal) = identity::animal_identity(id);
        let name = if anonymous {
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
        applications.push(json!({"telegram_id":id,"first_name":name,"avatar_url":avatar,"avatar_emoji":if anonymous{emoji}else{""},"anonymous":anonymous,"profile_token":auth::issue_scoped_token(&p.bot_token,"profile",0,id,now,86400),"username":username,"username_visible":visible.unwrap_or(false),"message":message,"created_at":created}));
    }
    Ok(json!({"ok":true,"applications":applications}))
}
pub fn decide(
    p: &Profiles,
    init: &str,
    now: i64,
    target: i64,
    approve: bool,
) -> Result<Value, ProfileError> {
    let mut db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    let actor = coach_registry::manager(p, &db, init, now)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM coach_applications WHERE telegram_id=?1",
            [target],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?;
    let status = status.ok_or(ProfileError::Rule(404, "Заявка не найдена"))?;
    if status != "pending" {
        return Err(ProfileError::Rule(409, "Заявка уже рассмотрена"));
    }
    if approve {
        tx.execute(
            "INSERT OR IGNORE INTO coaches(telegram_id,added_by,created_at) VALUES(?1,?2,?3)",
            params![target, actor, now],
        )
        .map_err(|_| ProfileError::Storage)?;
        tx.execute("UPDATE users SET is_coach=1 WHERE telegram_id=?1", [target])
            .map_err(|_| ProfileError::Storage)?;
    }
    tx.execute(
        "UPDATE coach_applications SET status=?1,decided_by=?2,decided_at=?3 WHERE telegram_id=?4",
        params![
            if approve { "approved" } else { "rejected" },
            actor,
            now,
            target
        ],
    )
    .map_err(|_| ProfileError::Storage)?;
    tx.execute("UPDATE event_notifications SET delete_at=?1 WHERE kind='coach_application' AND entity_id=?2",params![now,target]).map_err(|_|ProfileError::Storage)?;
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    const INIT:&str="auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    #[test]
    fn simultaneous_decisions_have_one_winner() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute(
            "INSERT INTO coach_applications(telegram_id,status,created_at) VALUES(77,'pending',0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at) VALUES('coach_application',77,42,42,123,'',0,2000000)",[]).unwrap();
        let p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "123456:TEST-TOKEN".into(),
            owner: 42,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(RateLimiter::new(60, Duration::from_secs(60)))),
        };
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [true, false]
            .into_iter()
            .map(|approve| {
                let p = p.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    decide(&p, INIT, 1_000_000, 77, approve)
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(ProfileError::Rule(409, _))))
                .count(),
            1
        );
        let db = p.db.lock().unwrap();
        let status: String = db
            .query_row(
                "SELECT status FROM coach_applications WHERE telegram_id=77",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let is_coach: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=77)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status == "approved", is_coach);
        assert_eq!(
            db.query_row(
                "SELECT delete_at FROM event_notifications WHERE kind='coach_application'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1_000_000
        );
    }
}

//! Private player/coach conversations. Message bodies never enter Telegram notifications.
use crate::{
    coach, identity,
    profile::{ProfileError, Profiles},
    rate_limit::RateLimiter,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
pub struct Messages {
    pub profiles: Profiles,
    limiter: Arc<Mutex<RateLimiter>>,
}
#[derive(Deserialize)]
pub struct MessageIn {
    pub text: String,
    pub training_id: Option<i64>,
}
pub struct Sent {
    pub recipient: i64,
    pub author: i64,
    pub recipient_is_coach: bool,
}
impl Messages {
    pub fn new(profiles: Profiles) -> Self {
        Self {
            profiles,
            limiter: Arc::new(Mutex::new(RateLimiter::new(20, Duration::from_secs(60)))),
        }
    }
    fn auth(
        &self,
        db: &Connection,
        init: &str,
        now: i64,
        coach_only: bool,
    ) -> Result<i64, ProfileError> {
        if coach_only {
            return coach::coach(&self.profiles, db, init, now);
        }
        let (id, user) = self.profiles.user_id(init, now)?;
        self.profiles.register(db, id, &user, now)?;
        Ok(id)
    }
    fn validate(&self, id: i64, text: &str) -> Result<String, ProfileError> {
        self.limiter
            .lock()
            .map_err(|_| ProfileError::Storage)?
            .check(&id.to_string())
            .map_err(ProfileError::RateLimited)?;
        if text.is_empty() || text.chars().count() > 1000 {
            return Err(ProfileError::Invalid("Некорректная длина сообщения."));
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(ProfileError::Rule(422, "Пустое сообщение."));
        }
        Ok(text.to_owned())
    }
    pub fn send_to_coach(
        &self,
        init: &str,
        now: i64,
        payload: MessageIn,
    ) -> Result<Sent, ProfileError> {
        let db = self.profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        let author = self.auth(&db, init, now, false)?;
        let text = self.validate(author, &payload.text)?;
        let recipient = if let Some(id) = payload.training_id {
            let row: Option<(Option<i64>, Option<i64>)> = db
                .query_row(
                    "SELECT created_by,deleted_at FROM trainings WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|_| ProfileError::Storage)?;
            let (owner, deleted) = row.ok_or(ProfileError::Rule(404, "Тренировка не найдена."))?;
            if deleted.unwrap_or(0) != 0 {
                return Err(ProfileError::Rule(404, "Тренировка не найдена."));
            }
            owner.filter(|n| *n != 0).unwrap_or(self.profiles.owner)
        } else {
            self.profiles.owner
        };
        db.execute(
            "INSERT INTO messages(from_user,to_user,text,created_at) VALUES(?1,?2,?3,?4)",
            params![author, recipient, text, now],
        )
        .map_err(|_| ProfileError::Storage)?;
        Ok(Sent {
            author,
            recipient,
            recipient_is_coach: true,
        })
    }
    pub fn coach_reply(
        &self,
        init: &str,
        now: i64,
        recipient: i64,
        payload: MessageIn,
    ) -> Result<Sent, ProfileError> {
        let db = self.profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        let author = self.auth(&db, init, now, true)?;
        let text = self.validate(author, &payload.text)?;
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM users WHERE telegram_id=?1)",
                [recipient],
                |r| r.get(0),
            )
            .map_err(|_| ProfileError::Storage)?;
        if !exists {
            return Err(ProfileError::Rule(404, "Игрок не найден."));
        }
        db.execute(
            "INSERT INTO messages(from_user,to_user,text,created_at) VALUES(?1,?2,?3,?4)",
            params![author, recipient, text, now],
        )
        .map_err(|_| ProfileError::Storage)?;
        Ok(Sent {
            author,
            recipient,
            recipient_is_coach: false,
        })
    }
    pub fn thread(&self, init: &str, now: i64, other: Option<i64>) -> Result<Value, ProfileError> {
        let db = self.profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        let id = self.auth(&db, init, now, other.is_some())?;
        let sql = if other.is_some() {
            "SELECT id,from_user,text,created_at FROM messages WHERE (from_user=?1 AND to_user=?2) OR (from_user=?2 AND to_user=?1) ORDER BY id ASC"
        } else {
            "SELECT id,from_user,text,created_at FROM messages WHERE from_user=?1 OR to_user=?1 ORDER BY id ASC"
        };
        let mut stmt = db.prepare(sql).map_err(|_| ProfileError::Storage)?;
        let collect = |r: &rusqlite::Row<'_>| {
            Ok(
                json!({"id":r.get::<_,i64>(0)?,"from_me":r.get::<_,i64>(1)?==id,"text":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?}),
            )
        };
        let messages = if let Some(other) = other {
            stmt.query_map(params![id, other], collect)
                .map_err(|_| ProfileError::Storage)?
                .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([id], collect)
                .map_err(|_| ProfileError::Storage)?
                .collect::<Result<Vec<_>, _>>()
        }
        .map_err(|_| ProfileError::Storage)?;
        if let Some(other) = other {
            db.execute("UPDATE event_notifications SET delete_at=?1 WHERE kind='chat' AND entity_id=?2 AND chat_id=?3",params![now,other,id]).map_err(|_|ProfileError::Storage)?;
        } else {
            db.execute("UPDATE event_notifications SET delete_at=?1 WHERE kind='chat' AND chat_id=?2 AND entity_id IN (SELECT from_user FROM messages WHERE to_user=?2)",params![now,id]).map_err(|_|ProfileError::Storage)?;
        }
        Ok(json!({"ok":true,"messages":messages}))
    }
    pub fn conversations(&self, init: &str, now: i64) -> Result<Value, ProfileError> {
        let db = self.profiles.db.lock().map_err(|_| ProfileError::Storage)?;
        let id = self.auth(&db, init, now, true)?;
        let mut stmt=db.prepare("SELECT m.from_user,m.to_user,m.text,m.created_at,u.first_name,u.display_mode,u.avatar_file_id FROM messages m JOIN users u ON u.telegram_id=CASE WHEN m.from_user=?1 THEN m.to_user ELSE m.from_user END WHERE m.from_user=?1 OR m.to_user=?1 ORDER BY m.id DESC").map_err(|_|ProfileError::Storage)?;
        let rows = stmt
            .query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|_| ProfileError::Storage)?;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for row in rows {
            let (from, to, text, created, name, mode, file) =
                row.map_err(|_| ProfileError::Storage)?;
            let other = if from == id { to } else { from };
            if !seen.insert(other) {
                continue;
            }
            let anonymous = mode == "animal";
            let display = if anonymous {
                identity::animal_identity(other).1.to_owned()
            } else {
                name.filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Игрок".into())
            };
            let avatar = if anonymous {
                String::new()
            } else {
                self.profiles.avatar_url(other, file.as_deref(), true, now)
            };
            out.push(json!({"telegram_id":other,"first_name":display,"avatar_url":avatar,"last_text":text,"last_at":created}));
        }
        Ok(json!({"ok":true,"conversations":out}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    fn token(id: i64) -> String {
        let user = format!("{{\"id\":{id},\"first_name\":\"U{id}\"}}");
        let date = "1000000";
        let data = format!("auth_date={date}\nuser={user}");
        let mut key = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
        key.update(b"test-token");
        let mut mac = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes()).unwrap();
        mac.update(data.as_bytes());
        let hash = hex::encode(mac.finalize().into_bytes());
        form_urlencoded::Serializer::new(String::new())
            .append_pair("auth_date", date)
            .append_pair("user", &user)
            .append_pair("hash", &hash)
            .finish()
    }
    fn service() -> Messages {
        let mut db = Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute(
            "INSERT INTO coaches(telegram_id,created_at) VALUES(777,0),(888,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO trainings(id,title,start_at,created_by,created_at) VALUES(5,'T',2000000,777,0)",[]).unwrap();
        let profiles = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test-token".into(),
            owner: 123,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(RateLimiter::new(60, Duration::from_secs(60)))),
        };
        Messages::new(profiles)
    }
    #[test]
    fn private_threads_route_to_training_coach_and_isolate_other_coaches() {
        let svc = service();
        let player = token(42);
        let coach1 = token(123);
        let coach2 = token(777);
        let outsider = token(888);
        let another_player = token(43);
        svc.send_to_coach(
            &player,
            1_000_000,
            MessageIn {
                text: " default ".into(),
                training_id: None,
            },
        )
        .unwrap();
        svc.send_to_coach(
            &player,
            1_000_001,
            MessageIn {
                text: " private to coach two ".into(),
                training_id: Some(5),
            },
        )
        .unwrap();
        svc.coach_reply(
            &coach2,
            1_000_002,
            42,
            MessageIn {
                text: "reply".into(),
                training_id: None,
            },
        )
        .unwrap();
        let p = svc.thread(&player, 1_000_003, None).unwrap();
        assert_eq!(p["messages"].as_array().unwrap().len(), 3);
        assert_eq!(p["messages"][2]["from_me"], false);
        let c1 = svc.thread(&coach1, 1_000_003, Some(42)).unwrap();
        assert_eq!(c1["messages"].as_array().unwrap().len(), 1);
        assert_eq!(c1["messages"][0]["text"], "default");
        let c2 = svc.thread(&coach2, 1_000_003, Some(42)).unwrap();
        assert_eq!(c2["messages"].as_array().unwrap().len(), 2);
        assert_eq!(c2["messages"][1]["from_me"], true);
        assert_eq!(
            svc.thread(&outsider, 1_000_003, Some(42)).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            svc.thread(&another_player, 1_000_003, None).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert!(matches!(
            svc.thread(&another_player, 1_000_003, Some(42)),
            Err(ProfileError::Forbidden(_))
        ));
        assert_eq!(
            svc.conversations(&coach2, 1_000_003).unwrap()["conversations"][0]["last_text"],
            "reply"
        );
        assert!(matches!(
            svc.send_to_coach(
                &player,
                1_000_003,
                MessageIn {
                    text: "   ".into(),
                    training_id: None
                }
            ),
            Err(ProfileError::Rule(422, _))
        ));
    }
}

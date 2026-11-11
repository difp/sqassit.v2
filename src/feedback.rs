//! Feedback persistence and owner notification payload.
use crate::{
    bot, identity,
    profile::{ProfileError, Profiles},
    rate_limit::RateLimiter,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Feedback {
    limiter: Arc<Mutex<RateLimiter>>,
}
impl Default for Feedback {
    fn default() -> Self {
        Self {
            limiter: Arc::new(Mutex::new(RateLimiter::new(3, Duration::from_secs(600)))),
        }
    }
}
#[derive(Deserialize)]
pub struct FeedbackInput {
    kind: String,
    text: String,
    #[serde(default)]
    page: String,
}
pub struct Submitted {
    pub text: String,
}
impl Feedback {
    pub fn submit(
        &self,
        p: &Profiles,
        init: &str,
        now: i64,
        value: FeedbackInput,
    ) -> Result<Submitted, ProfileError> {
        let (id, user) = p.user_id(init, now)?;
        self.limiter
            .lock()
            .map_err(|_| ProfileError::Storage)?
            .check(&id.to_string())
            .map_err(ProfileError::RateLimited)?;
        if !["bug", "idea"].contains(&value.kind.as_str())
            || value.text.chars().count() > 1500
            || value.page.chars().count() > 40
        {
            return Err(ProfileError::Invalid("Некорректные данные обращения."));
        }
        let text = value.text.trim();
        if text.chars().count() < 3 {
            return Err(ProfileError::Rule(
                422,
                "Опишите проблему или предложение чуть подробнее.",
            ));
        }
        let page = if ["Тренировки", "Матч", "Тренер"].contains(&value.page.as_str())
        {
            value.page.as_str()
        } else {
            "Приложение"
        };
        let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
        p.register(&db, id, &user, now)?;
        let (name, mode): (Option<String>, String) = db
            .query_row(
                "SELECT first_name,display_mode FROM users WHERE telegram_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| ProfileError::Storage)?;
        db.execute(
            "INSERT INTO feedback(user_id,kind,text,page,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![id, value.kind, text, page, now],
        )
        .map_err(|_| ProfileError::Storage)?;
        let name = if mode == "animal" {
            identity::animal_identity(id).1.to_owned()
        } else {
            name.filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Игрок".into())
        };
        let label = if value.kind == "bug" {
            "🐞 <b>Проблема</b>"
        } else {
            "💡 <b>Предложение</b>"
        };
        Ok(Submitted {
            text: format!(
                "{label}\n👤 {}\n📍 {}\n\n{}",
                bot::escape(&name),
                bot::escape(page),
                bot::escape(text)
            ),
        })
    }
}
pub fn markup(base: &str) -> serde_json::Value {
    json!({"inline_keyboard":[[{"text":"Открыть приложение","web_app":{"url":bot::web_app_url(base)}}]]})
}

#[cfg(test)]
mod tests {
    use super::*;
    const INIT:&str="auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    #[test]
    fn stores_feedback_with_anonymous_escaped_notification_and_rate_limit() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO users(telegram_id,first_name,display_mode,created_at) VALUES(42,'Secret','animal',0)",[]).unwrap();
        let p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "123456:TEST-TOKEN".into(),
            owner: 99,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(RateLimiter::new(60, Duration::from_secs(60)))),
        };
        let service = Feedback::default();
        let data = || {
            serde_json::from_value(json!({"kind":"bug","text":"  <ошибка>  ","page":"invalid"}))
                .unwrap()
        };
        for _ in 0..3 {
            let result = service.submit(&p, INIT, 1_000_000, data()).unwrap();
            assert!(result.text.contains("&lt;ошибка&gt;"));
            assert!(!result.text.contains("Иван"));
            assert!(result.text.contains(identity::animal_identity(42).1));
        }
        assert!(matches!(
            service.submit(&p, INIT, 1_000_000, data()),
            Err(ProfileError::RateLimited(_))
        ));
        let db = p.db.lock().unwrap();
        let (text, page): (String, String) = db
            .query_row("SELECT text,page FROM feedback LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((text.as_str(), page.as_str()), ("<ошибка>", "Приложение"));
    }
}

//! Best-effort notifications contain no private message text.
use crate::{
    bot,
    messages::Sent,
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

#[derive(Debug)]
pub enum NotifyError {
    Database,
    Telegram,
}
pub async fn notify<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    sent: &Sent,
    base: &str,
    now: i64,
) -> Result<(), NotifyError> {
    let existing = {
        let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
        let enabled: Option<(bool, bool)> = db
            .query_row(
                "SELECT notifications_enabled,notify_messages FROM users WHERE telegram_id=?1",
                [sent.recipient],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| NotifyError::Database)?;
        if !matches!(enabled, Some((true, true))) {
            return Ok(());
        }
        db.query_row("SELECT message_id FROM event_notifications WHERE kind='chat' AND entity_id=?1 AND chat_id=?2 AND user_id=?2",params![sent.author,sent.recipient],|r|r.get::<_,i64>(0)).optional().map_err(|_|NotifyError::Database)?.unwrap_or(0)
    };
    let text = if sent.recipient_is_coach {
        "✉️ Новое сообщение в приложении."
    } else {
        "✉️ Новое сообщение от тренера в приложении."
    };
    let markup = json!({"inline_keyboard":[[{"text":"Открыть чат","web_app":{"url":bot::web_app_url(base)}}]]});
    let mut message = 0;
    if existing != 0 {
        let result=bot.request("editMessageText",json!({"chat_id":sent.recipient,"message_id":existing,"text":text,"parse_mode":"HTML","reply_markup":markup})).await;
        if matches!(result, Ok(_) | Err(TelegramError::NotModified)) {
            message = existing;
        }
    }
    if message == 0 {
        let value=bot.request("sendMessage",json!({"chat_id":sent.recipient,"text":text,"parse_mode":"HTML","reply_markup":markup})).await.map_err(|_|NotifyError::Telegram)?;
        message = value
            .pointer("/result/message_id")
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .ok_or(NotifyError::Telegram)?;
    }
    let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
    db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,phase,sent_at,final_at) VALUES('chat',?1,?2,?2,?3,?4,?5,?6,'',?5,NULL) ON CONFLICT(kind,entity_id,chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,text=excluded.text,updated_at=excluded.updated_at,delete_at=CASE WHEN event_notifications.message_id=excluded.message_id AND event_notifications.delete_at IS NOT NULL AND excluded.delete_at IS NOT NULL THEN MIN(event_notifications.delete_at,excluded.delete_at) ELSE excluded.delete_at END,phase=excluded.phase,sent_at=CASE WHEN event_notifications.message_id=excluded.message_id THEN event_notifications.sent_at ELSE excluded.sent_at END,final_at=excluded.final_at",params![sent.author,sent.recipient,message,text,now,now+86400]).map_err(|_|NotifyError::Database)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fake(Mutex<Vec<(String, Value)>>);
    impl BotTransport for Fake {
        async fn request(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
            self.0.lock().unwrap().push((method.into(), payload));
            if method == "editMessageText" {
                Err(TelegramError::NotModified)
            } else {
                Ok(json!({"ok":true,"result":{"message_id":101}}))
            }
        }
    }
    #[tokio::test]
    async fn notifications_respect_preferences_and_never_send_chat_body() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute(
            "INSERT INTO users(telegram_id,created_at) VALUES(123,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO messages(from_user,to_user,text,created_at) VALUES(42,123,'СЕКРЕТНАЯ ФРАЗА',0)",[]).unwrap();
        let profiles = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 123,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let bot = Fake::default();
        let sent = Sent {
            author: 42,
            recipient: 123,
            recipient_is_coach: true,
        };
        notify(&profiles, &bot, &sent, "https://example.test", 1000)
            .await
            .unwrap();
        notify(&profiles, &bot, &sent, "https://example.test", 1001)
            .await
            .unwrap();
        {
            let calls = bot.0.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].0, "sendMessage");
            assert_eq!(calls[1].0, "editMessageText");
            assert!(calls
                .iter()
                .all(|(_, payload)| !payload.to_string().contains("СЕКРЕТНАЯ ФРАЗА")));
            assert_eq!(calls[0].1["text"], "✉️ Новое сообщение в приложении.");
        }
        profiles
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE users SET notify_messages=0 WHERE telegram_id=123",
                [],
            )
            .unwrap();
        notify(&profiles, &bot, &sent, "", 1002).await.unwrap();
        assert_eq!(bot.0.lock().unwrap().len(), 2);
        let db = profiles.db.lock().unwrap();
        let (count, id, deadline): (i64, i64, i64) = db
            .query_row(
                "SELECT count(*),message_id,delete_at FROM event_notifications WHERE kind='chat'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((count, id, deadline), (1, 101, 87400));
    }
}

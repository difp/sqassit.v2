//! Shared editable event notification delivery. The caller decides recipients and consent.
use crate::{
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Edited,
    Same,
}
#[derive(Debug)]
pub enum NotifyError {
    Database,
    Telegram,
}
#[derive(Clone, Default)]
pub struct EventNotifier {
    pub(crate) lock: Arc<tokio::sync::Mutex<()>>,
}
pub struct EventMessage<'a> {
    pub kind: &'a str,
    pub entity: i64,
    pub chat: i64,
    pub user: i64,
    pub text: &'a str,
    pub markup: Option<Value>,
    pub now: i64,
}
impl EventNotifier {
    /// Remove an existing message, retaining the row on retryable transport errors.
    pub async fn delete<T: BotTransport>(
        &self,
        profiles: &Profiles,
        bot: &T,
        row_id: i64,
        expected_message: i64,
        expected_text: &str,
    ) -> Result<(), NotifyError> {
        let _guard = self.lock.lock().await;
        let record = {
            let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
            db.query_row("SELECT chat_id,message_id FROM event_notifications WHERE id=?1 AND message_id=?2 AND text=?3",params![row_id,expected_message,expected_text],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional().map_err(|_|NotifyError::Database)?
        };
        let Some((chat, message)) = record else {
            return Ok(());
        };
        if message != 0
            && matches!(
                bot.request(
                    "deleteMessage",
                    json!({"chat_id":chat,"message_id":message})
                )
                .await,
                Err(TelegramError::Transport | TelegramError::Configuration)
            )
        {
            return Err(NotifyError::Telegram);
        }
        let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
        db.execute(
            "DELETE FROM event_notifications WHERE id=?1 AND chat_id=?2 AND message_id=?3",
            params![row_id, chat, message],
        )
        .map_err(|_| NotifyError::Database)?;
        Ok(())
    }
    pub async fn send<T: BotTransport>(
        &self,
        profiles: &Profiles,
        bot: &T,
        event: EventMessage<'_>,
    ) -> Result<Delivery, NotifyError> {
        // Serialize the read/send/save sequence so concurrent updates do not create duplicates.
        let _guard = self.lock.lock().await;
        let existing = {
            let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
            db.query_row("SELECT message_id,text FROM event_notifications WHERE kind=?1 AND entity_id=?2 AND chat_id=?3 AND user_id=?4",params![event.kind,event.entity,event.chat,event.user],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).optional().map_err(|_|NotifyError::Database)?
        };
        if existing
            .as_ref()
            .is_some_and(|(id, text)| *id != 0 && text == event.text && event.markup.is_none())
        {
            return Ok(Delivery::Same);
        }
        let mut message = 0;
        let mut outcome = Delivery::Sent;
        if let Some((id, _)) = existing.filter(|(id, _)| *id != 0) {
            let mut payload =
                json!({"chat_id":event.chat,"message_id":id,"text":event.text,"parse_mode":"HTML"});
            if let Some(markup) = &event.markup {
                payload["reply_markup"] = markup.clone();
            }
            match bot.request("editMessageText", payload).await {
                Ok(_) => {
                    message = id;
                    outcome = Delivery::Edited;
                }
                Err(TelegramError::NotModified) => {
                    message = id;
                    outcome = Delivery::Same;
                }
                Err(_) => {}
            }
        }
        if message == 0 {
            let mut payload = json!({"chat_id":event.chat,"text":event.text,"parse_mode":"HTML"});
            if let Some(markup) = &event.markup {
                payload["reply_markup"] = markup.clone();
            }
            let result = bot
                .request("sendMessage", payload)
                .await
                .map_err(|_| NotifyError::Telegram)?;
            message = result
                .pointer("/result/message_id")
                .and_then(Value::as_i64)
                .filter(|id| *id > 0)
                .ok_or(NotifyError::Telegram)?;
        }
        let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
        let deadline = if event.kind == "match" || event.kind == "training" {
            let sql = if event.kind == "match" {
                "SELECT start_at,duration_min,deleted_at FROM matches WHERE id=?1"
            } else {
                "SELECT start_at,duration_min,deleted_at FROM trainings WHERE id=?1"
            };
            let item: Option<(i64, i64, Option<i64>)> = db
                .query_row(sql, [event.entity], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()
                .map_err(|_| NotifyError::Database)?;
            match item {
                Some((start, duration, deleted)) if deleted.unwrap_or(0) == 0 => {
                    (start + duration * 60 + 14400).min(event.now + 47 * 3600)
                }
                _ => event.now + 86400,
            }
        } else {
            event.now + 86400
        };
        db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,phase,sent_at,final_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'',?7,NULL) ON CONFLICT(kind,entity_id,chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,text=excluded.text,updated_at=excluded.updated_at,delete_at=CASE WHEN event_notifications.message_id=excluded.message_id AND event_notifications.delete_at IS NOT NULL AND excluded.delete_at IS NOT NULL THEN MIN(event_notifications.delete_at,excluded.delete_at) ELSE excluded.delete_at END,phase=excluded.phase,sent_at=CASE WHEN event_notifications.message_id=excluded.message_id THEN event_notifications.sent_at ELSE excluded.sent_at END,final_at=excluded.final_at",params![event.kind,event.entity,event.chat,event.user,message,event.text,event.now,deadline]).map_err(|_|NotifyError::Database)?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[derive(Default)]
    struct Fake(Mutex<Vec<(String, Value)>>);
    impl BotTransport for Fake {
        async fn request(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
            self.0.lock().unwrap().push((method.into(), payload));
            if method == "sendMessage" {
                Ok(json!({"result":{"message_id":42}}))
            } else {
                Ok(json!({"ok":true}))
            }
        }
    }
    #[tokio::test]
    async fn send_edit_and_deduplicate_concurrent_updates() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO matches(id,start_at,duration_min,created_by,created_at) VALUES(1,10000,60,1,0)",[]).unwrap();
        let profiles = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 1,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        let msg = || EventMessage {
            kind: "match",
            entity: 1,
            chat: 2,
            user: 2,
            text: "Hello",
            markup: None,
            now: 1000,
        };
        let (a, b) = tokio::join!(
            notifier.send(&profiles, &bot, msg()),
            notifier.send(&profiles, &bot, msg())
        );
        assert_eq!(a.unwrap(), Delivery::Sent);
        assert_eq!(b.unwrap(), Delivery::Same);
        assert_eq!(bot.0.lock().unwrap().len(), 1);
        let edited = EventMessage {
            kind: "match",
            entity: 1,
            chat: 2,
            user: 2,
            text: "Changed",
            markup: None,
            now: 2000,
        };
        assert_eq!(
            notifier.send(&profiles, &bot, edited).await.unwrap(),
            Delivery::Edited
        );
        let db = profiles.db.lock().unwrap();
        let (message,text,deadline,sent):(i64,String,i64,i64)=db.query_row("SELECT message_id,text,delete_at,sent_at FROM event_notifications WHERE kind='match'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            (message, text.as_str(), deadline, sent),
            (42, "Changed", 28000, 1000)
        );
    }
}

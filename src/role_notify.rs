//! Best-effort notifications for coach applications and training transfers.
use crate::{
    bot,
    event_notify::{EventMessage, EventNotifier, NotifyError},
    identity,
    profile::Profiles,
    telegram::BotTransport,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

/// Send a fresh temporary notice, as opposed to editing an existing event card.
pub async fn notice<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    chat: i64,
    text: &str,
    markup: Option<Value>,
    now: i64,
) -> Result<(), NotifyError> {
    let mut payload = json!({"chat_id":chat,"text":text,"parse_mode":"HTML"});
    if let Some(markup) = markup {
        payload["reply_markup"] = markup;
    }
    let sent = bot
        .request("sendMessage", payload)
        .await
        .map_err(|_| NotifyError::Telegram)?;
    let id = sent
        .pointer("/result/message_id")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .ok_or(NotifyError::Telegram)?;
    let db = p.db.lock().map_err(|_| NotifyError::Database)?;
    db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,sent_at) VALUES('notice',?1,?2,?2,?1,?3,?4,?5,?4) ON CONFLICT(kind,entity_id,chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,text=excluded.text,updated_at=excluded.updated_at,delete_at=excluded.delete_at",params![id,chat,text,now,now+86400]).map_err(|_|NotifyError::Database)?;
    Ok(())
}
pub async fn new_coach<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    base: &str,
    id: i64,
    now: i64,
) -> Result<(), NotifyError> {
    let markup = if base.is_empty() {
        None
    } else {
        Some(
            json!({"inline_keyboard":[[{"text":"🏸 Открыть тренировки","web_app":{"url":bot::web_app_url(base)}}]]}),
        )
    };
    notice(
        p,
        bot,
        id,
        "Вам выдан доступ тренера 🏸\nОткройте приложение — появится вкладка «Тренер».",
        markup,
        now,
    )
    .await
}
pub async fn rejected<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    id: i64,
    now: i64,
) -> Result<(), NotifyError> {
    notice(p, bot, id, "Ваша заявка на тренера отклонена.", None, now).await
}
pub async fn transferred<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    id: i64,
    training: i64,
    now: i64,
) -> Result<(), NotifyError> {
    let info = {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        let allowed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE telegram_id=?1 AND notifications_enabled=1 AND notify_trainings=1)",[id],|r|r.get(0)).map_err(|_|NotifyError::Database)?;
        if !allowed {
            return Ok(());
        }
        db.query_row(
            "SELECT training_type,start_at FROM trainings WHERE id=?1",
            [training],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|_| NotifyError::Database)?
    };
    let Some((typ, start)) = info else {
        return Ok(());
    };
    let date = DateTime::<Utc>::from_timestamp(start, 0)
        .ok_or(NotifyError::Database)?
        .with_timezone(&p.timezone);
    let text = format!(
        "Вам передали занятие.\n\n🏸 {} · {}",
        bot::escape(if typ.is_empty() {
            "Тренировка"
        } else {
            &typ
        }),
        date.format("%d.%m.%Y в %H:%M")
    );
    notice(p, bot, id, &text, None, now).await
}
pub async fn application<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    base: &str,
    id: i64,
    now: i64,
) -> Result<(), NotifyError> {
    let (name, recipients) = {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        let data:Option<(Option<String>,String)>=db.query_row("SELECT u.first_name,u.display_mode FROM coach_applications a JOIN users u ON u.telegram_id=a.telegram_id WHERE a.telegram_id=?1 AND a.status='pending'",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|NotifyError::Database)?;
        let Some((name, mode)) = data else {
            return Ok(());
        };
        let name = if mode == "animal" {
            identity::animal_identity(id).1.to_owned()
        } else {
            name.filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Пользователь".into())
        };
        let mut stmt=db.prepare("SELECT u.telegram_id FROM users u WHERE u.notifications_enabled=1 AND u.notify_messages=1 AND (u.telegram_id=?1 OR EXISTS(SELECT 1 FROM coaches c WHERE c.telegram_id=u.telegram_id AND c.is_manager=1))").map_err(|_|NotifyError::Database)?;
        let rows = stmt
            .query_map([p.owner], |r| r.get::<_, i64>(0))
            .map_err(|_| NotifyError::Database)?;
        let recipients = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| NotifyError::Database)?;
        (name, recipients)
    };
    let text = format!("📝 <b>Заявка в тренеры</b>\n{}", bot::escape(&name));
    let markup = json!({"inline_keyboard":[[{"text":"Открыть заявки","web_app":{"url":bot::web_app_url(base)}}]]});
    let mut failed = false;
    for chat in recipients {
        // Skip delayed work after a decision or a preference change.
        let allowed = {
            let db = p.db.lock().map_err(|_| NotifyError::Database)?;
            db.query_row("SELECT EXISTS(SELECT 1 FROM coach_applications a JOIN users u ON u.telegram_id=?2 WHERE a.telegram_id=?1 AND a.status='pending' AND u.notifications_enabled=1 AND u.notify_messages=1)",params![id,chat],|r|r.get::<_,bool>(0)).map_err(|_|NotifyError::Database)?
        };
        if !allowed {
            continue;
        }
        if notifier
            .send(
                p,
                bot,
                EventMessage {
                    kind: "coach_application",
                    entity: id,
                    chat,
                    user: chat,
                    text: &text,
                    markup: Some(markup.clone()),
                    now,
                },
            )
            .await
            .is_err()
        {
            failed = true;
        }
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        db.execute("UPDATE event_notifications SET delete_at=?1 WHERE kind='coach_application' AND entity_id=?2 AND chat_id=?3 AND NOT EXISTS(SELECT 1 FROM coach_applications WHERE telegram_id=?2 AND status='pending')",params![now,id,chat]).map_err(|_|NotifyError::Database)?;
    }
    if failed {
        Err(NotifyError::Telegram)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rate_limit::RateLimiter, telegram::TelegramError};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    #[derive(Default)]
    struct Fake(Mutex<Vec<Value>>);
    impl BotTransport for Fake {
        async fn request(&self, _: &str, value: Value) -> Result<Value, TelegramError> {
            let mut calls = self.0.lock().unwrap();
            calls.push(value);
            Ok(json!({"result":{"message_id":calls.len() as i64}}))
        }
    }
    #[tokio::test]
    async fn application_and_transfer_respect_preferences_and_anonymity() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO users(telegram_id,first_name,display_mode,notify_messages,notify_trainings,created_at) VALUES(1,'Owner','name',1,1,0),(2,'Secret <Name>','animal',1,1,0),(3,'Manager','name',0,1,0),(4,'Coach','name',1,0,0); INSERT INTO coaches(telegram_id,is_manager,created_at) VALUES(3,1,0); INSERT INTO coach_applications(telegram_id,status,created_at) VALUES(2,'pending',0); INSERT INTO trainings(id,title,training_type,start_at,created_by,created_at) VALUES(7,'x','<Squash>',2000,1,0);").unwrap();
        let p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 1,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(RateLimiter::new(60, Duration::from_secs(60)))),
        };
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        application(&p, &bot, &notifier, "https://example.test", 2, 1000)
            .await
            .unwrap();
        {
            let calls = bot.0.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0]["chat_id"], 1);
            let text = calls[0]["text"].as_str().unwrap();
            assert!(text.contains(identity::animal_identity(2).1));
            assert!(!text.contains("Secret"));
        }
        transferred(&p, &bot, 4, 7, 1001).await.unwrap();
        assert_eq!(bot.0.lock().unwrap().len(), 1);
        transferred(&p, &bot, 2, 7, 1001).await.unwrap();
        assert!(bot.0.lock().unwrap()[1]["text"]
            .as_str()
            .unwrap()
            .contains("&lt;Squash&gt;"));
        new_coach(&p, &bot, "https://example.test", 2, 1002)
            .await
            .unwrap();
        rejected(&p, &bot, 2, 1003).await.unwrap();
        let db = p.db.lock().unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM event_notifications WHERE kind='notice' AND delete_at IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),3);
    }
}

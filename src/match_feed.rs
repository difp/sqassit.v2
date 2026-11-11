//! Best-effort editable match feed, following the legacy recipient and expiry rules.
use crate::{
    bot,
    event_notify::{EventMessage, EventNotifier, NotifyError},
    profile::Profiles,
    telegram::BotTransport,
};
use chrono::{DateTime, Utc};
use rusqlite::params;

fn content(profiles: &Profiles, now: i64, until: i64) -> Result<(String, bool), NotifyError> {
    let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
    let mut stmt=db.prepare("SELECT title,location,start_at FROM matches WHERE start_at>=?1 AND start_at<=?2 AND deleted_at IS NULL ORDER BY start_at ASC LIMIT 10").map_err(|_|NotifyError::Database)?;
    let rows = stmt
        .query_map(params![now, until], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| NotifyError::Database)?;
    let mut lines = vec!["🏸 <b>Ближайшие матчи</b>".to_owned()];
    for row in rows {
        let (title, location, start) = row.map_err(|_| NotifyError::Database)?;
        let local = DateTime::<Utc>::from_timestamp(start, 0)
            .ok_or(NotifyError::Database)?
            .with_timezone(&profiles.timezone);
        let mut line = format!(
            "• {} — {}",
            local.format("%d.%m в %H:%M"),
            bot::escape(if title.is_empty() { "Матч" } else { &title })
        );
        if !location.is_empty() {
            line.push_str(&format!(" · {}", bot::escape(&location)));
        }
        lines.push(line);
    }
    let any = lines.len() > 1;
    Ok((
        if any {
            lines.join("\n")
        } else {
            "🏸 Ближайших матчей пока нет.".into()
        },
        any,
    ))
}
/// Publish to opted-in users, except the creator who receives a personal match card.
pub async fn publish<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    exclude: i64,
    now: i64,
) -> Result<(), NotifyError> {
    let until = crate::trainings::end_of_window(now, 30, profiles.timezone)
        .map_err(|_| NotifyError::Database)?;
    let (text, _) = content(profiles, now, until)?;
    let recipients = {
        let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
        let mut stmt=db.prepare("SELECT telegram_id FROM users WHERE notifications_enabled=1 AND notify_matches=1 AND telegram_id!=?1").map_err(|_|NotifyError::Database)?;
        let rows = stmt
            .query_map([exclude], |r| r.get::<_, i64>(0))
            .map_err(|_| NotifyError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| NotifyError::Database)?
    };
    let mut failed = false;
    for user in recipients {
        // Recheck settings immediately before each delivery, since a fan-out can take time.
        let allowed = {
            let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
            db.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE telegram_id=?1 AND notifications_enabled=1 AND notify_matches=1)",[user],|r|r.get::<_,bool>(0)).map_err(|_|NotifyError::Database)?
        };
        if allowed
            && notifier
                .send(
                    profiles,
                    bot,
                    EventMessage {
                        kind: "match_feed",
                        entity: 0,
                        chat: user,
                        user,
                        text: &text,
                        markup: None,
                        now,
                    },
                )
                .await
                .is_err()
        {
            failed = true;
        }
    }
    if failed {
        Err(NotifyError::Telegram)
    } else {
        Ok(())
    }
}
/// Refresh only existing feeds; remove them when no matches remain or consent is withdrawn.
pub async fn refresh<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    now: i64,
) -> Result<(), NotifyError> {
    let (text, any) = content(profiles, now, now + 30 * 86400)?;
    let records = {
        let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
        let mut stmt=db.prepare("SELECT n.id,n.chat_id,n.user_id,n.message_id,n.text,COALESCE(u.notifications_enabled AND u.notify_matches,0) FROM event_notifications n LEFT JOIN users u ON u.telegram_id=n.chat_id WHERE n.kind='match_feed'").map_err(|_|NotifyError::Database)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, bool>(5)?,
                ))
            })
            .map_err(|_| NotifyError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| NotifyError::Database)?
    };
    let mut failed = false;
    for (id, chat, user, message, old, allowed) in records {
        let result = if !any || !allowed {
            notifier
                .delete(profiles, bot, id, message, &old)
                .await
                .map(|_| ())
        } else if old != text {
            notifier
                .send(
                    profiles,
                    bot,
                    EventMessage {
                        kind: "match_feed",
                        entity: 0,
                        chat,
                        user,
                        text: &text,
                        markup: None,
                        now,
                    },
                )
                .await
                .map(|_| ())
        } else {
            Ok(())
        };
        if result.is_err() {
            failed = true;
        }
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
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
        fail_delete: Mutex<bool>,
    }
    impl BotTransport for Fake {
        async fn request(&self, method: &str, value: Value) -> Result<Value, TelegramError> {
            self.calls.lock().unwrap().push((method.into(), value));
            if method == "deleteMessage" && *self.fail_delete.lock().unwrap() {
                return Err(TelegramError::Transport);
            }
            Ok(json!({"result":{"message_id":42}}))
        }
    }
    #[tokio::test]
    async fn feed_excludes_creator_respects_preferences_and_retries_deletion() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO users(telegram_id,notify_matches,created_at) VALUES(1,1,0),(2,1,0),(3,0,0)",[]).unwrap();
        db.execute("INSERT INTO matches(id,title,location,start_at,created_by,created_at) VALUES(1,'<Match>','A&B',2000,1,0)",[]).unwrap();
        let p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 1,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        publish(&p, &bot, &notifier, 1, 1000).await.unwrap();
        {
            let calls = bot.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].1["chat_id"], 2);
            assert!(calls[0].1["text"]
                .as_str()
                .unwrap()
                .contains("&lt;Match&gt; · A&amp;B"));
        }
        p.db.lock()
            .unwrap()
            .execute("UPDATE matches SET title='Changed' WHERE id=1", [])
            .unwrap();
        refresh(&p, &bot, &notifier, 1001).await.unwrap();
        assert_eq!(bot.calls.lock().unwrap()[1].0, "editMessageText");
        *bot.fail_delete.lock().unwrap() = true;
        assert!(refresh(&p, &bot, &notifier, 2001).await.is_err());
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='match_feed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        *bot.fail_delete.lock().unwrap() = false;
        refresh(&p, &bot, &notifier, 2002).await.unwrap();
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='match_feed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        p.db.lock()
            .unwrap()
            .execute("UPDATE matches SET start_at=4000 WHERE id=1", [])
            .unwrap();
        publish(&p, &bot, &notifier, 1, 2003).await.unwrap();
        p.db.lock()
            .unwrap()
            .execute(
                "UPDATE users SET notifications_enabled=0 WHERE telegram_id=2",
                [],
            )
            .unwrap();
        refresh(&p, &bot, &notifier, 2004).await.unwrap();
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='match_feed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
}

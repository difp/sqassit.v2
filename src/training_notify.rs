//! Preview training cards for booking changes; status-phase scheduling remains incomplete.
use crate::{
    bot,
    event_notify::{EventMessage, EventNotifier, NotifyError},
    identity,
    profile::Profiles,
    telegram::BotTransport,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::json;

fn status_text(status: &str) -> &'static str {
    match status {
        "pending" => "Заявка на занятие отправлена.",
        "booked" | "promoted" => "Вы записаны.",
        "waitlist" => {
            "Заявка на занятие одобрена, но пока нет мест,\nзаписали вас в лист ожидания."
        }
        "rejected" => "Заявка на занятие отклонена — к сожалению, мест нет.",
        "cancelled" => "Тренер снял вас с тренировки.",
        "deleted" => "Тренировка отменена.",
        _ => "Запись обновлена.",
    }
}
struct Snapshot {
    player_header: String,
    manager_header: String,
    summary: String,
    roster: String,
    date: String,
    managers: Vec<i64>,
    pending: i64,
}
fn snapshot(p: &Profiles, id: i64) -> Result<Option<Snapshot>, NotifyError> {
    let db = p.db.lock().map_err(|_| NotifyError::Database)?;
    type TrainingRow = (String, String, i64, Option<i64>, Option<i64>);
    let event: Option<TrainingRow> = db
        .query_row(
            "SELECT training_type,level,start_at,created_by,deleted_at FROM trainings WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|_| NotifyError::Database)?;
    let Some((typ, level, start, owner, deleted)) = event else {
        return Ok(None);
    };
    let coach_name: Option<String> = if let Some(owner) = owner {
        db.query_row(
            "SELECT first_name FROM users WHERE telegram_id=?1",
            [owner],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|_| NotifyError::Database)?
        .flatten()
    } else {
        None
    };
    let coach = coach_name.as_deref().unwrap_or("").trim();
    let typ = if typ.is_empty() {
        "Тренировка"
    } else {
        &typ
    };
    let mut player_header = typ.to_owned();
    if !level.is_empty() {
        player_header.push_str(&format!(": {level}"));
    }
    if !coach.is_empty() {
        player_header.push_str(&format!(". Тренер: {coach}"));
    }
    let details = [level.as_str(), coach]
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(" - ");
    let manager_header = if details.is_empty() {
        typ.to_owned()
    } else {
        format!("{typ}: {details}")
    };
    let date = DateTime::<Utc>::from_timestamp(start, 0)
        .ok_or(NotifyError::Database)?
        .with_timezone(&p.timezone)
        .format("%d.%m / %H:%M")
        .to_string();
    let mut stmt=db.prepare("SELECT b.user_id,b.status,u.first_name,u.display_mode FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=?1 AND b.status IN ('pending','booked','waitlist') ORDER BY CASE b.status WHEN 'booked' THEN 0 ELSE 1 END,b.position ASC").map_err(|_|NotifyError::Database)?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|_| NotifyError::Database)?;
    let mut names = Vec::new();
    let mut pending = 0;
    for row in rows {
        let (id, status, name, mode) = row.map_err(|_| NotifyError::Database)?;
        if status == "pending" {
            pending += 1;
        }
        if status == "booked" {
            names.push(if mode == "animal" {
                identity::animal_identity(id).1.to_owned()
            } else {
                name.filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Игрок".into())
            });
        }
    }
    let mut roster = String::from("Записаны:");
    if names.is_empty() {
        roster.push_str("\nПока никто не записан");
    } else {
        for (i, name) in names.iter().take(25).enumerate() {
            roster.push_str(&format!("\n{}. {}", i + 1, bot::escape(name)));
        }
        if names.len() > 25 {
            roster.push_str("\n…");
        }
    }
    let mut summary = format!("{}\n{date}\n{roster}", bot::escape(&manager_header));
    if deleted.unwrap_or(0) != 0 {
        summary.push_str("\nТренировка отменена");
    }
    let mut managers = Vec::new();
    if let Some(owner) = owner {
        managers.push(owner);
    }
    let mut stmt=db.prepare("SELECT user_id FROM event_coauthors WHERE event_type='training' AND event_id=?1 ORDER BY created_at").map_err(|_|NotifyError::Database)?;
    let rows = stmt
        .query_map([id], |r| r.get::<_, i64>(0))
        .map_err(|_| NotifyError::Database)?;
    for row in rows {
        let manager = row.map_err(|_| NotifyError::Database)?;
        if !managers.contains(&manager) {
            managers.push(manager);
        }
    }
    Ok(Some(Snapshot {
        player_header: bot::escape(&player_header),
        manager_header: bot::escape(&manager_header),
        summary,
        roster,
        date,
        managers,
        pending,
    }))
}
fn allowed(p: &Profiles, id: i64) -> Result<bool, NotifyError> {
    let db = p.db.lock().map_err(|_| NotifyError::Database)?;
    db.query_row(
        "SELECT notifications_enabled AND notify_trainings FROM users WHERE telegram_id=?1",
        [id],
        |r| r.get::<_, bool>(0),
    )
    .optional()
    .map_err(|_| NotifyError::Database)
    .map(|v| v.unwrap_or(false))
}
pub struct TrainingChange<'a> {
    pub training: i64,
    pub user: i64,
    pub status: &'a str,
    pub base: &'a str,
    pub now: i64,
    pub actor: Option<i64>,
}
/// Sends editable cards and selected alerts. The legacy status-card lifecycle remains incomplete.
pub async fn notify<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    change: TrainingChange<'_>,
) -> Result<(), NotifyError> {
    let TrainingChange {
        training,
        user,
        status,
        base,
        now,
        actor,
    } = change;
    let Some(s) = snapshot(p, training)? else {
        return Ok(());
    };
    if !matches!(status, "booked" | "promoted") {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        db.execute("UPDATE event_notifications SET delete_at=?1 WHERE kind='training_reminder' AND entity_id=?2 AND chat_id=?3",params![now,training,user]).map_err(|_|NotifyError::Database)?;
    }
    let personal = format!(
        "{}\n\n{}\n{}\n{}",
        status_text(status),
        s.player_header,
        s.date,
        s.roster
    );
    let markup = if s.pending > 0 {
        Some(
            json!({"inline_keyboard":[[{"text":format!("Открыть заявки · {}",s.pending),"web_app":{"url":bot::web_app_url(base)}}]]}),
        )
    } else {
        None
    };
    let mut failed = false;
    if allowed(p, user)?
        && notifier
            .send(
                p,
                bot,
                EventMessage {
                    kind: "training",
                    entity: training,
                    chat: user,
                    user,
                    text: &personal,
                    markup: None,
                    now,
                },
            )
            .await
            .is_err()
    {
        failed = true;
    }
    // Refresh existing participant cards without creating new messages for others.
    let existing = {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        let mut stmt=db.prepare("SELECT b.user_id,b.status FROM bookings b JOIN event_notifications n ON n.kind='training' AND n.entity_id=b.training_id AND n.chat_id=b.user_id AND n.user_id=b.user_id WHERE b.training_id=?1 AND b.user_id!=?2 AND b.status IN ('pending','booked','waitlist')").map_err(|_|NotifyError::Database)?;
        let rows = stmt
            .query_map(params![training, user], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|_| NotifyError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| NotifyError::Database)?
    };
    for (other, other_status) in existing {
        if allowed(p, other)? {
            let text = format!(
                "{}\n\n{}\n{}\n{}",
                status_text(&other_status),
                s.player_header,
                s.date,
                s.roster
            );
            if notifier
                .send(
                    p,
                    bot,
                    EventMessage {
                        kind: "training",
                        entity: training,
                        chat: other,
                        user: other,
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
    }
    let actor_name = if let Some(actor) = actor {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        let value: Option<(Option<String>, String)> = db
            .query_row(
                "SELECT first_name,display_mode FROM users WHERE telegram_id=?1",
                [actor],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| NotifyError::Database)?;
        Some(match value {
            Some((_, mode)) if mode == "animal" => identity::animal_identity(actor).1.to_owned(),
            Some((name, _)) => name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Игрок".into()),
            None => "Игрок".into(),
        })
    } else {
        None
    };
    for manager in s.managers {
        if allowed(p, manager)? {
            let delivery = notifier
                .send(
                    p,
                    bot,
                    EventMessage {
                        kind: "training",
                        entity: training,
                        chat: manager,
                        user: 0,
                        text: &s.summary,
                        markup: markup.clone(),
                        now,
                    },
                )
                .await;
            let edited = matches!(delivery, Ok(crate::event_notify::Delivery::Edited));
            if delivery.is_err() {
                failed = true;
            }
            if let Some(name) = actor_name.as_deref().filter(|_| actor != Some(manager)) {
                if edited || matches!(status, "pending" | "waitlist" | "cancelled" | "rejected") {
                    let action = match status {
                        "pending" => "Новая заявка",
                        "booked" => "Новая запись",
                        "waitlist" => "В лист ожидания",
                        "cancelled" => "Отмена записи",
                        "rejected" => "Заявка отклонена",
                        "promoted" => "Место освободилось",
                        _ => "Изменение записи",
                    };
                    let alert = format!(
                        "{action}: {}\n{}\n{}",
                        bot::escape(name),
                        s.manager_header,
                        s.date
                    );
                    if crate::role_notify::notice(p, bot, manager, &alert, markup.clone(), now)
                        .await
                        .is_err()
                    {
                        failed = true;
                    }
                }
            }
        }
    }
    // Telegram edits do not produce a push. Send a separate temporary notice on promotion.
    if (status == "promoted"
        || (actor.is_some_and(|a| a != user)
            && matches!(status, "booked" | "waitlist" | "rejected" | "cancelled")))
        && allowed(p, user)?
        && crate::role_notify::notice(p, bot, user, &personal, None, now)
            .await
            .is_err()
    {
        failed = true;
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
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fake(Mutex<Vec<(String, Value)>>);
    impl BotTransport for Fake {
        async fn request(&self, method: &str, value: Value) -> Result<Value, TelegramError> {
            self.0.lock().unwrap().push((method.into(), value));
            Ok(json!({"result":{"message_id":123}}))
        }
    }
    #[tokio::test]
    async fn existing_cards_refresh_and_coach_receives_anonymous_alert() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO users(telegram_id,first_name,display_mode,created_at) VALUES(1,'Coach','name',0),(2,'Private','animal',0),(4,'Other','name',0); INSERT INTO trainings(id,title,training_type,start_at,created_at,created_by) VALUES(1,'t','Squash',2000,0,1); INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(1,2,'booked',0),(1,4,'booked',1); INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at) VALUES('training',1,1,0,10,'old',0,9999),('training',1,4,4,11,'old',0,9999);").unwrap();
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
        notify(
            &p,
            &bot,
            &notifier,
            TrainingChange {
                training: 1,
                user: 2,
                status: "booked",
                actor: Some(2),
                base: "https://example.test",
                now: 1000,
            },
        )
        .await
        .unwrap();
        let calls = bot.0.lock().unwrap();
        assert!(calls.iter().any(|(method, v)| method == "editMessageText"
            && v["chat_id"] == 4
            && v["text"]
                .as_str()
                .unwrap()
                .contains(identity::animal_identity(2).1)));
        assert!(calls.iter().any(|(method, v)| method == "sendMessage"
            && v["chat_id"] == 1
            && v["text"].as_str().unwrap().contains("Новая запись")));
        assert!(calls
            .iter()
            .all(|(_, v)| !v.to_string().contains("Private")));
    }
    #[tokio::test]
    async fn cards_respect_privacy_preferences_and_expire_reminders() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO users(telegram_id,first_name,display_mode,notify_trainings,created_at) VALUES(1,'Coach','name',1,0),(2,'Secret','animal',1,0),(3,'Hidden','name',0,0); INSERT INTO trainings(id,title,training_type,level,start_at,created_at,created_by) VALUES(1,'test','<Squash>','',2000,0,1); INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(1,2,'booked',0),(1,3,'pending',1); INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at) VALUES('training_reminder',1,2,2,55,'',0,9999);").unwrap();
        let p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 1,
            timezone: chrono_tz::UTC,
            require_event_approval: true,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        notify(
            &p,
            &bot,
            &notifier,
            TrainingChange {
                training: 1,
                user: 2,
                status: "promoted",
                actor: None,
                base: "https://example.test",
                now: 1000,
            },
        )
        .await
        .unwrap();
        {
            let calls = bot.0.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0].1["chat_id"], 2);
            assert_eq!(calls[1].1["chat_id"], 1);
            assert_eq!(calls[2].1["chat_id"], 2);
            let text = calls[1].1["text"].as_str().unwrap();
            assert!(text.contains("&lt;Squash&gt;"));
            assert!(text.contains(identity::animal_identity(2).1));
            assert!(!text.contains("Secret"));
            assert_eq!(
                calls[1].1["reply_markup"]["inline_keyboard"][0][0]["text"],
                "Открыть заявки · 1"
            );
        }
        p.db.lock()
            .unwrap()
            .execute(
                "UPDATE bookings SET status='cancelled' WHERE training_id=1 AND user_id=2",
                [],
            )
            .unwrap();
        notify(
            &p,
            &bot,
            &notifier,
            TrainingChange {
                training: 1,
                user: 2,
                status: "cancelled",
                actor: None,
                base: "https://example.test",
                now: 1001,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT delete_at FROM event_notifications WHERE kind='training_reminder'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1001
        );
        let count = bot.0.lock().unwrap().len();
        notify(
            &p,
            &bot,
            &notifier,
            TrainingChange {
                training: 1,
                user: 3,
                status: "pending",
                actor: None,
                base: "https://example.test",
                now: 1002,
            },
        )
        .await
        .unwrap();
        assert!(bot.0.lock().unwrap()[count..]
            .iter()
            .all(|(_, payload)| payload["chat_id"] != 3));
    }
}

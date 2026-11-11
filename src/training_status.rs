//! Temporary training-status messages and their preliminary/final lifecycle.
use crate::{
    event_notify::{EventNotifier, NotifyError},
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
    training_notify,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

#[derive(Clone)]
struct Record {
    id: i64,
    kind: String,
    message: i64,
    phase: String,
    final_at: Option<i64>,
}
fn records(p: &Profiles, training: i64, user: i64) -> Result<Vec<Record>, NotifyError> {
    let db = p.db.lock().map_err(|_| NotifyError::Database)?;
    let mut stmt=db.prepare("SELECT id,kind,message_id,phase,final_at FROM event_notifications WHERE entity_id=?1 AND chat_id=?2 AND user_id=?2 AND kind IN ('training_booked','training_waitlist','training_rejected','training_reminder')").map_err(|_|NotifyError::Database)?;
    let rows = stmt
        .query_map(params![training, user], |r| {
            Ok(Record {
                id: r.get(0)?,
                kind: r.get(1)?,
                message: r.get(2)?,
                phase: r.get(3)?,
                final_at: r.get(4)?,
            })
        })
        .map_err(|_| NotifyError::Database)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| NotifyError::Database)
}
fn state(p: &Profiles, training: i64, user: i64) -> Result<Option<(String, bool)>, NotifyError> {
    let db = p.db.lock().map_err(|_| NotifyError::Database)?;
    db.query_row("SELECT b.status,u.notifications_enabled AND u.notify_trainings FROM bookings b JOIN users u ON u.telegram_id=b.user_id JOIN trainings t ON t.id=b.training_id WHERE b.training_id=?1 AND b.user_id=?2 AND t.deleted_at IS NULL",params![training,user],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|NotifyError::Database)
}
async fn remove<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    chat: i64,
    row: &Record,
) -> Result<(), NotifyError> {
    if row.message != 0
        && matches!(
            bot.request(
                "deleteMessage",
                json!({"chat_id":chat,"message_id":row.message})
            )
            .await,
            Err(TelegramError::Transport | TelegramError::Configuration)
        )
    {
        return Err(NotifyError::Telegram);
    }
    let db = p.db.lock().map_err(|_| NotifyError::Database)?;
    db.execute(
        "DELETE FROM event_notifications WHERE id=?1 AND message_id=?2",
        params![row.id, row.message],
    )
    .map_err(|_| NotifyError::Database)?;
    Ok(())
}
fn booked_text(s: &training_notify::Snapshot, preliminary: bool) -> String {
    let suffix = if preliminary {
        "<i>Это сообщение будет удалено.\nАктуальную информацию пришлём за день до занятия.</i>"
    } else {
        "<i>Это сообщение будет автоматически удалено\nчерез 4 часа после окончания занятия.</i>"
    };
    format!(
        "Вы записаны.\n\n{}\n{}\n{}\n\n{}",
        s.player_header, s.date, s.roster, suffix
    )
}
fn status_text(s: &training_notify::Snapshot, status: &str) -> String {
    let suffix = if status == "rejected" {
        "\n\n<i>Это сообщение будет автоматически очищено.</i>"
    } else {
        ""
    };
    format!(
        "{}\n\n{}\n{}{suffix}",
        training_notify::status_text(status),
        s.player_header,
        s.date
    )
}
struct StatusMessage<'a> {
    training: i64,
    user: i64,
    kind: &'a str,
    text: &'a str,
    phase: &'a str,
    deadline: i64,
    final_at: Option<i64>,
    now: i64,
}
async fn send_new<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    entry: StatusMessage<'_>,
) -> Result<i64, NotifyError> {
    let StatusMessage {
        training,
        user,
        kind,
        text,
        phase,
        deadline,
        final_at,
        now,
    } = entry;
    let sent = bot
        .request(
            "sendMessage",
            json!({"chat_id":user,"text":text,"parse_mode":"HTML"}),
        )
        .await
        .map_err(|_| NotifyError::Telegram)?;
    let message = sent
        .pointer("/result/message_id")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .ok_or(NotifyError::Telegram)?;
    let db = p.db.lock().map_err(|_| NotifyError::Database)?;
    db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,phase,sent_at,final_at) VALUES(?1,?2,?3,?3,?4,?5,?6,?7,?8,?6,?9) ON CONFLICT(kind,entity_id,chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,text=excluded.text,updated_at=excluded.updated_at,delete_at=excluded.delete_at,phase=excluded.phase,sent_at=excluded.sent_at,final_at=excluded.final_at",params![kind,training,user,message,text,now,deadline,phase,final_at]).map_err(|_|NotifyError::Database)?;
    Ok(message)
}
/// Reconcile the target user's status. Serialized with other event sends in this process.
pub async fn ensure<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    training: i64,
    user: i64,
    expected: &str,
    now: i64,
) -> Result<(), NotifyError> {
    let _guard = notifier.lock.lock().await;
    let current = state(p, training, user)?;
    let expected = if expected == "promoted" {
        "booked"
    } else {
        expected
    };
    if current.as_ref().map(|v| v.0.as_str()) != Some(expected) {
        return Ok(());
    }
    let Some(s) = training_notify::snapshot(p, training)? else {
        return Ok(());
    };
    let existing = records(p, training, user)?;
    let target = if s.start <= now {
        None
    } else {
        match expected {
            "booked" => Some("training_booked"),
            "waitlist" => Some("training_waitlist"),
            "rejected" => Some("training_rejected"),
            _ => None,
        }
    };
    let mut failed = false;
    for row in &existing {
        if Some(row.kind.as_str()) != target
            && !(row.kind == "training_reminder" && expected == "booked" && s.start > now)
            && remove(p, bot, user, row).await.is_err()
        {
            failed = true;
        }
    }
    let Some(kind) = target else {
        return if failed {
            Err(NotifyError::Telegram)
        } else {
            Ok(())
        };
    };
    if !current.is_some_and(|v| v.1) {
        return if failed {
            Err(NotifyError::Telegram)
        } else {
            Ok(())
        };
    };
    if existing.iter().any(|row| row.kind == kind) {
        return if failed {
            Err(NotifyError::Telegram)
        } else {
            Ok(())
        };
    };
    let end = s
        .start
        .saturating_add(s.duration.saturating_mul(60))
        .saturating_add(14400);
    let preliminary =
        kind == "training_booked" && (s.start - now >= 2 * 86400 || end - now > 47 * 3600);
    let (text, phase, deadline, final_at) = if kind == "training_booked" {
        let final_at = preliminary.then_some(s.start - 86400);
        (
            booked_text(&s, preliminary),
            if preliminary { "preliminary" } else { "final" },
            if preliminary {
                (now + 86400).min(s.start - 86400)
            } else {
                end
            },
            final_at,
        )
    } else {
        (
            status_text(&s, expected),
            "status",
            end.min(now + 47 * 3600),
            None,
        )
    };
    if send_new(
        p,
        bot,
        StatusMessage {
            training,
            user,
            kind,
            text: &text,
            phase,
            deadline,
            final_at,
            now,
        },
    )
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

/// Process due status messages. Transient failures preserve rows for retry.
pub async fn tick<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    now: i64,
) -> Result<(), NotifyError> {
    let ids = {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        let mut stmt=db.prepare("SELECT id FROM event_notifications WHERE kind IN ('training_booked','training_waitlist','training_rejected') AND ((delete_at IS NOT NULL AND delete_at<=?1) OR (kind='training_booked' AND phase='preliminary' AND final_at IS NOT NULL AND final_at<=?1)) ORDER BY id").map_err(|_|NotifyError::Database)?;
        let rows = stmt
            .query_map([now], |r| r.get::<_, i64>(0))
            .map_err(|_| NotifyError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| NotifyError::Database)?
    };
    let mut failed = false;
    for id in ids {
        if process_due(p, bot, notifier, id, now).await.is_err() {
            failed = true;
        }
    }
    if failed {
        Err(NotifyError::Telegram)
    } else {
        Ok(())
    }
}

/// Update existing status messages after the roster or training details change.
pub async fn refresh<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    training: i64,
    now: i64,
) -> Result<(), NotifyError> {
    let _guard = notifier.lock.lock().await;
    let Some(s) = training_notify::snapshot(p, training)? else {
        return Ok(());
    };
    let rows = {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        let mut stmt=db.prepare("SELECT n.id,n.kind,n.message_id,n.phase,n.text,n.chat_id,b.status FROM event_notifications n JOIN bookings b ON b.training_id=n.entity_id AND b.user_id=n.user_id JOIN users u ON u.telegram_id=n.chat_id WHERE n.entity_id=?1 AND n.kind IN ('training_booked','training_waitlist','training_rejected') AND n.message_id!=0 AND u.notifications_enabled=1 AND u.notify_trainings=1").map_err(|_|NotifyError::Database)?;
        let rows = stmt
            .query_map([training], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(|_| NotifyError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| NotifyError::Database)?
    };
    let mut failed = false;
    for (id, kind, message, phase, old, chat, status) in rows {
        let text = match (kind.as_str(), status.as_str()) {
            ("training_booked", "booked") => booked_text(&s, phase == "preliminary"),
            ("training_waitlist", "waitlist") => status_text(&s, "waitlist"),
            ("training_rejected", "rejected") => status_text(&s, "rejected"),
            _ => continue,
        };
        if text == old {
            continue;
        }
        let result = bot
            .request(
                "editMessageText",
                json!({"chat_id":chat,"message_id":message,"text":text,"parse_mode":"HTML"}),
            )
            .await;
        if !matches!(result, Ok(_) | Err(TelegramError::NotModified)) {
            failed = true;
            continue;
        }
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        db.execute("UPDATE event_notifications SET text=?1,updated_at=?2 WHERE id=?3 AND message_id=?4 AND text=?5",params![text,now,id,message,old]).map_err(|_|NotifyError::Database)?;
    }
    if failed {
        Err(NotifyError::Telegram)
    } else {
        Ok(())
    }
}
async fn process_due<T: BotTransport>(
    p: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    id: i64,
    now: i64,
) -> Result<(), NotifyError> {
    let _guard = notifier.lock.lock().await;
    let row = {
        let db = p.db.lock().map_err(|_| NotifyError::Database)?;
        db.query_row("SELECT id,kind,message_id,phase,final_at,entity_id,chat_id,delete_at FROM event_notifications WHERE id=?1",[id],|r|Ok((Record{id:r.get(0)?,kind:r.get(1)?,message:r.get(2)?,phase:r.get(3)?,final_at:r.get(4)?},r.get::<_,i64>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<i64>>(7)?))).optional().map_err(|_|NotifyError::Database)?
    };
    let Some((row, training, user, delete_at)) = row else {
        return Ok(());
    };
    if row.kind == "training_booked" && row.phase == "preliminary" {
        if row.final_at.is_some_and(|v| v <= now) {
            let current = state(p, training, user)?;
            let snapshot = training_notify::snapshot(p, training)?;
            if !matches!(current,Some((ref status,true)) if status=="booked")
                || snapshot.as_ref().is_none_or(|s| s.start <= now)
            {
                return remove(p, bot, user, &row).await;
            }
            let s = snapshot.ok_or(NotifyError::Database)?;
            let deadline = s
                .start
                .saturating_add(s.duration.saturating_mul(60))
                .saturating_add(14400);
            let text = booked_text(&s, false);
            let new_id = send_new(
                p,
                bot,
                StatusMessage {
                    training,
                    user,
                    kind: "training_booked",
                    text: &text,
                    phase: "final",
                    deadline,
                    final_at: None,
                    now,
                },
            )
            .await?;
            if row.message != 0
                && row.message != new_id
                && matches!(
                    bot.request(
                        "deleteMessage",
                        json!({"chat_id":user,"message_id":row.message})
                    )
                    .await,
                    Err(TelegramError::Transport | TelegramError::Configuration)
                )
            {
                let db = p.db.lock().map_err(|_| NotifyError::Database)?;
                db.execute("INSERT OR IGNORE INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,sent_at) VALUES('notice',?1,?2,?2,?1,'',?3,?3,?3)",params![row.message,user,now]).map_err(|_|NotifyError::Database)?;
            }
            return Ok(());
        }
        if delete_at.is_some_and(|v| v <= now) {
            if row.message != 0
                && matches!(
                    bot.request(
                        "deleteMessage",
                        json!({"chat_id":user,"message_id":row.message})
                    )
                    .await,
                    Err(TelegramError::Transport | TelegramError::Configuration)
                )
            {
                return Err(NotifyError::Telegram);
            }
            let db = p.db.lock().map_err(|_| NotifyError::Database)?;
            db.execute("UPDATE event_notifications SET message_id=0,delete_at=NULL WHERE id=?1 AND message_id=?2 AND phase='preliminary'",params![id,row.message]).map_err(|_|NotifyError::Database)?;
        }
        return Ok(());
    }
    if delete_at.is_some_and(|v| v <= now) {
        remove(p, bot, user, &row).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
        fail_send: Mutex<bool>,
        fail_delete: Mutex<bool>,
    }
    impl BotTransport for Fake {
        async fn request(&self, method: &str, value: Value) -> Result<Value, TelegramError> {
            self.calls.lock().unwrap().push((method.into(), value));
            if (method == "sendMessage" && *self.fail_send.lock().unwrap())
                || (method == "deleteMessage" && *self.fail_delete.lock().unwrap())
            {
                return Err(TelegramError::Transport);
            }
            Ok(json!({"result":{"message_id":self.calls.lock().unwrap().len() as i64}}))
        }
    }
    fn service() -> Profiles {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO users(telegram_id,first_name,created_at) VALUES(1,'Coach',0),(2,'Player',0); INSERT INTO trainings(id,title,training_type,start_at,duration_min,created_at,created_by) VALUES(1,'T','Squash',400000,60,0,1); INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(1,2,'booked',0);").unwrap();
        Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 1,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                Duration::from_secs(60),
            ))),
        }
    }
    #[tokio::test]
    async fn preliminary_expiry_and_final_retry_are_persisted() {
        let p = service();
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        ensure(&p, &bot, &notifier, 1, 2, "booked", 1000)
            .await
            .unwrap();
        ensure(&p, &bot, &notifier, 1, 2, "booked", 1001)
            .await
            .unwrap();
        assert_eq!(bot.calls.lock().unwrap().len(), 1);
        {
            let db = p.db.lock().unwrap();
            let row:(String,i64,i64)=db.query_row("SELECT phase,delete_at,final_at FROM event_notifications WHERE kind='training_booked'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            assert_eq!(row, ("preliminary".into(), 87400, 313600));
        }
        *bot.fail_delete.lock().unwrap() = true;
        assert!(tick(&p, &bot, &notifier, 87400).await.is_err());
        assert_ne!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT message_id FROM event_notifications WHERE kind='training_booked'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        *bot.fail_delete.lock().unwrap() = false;
        tick(&p, &bot, &notifier, 87401).await.unwrap();
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT message_id FROM event_notifications WHERE kind='training_booked'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        *bot.fail_send.lock().unwrap() = true;
        assert!(tick(&p, &bot, &notifier, 313600).await.is_err());
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT phase FROM event_notifications WHERE kind='training_booked'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "preliminary"
        );
        *bot.fail_send.lock().unwrap() = false;
        tick(&p, &bot, &notifier, 313601).await.unwrap();
        {
            let db = p.db.lock().unwrap();
            let row:(String,i64,Option<i64>)=db.query_row("SELECT phase,delete_at,final_at FROM event_notifications WHERE kind='training_booked'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            assert_eq!(row, ("final".into(), 418000, None));
        }
        tick(&p, &bot, &notifier, 418000).await.unwrap();
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='training_booked'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn status_changes_remove_previous_card_and_honor_opt_out() {
        let p = service();
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        p.db.lock()
            .unwrap()
            .execute(
                "UPDATE bookings SET status='waitlist' WHERE training_id=1 AND user_id=2",
                [],
            )
            .unwrap();
        ensure(&p, &bot, &notifier, 1, 2, "waitlist", 1000)
            .await
            .unwrap();
        p.db.lock()
            .unwrap()
            .execute(
                "UPDATE bookings SET status='booked' WHERE training_id=1 AND user_id=2",
                [],
            )
            .unwrap();
        ensure(&p, &bot, &notifier, 1, 2, "booked", 1001)
            .await
            .unwrap();
        {
            let db = p.db.lock().unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='training_waitlist'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='training_booked'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
            db.execute(
                "UPDATE bookings SET status='cancelled' WHERE training_id=1 AND user_id=2",
                [],
            )
            .unwrap();
        }
        ensure(&p, &bot, &notifier, 1, 2, "cancelled", 1002)
            .await
            .unwrap();
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='training_booked'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        {
            let db = p.db.lock().unwrap();
            db.execute(
                "UPDATE users SET notify_trainings=0 WHERE telegram_id=2",
                [],
            )
            .unwrap();
            db.execute(
                "UPDATE bookings SET status='booked' WHERE training_id=1 AND user_id=2",
                [],
            )
            .unwrap();
        }
        let before = bot.calls.lock().unwrap().len();
        ensure(&p, &bot, &notifier, 1, 2, "booked", 1003)
            .await
            .unwrap();
        assert_eq!(bot.calls.lock().unwrap().len(), before);
    }
    #[tokio::test]
    async fn failed_old_message_delete_remains_scheduled_after_final_delivery() {
        let p = service();
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        ensure(&p, &bot, &notifier, 1, 2, "booked", 1000)
            .await
            .unwrap();
        *bot.fail_delete.lock().unwrap() = true;
        tick(&p, &bot, &notifier, 313600).await.unwrap();
        let db = p.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT phase FROM event_notifications WHERE kind='training_booked'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "final"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM event_notifications WHERE kind='notice' AND delete_at=313600",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    #[tokio::test]
    async fn refresh_updates_roster_without_resetting_status_deadline() {
        let p = service();
        let bot = Fake::default();
        let notifier = EventNotifier::default();
        ensure(&p, &bot, &notifier, 1, 2, "booked", 1000)
            .await
            .unwrap();
        {
            let db = p.db.lock().unwrap();
            db.execute_batch("INSERT INTO users(telegram_id,first_name,display_mode,created_at) VALUES(3,'Secret','animal',0); INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(1,3,'booked',0);").unwrap();
        }
        refresh(&p, &bot, &notifier, 1, 1001).await.unwrap();
        let calls = bot.calls.lock().unwrap();
        assert_eq!(calls[1].0, "editMessageText");
        let text = calls[1].1["text"].as_str().unwrap();
        assert!(text.contains(crate::identity::animal_identity(3).1));
        assert!(!text.contains("Secret"));
        drop(calls);
        let db = p.db.lock().unwrap();
        let (phase,deadline,final_at):(String,i64,i64)=db.query_row("SELECT phase,delete_at,final_at FROM event_notifications WHERE kind='training_booked'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(
            (phase.as_str(), deadline, final_at),
            ("preliminary", 87400, 313600)
        );
    }
}

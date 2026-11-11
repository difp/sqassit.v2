//! The legacy training reminder cycle. Run only one scheduler per database.
use crate::{
    profile::Profiles,
    telegram::{ReminderDeleter, ReminderSender, TelegramError},
};
use chrono::{DateTime, Utc};
use rusqlite::params;

#[derive(Debug)]
pub enum ReminderError {
    Database,
    InvalidTime,
}
#[derive(Default, Debug, PartialEq, Eq)]
pub struct TickReport {
    pub sent: usize,
    pub failed: usize,
}
struct Due {
    id: i64,
    start: i64,
    duration: i64,
    capacity: i64,
    training_type: String,
}
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
fn message(t: &Due, tz: chrono_tz::Tz) -> Result<String, ReminderError> {
    let local = DateTime::<Utc>::from_timestamp(t.start, 0)
        .ok_or(ReminderError::InvalidTime)?
        .with_timezone(&tz);
    let typ = if t.training_type.is_empty() {
        "Сквош-тренировка"
    } else {
        &t.training_type
    };
    Ok(format!(
        "⏰ <b>Напоминание о тренировке</b>\n\n🏸 {}\n📅 {}\nВсего мест: {}",
        escape_html(typ),
        local.format("%d.%m.%Y в %H:%M"),
        t.capacity
    ))
}
/// Run one tick. A failed recipient stays eligible for retry. Delivery and DB
/// persistence cannot be exactly-once across an external Telegram request.
pub async fn run_tick<S: ReminderSender>(
    profiles: &Profiles,
    sender: &S,
    now: i64,
) -> Result<TickReport, ReminderError> {
    let due = {
        let db = profiles.db.lock().map_err(|_| ReminderError::Database)?;
        let mut stmt=db.prepare("SELECT id,start_at,duration_min,capacity,training_type FROM trainings WHERE reminder_enabled=1 AND reminder_sent=0 AND deleted_at IS NULL AND start_at>?1 AND start_at-reminder_offset_min*60<=?1 ORDER BY id").map_err(|_|ReminderError::Database)?;
        let rows = stmt
            .query_map([now], |r| {
                Ok(Due {
                    id: r.get(0)?,
                    start: r.get(1)?,
                    duration: r.get(2)?,
                    capacity: r.get(3)?,
                    training_type: r.get(4)?,
                })
            })
            .map_err(|_| ReminderError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| ReminderError::Database)?
    };
    let mut report = TickReport::default();
    for event in due {
        let recipients = {
            let db = profiles.db.lock().map_err(|_| ReminderError::Database)?;
            let mut stmt=db.prepare("SELECT u.telegram_id FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=?1 AND b.status='booked' AND u.notifications_enabled=1 AND u.notify_trainings=1 AND NOT EXISTS(SELECT 1 FROM reminder_deliveries d WHERE d.training_id=b.training_id AND d.user_id=b.user_id) ORDER BY u.telegram_id").map_err(|_|ReminderError::Database)?;
            let rows = stmt
                .query_map([event.id], |r| r.get::<_, i64>(0))
                .map_err(|_| ReminderError::Database)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|_| ReminderError::Database)?
        };
        let text = message(&event, profiles.timezone)?;
        for user in recipients {
            match sender.send(user, &text).await {
                Ok(message_id) => {
                    let mut db = profiles.db.lock().map_err(|_| ReminderError::Database)?;
                    let tx = db.transaction().map_err(|_| ReminderError::Database)?;
                    let deadline = (event.start + event.duration * 60 + 14400).min(now + 47 * 3600);
                    tx.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,phase,sent_at,final_at) VALUES('training_reminder',?1,?2,?2,?3,?4,?5,?6,'',?5,NULL) ON CONFLICT(kind,entity_id,chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,text=excluded.text,updated_at=excluded.updated_at,delete_at=CASE WHEN event_notifications.message_id=excluded.message_id AND event_notifications.delete_at IS NOT NULL AND excluded.delete_at IS NOT NULL THEN MIN(event_notifications.delete_at,excluded.delete_at) ELSE excluded.delete_at END,phase=excluded.phase,sent_at=CASE WHEN event_notifications.message_id=excluded.message_id THEN event_notifications.sent_at ELSE excluded.sent_at END,final_at=excluded.final_at",params![event.id,user,message_id,text,now,deadline]).map_err(|_|ReminderError::Database)?;
                    tx.execute("INSERT OR IGNORE INTO reminder_deliveries(training_id,user_id) VALUES(?1,?2)",params![event.id,user]).map_err(|_|ReminderError::Database)?;
                    tx.commit().map_err(|_| ReminderError::Database)?;
                    report.sent += 1;
                }
                Err(_) => report.failed += 1,
            }
        }
        let db = profiles.db.lock().map_err(|_| ReminderError::Database)?;
        db.execute("UPDATE trainings SET reminder_sent=1 WHERE id=?1 AND reminder_enabled=1 AND reminder_sent=0 AND deleted_at IS NULL AND start_at=?2 AND NOT EXISTS(SELECT 1 FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=?1 AND b.status='booked' AND u.notifications_enabled=1 AND u.notify_trainings=1 AND NOT EXISTS(SELECT 1 FROM reminder_deliveries d WHERE d.training_id=b.training_id AND d.user_id=b.user_id))",params![event.id,event.start]).map_err(|_|ReminderError::Database)?;
    }
    Ok(report)
}

/// Remove expired reminder messages; transient errors are retained for retry.
/// Handles chat, match, coach-application and reminder messages; other kinds remain pending.
pub async fn cleanup_expired<S: ReminderDeleter>(
    profiles: &Profiles,
    sender: &S,
    now: i64,
) -> Result<(), ReminderError> {
    let records = {
        let db = profiles.db.lock().map_err(|_| ReminderError::Database)?;
        let mut stmt=db.prepare("SELECT id,chat_id,message_id FROM event_notifications WHERE kind IN ('training_reminder','chat','match','match_feed','coach_application') AND delete_at IS NOT NULL AND delete_at<=?1 ORDER BY delete_at").map_err(|_|ReminderError::Database)?;
        let rows = stmt
            .query_map([now], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|_| ReminderError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| ReminderError::Database)?
    };
    for (id, chat, message) in records {
        if message != 0
            && matches!(
                sender.delete(chat, message).await,
                Err(TelegramError::Transport | TelegramError::Configuration)
            )
        {
            continue;
        }
        let db = profiles.db.lock().map_err(|_| ReminderError::Database)?;
        db.execute("DELETE FROM event_notifications WHERE id=?1 AND message_id=?2 AND delete_at IS NOT NULL AND delete_at<=?3",params![id,message,now]).map_err(|_|ReminderError::Database)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        profile::Profiles,
        telegram::{ReminderSender, TelegramError},
    };
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(i64, String)>>,
        deletions: Mutex<Vec<i64>>,
    }
    impl ReminderSender for Fake {
        async fn send(&self, user: i64, text: &str) -> Result<i64, TelegramError> {
            let mut calls = self.calls.lock().unwrap();
            let retry = calls.iter().any(|(id, _)| *id == user);
            calls.push((user, text.into()));
            if user == 2 && !retry {
                Err(TelegramError::Transport)
            } else {
                Ok(100 + user)
            }
        }
    }
    impl ReminderDeleter for Fake {
        async fn delete(&self, chat_id: i64, _message_id: i64) -> Result<(), TelegramError> {
            let mut calls = self.deletions.lock().unwrap();
            let retry = calls.contains(&chat_id);
            calls.push(chat_id);
            if chat_id == 2 && !retry {
                Err(TelegramError::Transport)
            } else {
                Ok(())
            }
        }
    }
    #[tokio::test]
    async fn retries_only_failed_recipients_and_records_temporary_messages() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,reminder_offset_min,created_at) VALUES(7,'x','<Сквош>',1100,60,4,10,0)",[]).unwrap();
        for id in 1..=3 {
            db.execute("INSERT INTO users(telegram_id,first_name,notify_trainings,created_at) VALUES(?1,'A',?2,0)",params![id,id!=3]).unwrap();
            db.execute("INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(7,?1,'booked',0)",[id]).unwrap();
        }
        let profiles = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 1,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let sender = Fake::default();
        assert_eq!(
            run_tick(&profiles, &sender, 1000).await.unwrap(),
            TickReport { sent: 1, failed: 1 }
        );
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row("SELECT reminder_sent FROM trainings WHERE id=7", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            run_tick(&profiles, &sender, 1001).await.unwrap(),
            TickReport { sent: 1, failed: 0 }
        );
        assert_eq!(
            run_tick(&profiles, &sender, 1002).await.unwrap(),
            TickReport { sent: 0, failed: 0 }
        );
        {
            let calls = sender.calls.lock().unwrap();
            assert_eq!(
                calls.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                vec![1, 2, 2]
            );
            assert!(calls[0].1.contains("&lt;Сквош&gt;"));
        }
        {
            let db = profiles.db.lock().unwrap();
            assert_eq!(
                db.query_row("SELECT reminder_sent FROM trainings WHERE id=7", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM reminder_deliveries", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                2
            );
            assert_eq!(db.query_row("SELECT count(*) FROM event_notifications WHERE kind='training_reminder' AND delete_at IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),2);
            db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at) VALUES('other',1,1,1,333,'',0,1)",[]).unwrap();
        }
        cleanup_expired(&profiles, &sender, 19_099).await.unwrap();
        assert!(sender.deletions.lock().unwrap().is_empty());
        cleanup_expired(&profiles, &sender, 19_100).await.unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='training_reminder'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        cleanup_expired(&profiles, &sender, 19_101).await.unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='training_reminder'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='other'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        profiles.db.lock().unwrap().execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at) VALUES('chat',99,3,3,777,'',0,19102)",[]).unwrap();
        cleanup_expired(&profiles, &sender, 19_102).await.unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM event_notifications WHERE kind='chat'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(sender.deletions.lock().unwrap().contains(&3));
        profiles.db.lock().unwrap().execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at) VALUES('match',1,4,4,888,'',0,19103),('match_feed',0,5,5,889,'',0,19103),('coach_application',77,6,6,890,'',0,19103)",[]).unwrap();
        cleanup_expired(&profiles, &sender, 19_103).await.unwrap();
        let db = profiles.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM event_notifications WHERE kind IN ('match','match_feed','coach_application')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

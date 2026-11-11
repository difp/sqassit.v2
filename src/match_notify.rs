//! Match event messages, using the same anonymous identities as match cards.
use crate::{
    bot,
    event_notify::{EventMessage, EventNotifier, NotifyError},
    identity,
    profile::Profiles,
    telegram::BotTransport,
};
use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use serde_json::json;

#[derive(Clone, Copy)]
pub enum Change {
    Request,
    Event,
    Deleted,
}
struct Player {
    id: i64,
    status: String,
    name: String,
}
struct Snapshot {
    creator: i64,
    card: String,
    header: String,
    pending: usize,
    players: Vec<Player>,
    managers: Vec<i64>,
}
fn snapshot(profiles: &Profiles, id: i64) -> Result<Option<Snapshot>, NotifyError> {
    let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
    let event:Option<(String,String,i64,i64,i64,Option<i64>)>=db.query_row("SELECT title,location,start_at,max_players,created_by,deleted_at FROM matches WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(|_|NotifyError::Database)?;
    let Some((title, location, start, capacity, creator, deleted)) = event else {
        return Ok(None);
    };
    let date = DateTime::<Utc>::from_timestamp(start, 0)
        .ok_or(NotifyError::Database)?
        .with_timezone(&profiles.timezone)
        .format("%d.%m.%Y в %H:%M")
        .to_string();
    let header = format!(
        "🏸 <b>{}</b>\n📅 {}",
        bot::escape(if title.is_empty() { "Матч" } else { &title }),
        date
    );
    let mut stmt=db.prepare("SELECT p.user_id,p.status,u.first_name,u.display_mode FROM match_players p JOIN users u ON u.telegram_id=p.user_id WHERE p.match_id=?1 ORDER BY p.created_at,p.id").map_err(|_|NotifyError::Database)?;
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
    let mut players = Vec::new();
    for row in rows {
        let (id, status, name, mode) = row.map_err(|_| NotifyError::Database)?;
        let name = if mode == "animal" {
            identity::animal_identity(id).1.to_owned()
        } else {
            name.filter(|v| !v.is_empty())
                .unwrap_or_else(|| "Игрок".into())
        };
        players.push(Player { id, status, name });
    }
    let booked: Vec<_> = players.iter().filter(|p| p.status == "booked").collect();
    let pending = players.iter().filter(|p| p.status == "pending").count();
    let shown: Vec<_> = if deleted.unwrap_or(0) != 0 {
        players.iter().collect()
    } else {
        booked
    };
    let mut card = format!("{header}\n👥 {}/{}", shown.len(), capacity);
    if !location.is_empty() {
        card.push_str(&format!("\n📍 {}", bot::escape(&location)));
    }
    let mut names = shown
        .iter()
        .take(25)
        .map(|p| bot::escape(&p.name))
        .collect::<Vec<_>>()
        .join(", ");
    if shown.len() > 25 {
        names.push('…');
    }
    if !names.is_empty() {
        card.push_str(&format!("\n{names}"));
    }
    if deleted.unwrap_or(0) != 0 {
        card.push_str("\n❌ Матч отменён");
    }
    let mut stmt=db.prepare("SELECT user_id FROM event_coauthors WHERE event_type='match' AND event_id=?1 ORDER BY created_at").map_err(|_|NotifyError::Database)?;
    let rows = stmt
        .query_map([id], |r| r.get::<_, i64>(0))
        .map_err(|_| NotifyError::Database)?;
    let mut managers = vec![creator];
    for row in rows {
        let id = row.map_err(|_| NotifyError::Database)?;
        if !managers.contains(&id) {
            managers.push(id);
        }
    }
    Ok(Some(Snapshot {
        creator,
        card,
        header,
        pending,
        players,
        managers,
    }))
}
fn allowed(profiles: &Profiles, id: i64) -> Result<bool, NotifyError> {
    let db = profiles.db.lock().map_err(|_| NotifyError::Database)?;
    db.query_row(
        "SELECT notifications_enabled AND notify_matches FROM users WHERE telegram_id=?1",
        [id],
        |r| r.get::<_, bool>(0),
    )
    .optional()
    .map_err(|_| NotifyError::Database)
    .map(|v| v.unwrap_or(false))
}
fn personal(header: &str, status: &str) -> String {
    let prefix = match status {
        "left" => "Вы вышли из матча.",
        "deleted" => "Матч отменён организатором.",
        "pending" => "📝 Заявка на матч отправлена организатору.",
        "rejected" => "К сожалению, заявка на матч отклонена.",
        "waitlist" => "⏳ Вы в листе ожидания матча.",
        _ => "✅ Вы присоединились к матчу.",
    };
    format!("{prefix}\n\n{header}")
}
fn update(card: &str, status: &str) -> String {
    let prefix = match status {
        "joined" => "✅ Вы присоединились к матчу.",
        "left" => "Вы вышли из матча.",
        _ => "🏸 Обновление матча.",
    };
    format!("{prefix}\n\n{card}")
}
/// Best effort: each eligible recipient is attempted even if another delivery fails.
pub struct MatchChange<'a> {
    pub base: &'a str,
    pub id: i64,
    pub user: i64,
    pub status: &'a str,
    pub change: Change,
    pub now: i64,
}
pub async fn notify<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    notifier: &EventNotifier,
    event: MatchChange<'_>,
) -> Result<(), NotifyError> {
    let MatchChange {
        base,
        id,
        user,
        status,
        change,
        now,
    } = event;
    let Some(snapshot) = snapshot(profiles, id)? else {
        return Ok(());
    };
    let mut deliveries = Vec::new();
    let mut management = snapshot.card.clone();
    if snapshot.pending > 0 && !matches!(change, Change::Deleted) {
        management.push_str(&format!("\n📝 Заявок: {}", snapshot.pending));
    }
    let markup = if snapshot.pending > 0 && !matches!(change, Change::Deleted) {
        Some(
            json!({"inline_keyboard":[[{"text":format!("Открыть заявки · {}",snapshot.pending),"web_app":{"url":bot::web_app_url(base)}}]]}),
        )
    } else {
        None
    };
    let managers = if matches!(change, Change::Deleted) {
        vec![snapshot.creator]
    } else {
        snapshot.managers.clone()
    };
    for manager in managers {
        deliveries.push((manager, 0, management.clone(), markup.clone()));
    }
    if matches!(change, Change::Deleted) {
        for player in &snapshot.players {
            if player.id != snapshot.creator {
                deliveries.push((
                    player.id,
                    player.id,
                    personal(&snapshot.header, "deleted"),
                    None,
                ));
            }
        }
    } else if matches!(change, Change::Request) {
        if user != snapshot.creator {
            deliveries.push((user, user, personal(&snapshot.header, "pending"), None));
        }
    } else {
        for player in &snapshot.players {
            if player.status == "booked" && player.id != snapshot.creator && player.id != user {
                deliveries.push((
                    player.id,
                    player.id,
                    update(&snapshot.card, "updated"),
                    None,
                ));
            }
        }
        if user != snapshot.creator {
            let text = if status == "joined" || status == "left" {
                update(&snapshot.card, status)
            } else {
                personal(&snapshot.header, status)
            };
            deliveries.push((user, user, text, None));
        }
    }
    let mut failed = false;
    for (chat, recipient, text, markup) in deliveries {
        if allowed(profiles, chat)?
            && notifier
                .send(
                    profiles,
                    bot,
                    EventMessage {
                        kind: "match",
                        entity: id,
                        chat,
                        user: recipient,
                        text: &text,
                        markup,
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
    async fn request_and_event_respect_preferences_and_anonymity() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO matches(id,title,location,start_at,max_players,created_by,created_at) VALUES(1,'<Cup>','A&B',20000,4,1,0)",[]).unwrap();
        db.execute("INSERT INTO users(telegram_id,first_name,display_mode,notifications_enabled,notify_matches,created_at) VALUES(1,'Owner','name',1,1,0),(2,'Private Name','animal',1,1,0),(3,'Pending','name',1,1,0),(4,'Disabled','name',1,0,0)",[]).unwrap();
        db.execute("INSERT INTO match_players(match_id,user_id,status,created_at) VALUES(1,2,'booked',1),(1,3,'pending',2),(1,4,'booked',3)",[]).unwrap();
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
            MatchChange {
                base: "https://example.test",
                id: 1,
                user: 3,
                status: "pending",
                change: Change::Request,
                now: 1000,
            },
        )
        .await
        .unwrap();
        {
            let calls = bot.0.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].1["chat_id"], 1);
            assert_eq!(calls[1].1["chat_id"], 3);
            let text = calls[0].1["text"].as_str().unwrap();
            assert!(text.contains("&lt;Cup&gt;"));
            assert!(text.contains("A&amp;B"));
            assert!(!text.contains("Private Name"));
            assert!(text.contains(identity::animal_identity(2).1));
            assert!(text.contains("Заявок: 1"));
        }
        notify(
            &p,
            &bot,
            &notifier,
            MatchChange {
                base: "https://example.test",
                id: 1,
                user: 3,
                status: "waitlist",
                change: Change::Event,
                now: 1001,
            },
        )
        .await
        .unwrap();
        {
            let calls = bot.0.lock().unwrap();
            assert!(calls.iter().all(|(_, v)| v["chat_id"] != 4));
            assert!(calls.iter().any(|(_, v)| v["chat_id"] == 2));
            assert!(calls.iter().any(|(_, v)| v["chat_id"] == 3
                && v["text"].as_str().unwrap().contains("листе ожидания")));
        }
        p.db.lock()
            .unwrap()
            .execute("UPDATE matches SET deleted_at=1002 WHERE id=1", [])
            .unwrap();
        notify(
            &p,
            &bot,
            &notifier,
            MatchChange {
                base: "https://example.test",
                id: 1,
                user: 1,
                status: "deleted",
                change: Change::Deleted,
                now: 1002,
            },
        )
        .await
        .unwrap();
        let calls = bot.0.lock().unwrap();
        for id in [2, 3] {
            assert!(calls.iter().any(|(_, v)| v["chat_id"] == id
                && v["text"]
                    .as_str()
                    .unwrap()
                    .contains("Матч отменён организатором")));
        }
    }
}

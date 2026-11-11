//! Slot and attendee callbacks for the incremental Telegram bot.
use crate::{
    booking,
    bot::{self, BotError},
    identity,
    profile::Profiles,
    telegram::BotTransport,
    trainings,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

struct Slot {
    id: i64,
    start: i64,
    capacity: i64,
    name: String,
    booked: i64,
}
fn get_slot(db: &Connection, id: i64) -> Result<Option<Slot>, BotError> {
    db.query_row("SELECT t.id,t.start_at,t.capacity,t.training_type,(SELECT count(*) FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=t.id AND b.status='booked') FROM trainings t WHERE t.id=?1 AND (t.deleted_at IS NULL OR t.deleted_at=0)",[id],|r|Ok(Slot{id:r.get(0)?,start:r.get(1)?,capacity:r.get(2)?,name:r.get(3)?,booked:r.get(4)?})).optional().map_err(|_|BotError::Database)
}
fn local(start: i64, tz: chrono_tz::Tz, fmt: &str) -> Result<String, BotError> {
    Ok(DateTime::<Utc>::from_timestamp(start, 0)
        .ok_or(BotError::Database)?
        .with_timezone(&tz)
        .format(fmt)
        .to_string())
}
fn label(slot: &Slot, tz: chrono_tz::Tz) -> Result<String, BotError> {
    let name = if slot.name.is_empty() {
        "Тренировка"
    } else {
        &slot.name
    };
    Ok(format!(
        "{} · {} · {}/{}",
        local(slot.start, tz, "%d.%m %H:%M")?,
        name.chars().take(18).collect::<String>(),
        slot.booked,
        slot.capacity
    ))
}
pub async fn send_slots<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    chat: i64,
    user: i64,
    now: i64,
    attendees: bool,
) -> Result<(), BotError> {
    let slots = {
        let db = profiles.db.lock().map_err(|_| BotError::Database)?;
        let is_coach = attendees
            && (user == profiles.owner
                || db
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
                        [user],
                        |r| r.get::<_, bool>(0),
                    )
                    .map_err(|_| BotError::Database)?);
        let end =
            trainings::end_of_window(now, 7, profiles.timezone).map_err(|_| BotError::Database)?;
        let sql="SELECT t.id,t.start_at,t.capacity,t.training_type,(SELECT count(*) FROM bookings b WHERE b.training_id=t.id AND b.status='booked') FROM trainings t WHERE t.start_at>=?1 AND t.deleted_at IS NULL AND ((?2=0 AND t.start_at<=?3) OR (?2=1 AND (t.created_by=?4 OR EXISTS(SELECT 1 FROM event_coauthors c WHERE c.event_type='training' AND c.event_id=t.id AND c.user_id=?4)))) ORDER BY t.start_at LIMIT 20";
        let mut stmt = db.prepare(sql).map_err(|_| BotError::Database)?;
        let rows = stmt
            .query_map(params![now, is_coach, end, user], |r| {
                Ok(Slot {
                    id: r.get(0)?,
                    start: r.get(1)?,
                    capacity: r.get(2)?,
                    name: r.get(3)?,
                    booked: r.get(4)?,
                })
            })
            .map_err(|_| BotError::Database)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| BotError::Database)?
    };
    if slots.is_empty() {
        bot::send(
            bot,
            chat,
            if attendees {
                "Ближайших тренировок нет."
            } else {
                "На ближайшие 7 дней тренировок нет."
            },
            None,
        )
        .await?;
        return Ok(());
    }
    let mut buttons = Vec::new();
    for slot in slots {
        buttons.push(json!([{"text":label(&slot,profiles.timezone)?,"callback_data":format!("{}:show:{}",if attendees{"attendees"}else{"slots"},slot.id)}]));
    }
    bot::send(
        bot,
        chat,
        if attendees {
            "Кто записался — выберите занятие:"
        } else {
            "Выберите слот:"
        },
        Some(json!({"inline_keyboard":buttons})),
    )
    .await?;
    Ok(())
}
fn details(
    profiles: &Profiles,
    id: i64,
    user: i64,
    now: i64,
) -> Result<Option<(String, Value)>, BotError> {
    let db = profiles.db.lock().map_err(|_| BotError::Database)?;
    let Some(slot) = get_slot(&db, id)? else {
        return Ok(None);
    };
    let status: Option<String> = db
        .query_row(
            "SELECT status FROM bookings WHERE training_id=?1 AND user_id=?2",
            params![id, user],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| BotError::Database)?;
    let waiting:i64=db.query_row("SELECT count(*) FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=?1 AND b.status='waitlist'",[id],|r|r.get(0)).map_err(|_|BotError::Database)?;
    let free = (slot.capacity - slot.booked).max(0);
    let status_text = match status.as_deref() {
        Some("booked") => "\n✅ Вы записаны",
        Some("waitlist") => "\n⏳ Вы в листе ожидания",
        Some("pending") => "\n📝 Заявка ожидает подтверждения",
        _ => "",
    };
    let name = if slot.name.is_empty() {
        "Тренировка"
    } else {
        &slot.name
    };
    let text = format!(
        "🏸 <b>{}</b>\n📅 {}\n👥 Свободно: {} из {}\n⌛ В очереди: {}{}",
        bot::escape(name),
        local(slot.start, profiles.timezone, "%d.%m.%Y в %H:%M")?,
        free,
        slot.capacity,
        waiting,
        status_text
    );
    let mut buttons = Vec::new();
    if matches!(status.as_deref(), Some("booked" | "waitlist" | "pending")) {
        buttons.push(
            json!([{"text":"🗑 Отменить запись","callback_data":format!("slots:cancel:{id}")}]),
        )
    } else if slot.start > now {
        buttons.push(json!([{"text":if free>0{"✅ Записаться"}else{"⏳ В лист ожидания"},"callback_data":format!("slots:book:{id}")}]))
    }
    buttons
        .push(json!([{"text":"👥 Кто записался","callback_data":format!("attendees:show:{id}")}]));
    buttons.push(json!([{"text":"⬅️ К слотам","callback_data":"slots:list"}]));
    Ok(Some((text, json!({"inline_keyboard":buttons}))))
}
async fn answer<T: BotTransport>(
    bot: &T,
    id: &str,
    text: &str,
    alert: bool,
) -> Result<(), BotError> {
    bot.request(
        "answerCallbackQuery",
        json!({"callback_query_id":id,"text":text,"show_alert":alert}),
    )
    .await?;
    Ok(())
}
fn attendees_text(profiles: &Profiles, id: i64, user: i64) -> Result<Option<String>, BotError> {
    let db = profiles.db.lock().map_err(|_| BotError::Database)?;
    let Some(slot) = get_slot(&db, id)? else {
        return Ok(None);
    };
    let coach = user == profiles.owner
        || db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
                [user],
                |r| r.get::<_, bool>(0),
            )
            .map_err(|_| BotError::Database)?;
    let mut stmt=db.prepare("SELECT b.status,u.telegram_id,u.first_name,u.username,u.phone,u.display_mode FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=?1 AND b.status IN ('booked','waitlist') ORDER BY CASE b.status WHEN 'booked' THEN 0 ELSE 1 END,b.position ASC").map_err(|_|BotError::Database)?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(|_| BotError::Database)?;
    let (mut booked, mut waiting) = (Vec::new(), Vec::new());
    for row in rows {
        let (status, id, name, username, phone, mode) = row.map_err(|_| BotError::Database)?;
        let name = if mode == "animal" {
            identity::animal_identity(id).1.to_owned()
        } else {
            name.filter(|x| !x.is_empty())
                .unwrap_or_else(|| "Игрок".into())
        };
        let mut text = bot::escape(&name);
        if coach {
            let username = username.unwrap_or_default();
            let handle = if !username.is_empty()
                && username.len() <= 32
                && username
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                format!("<a href=\"https://t.me/{username}\">@{username}</a>")
            } else {
                if username.is_empty() {
                    "без username".to_owned()
                } else {
                    bot::escape(&format!("@{username}"))
                }
            };
            text = format!(
                "{text} · {handle} · {}",
                bot::escape(if phone.is_empty() {
                    "телефон не указан"
                } else {
                    &phone
                })
            );
        }
        if status == "booked" && booked.len() < 40 {
            booked.push(text)
        } else if status == "waitlist" && waiting.len() < 20 {
            waiting.push(text)
        }
    }
    let name = if slot.name.is_empty() {
        "Тренировка"
    } else {
        &slot.name
    };
    let mut out = format!(
        "👥 <b>Кто записался</b> · {}\nЗаписаны: {}",
        bot::escape(name),
        if booked.is_empty() {
            "пока никого".into()
        } else {
            booked.join(", ")
        }
    );
    if !waiting.is_empty() {
        out.push_str(&format!("\nВ очереди: {}", waiting.join(", ")));
    }
    Ok(Some(out))
}
fn change(
    profiles: &Profiles,
    id: i64,
    user: i64,
    now: i64,
    book: bool,
) -> Result<Result<String, &'static str>, BotError> {
    let mut db = profiles.db.lock().map_err(|_| BotError::Database)?;
    let Some(slot) = get_slot(&db, id)? else {
        return Ok(Err("Тренировка не найдена"));
    };
    if book {
        if slot.start <= now {
            return Ok(Err("Тренировка уже началась"));
        }
        if profiles.phone_required_from.is_some_and(|v| now >= v) {
            let phone: Option<String> = db
                .query_row(
                    "SELECT phone FROM users WHERE telegram_id=?1",
                    [user],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| BotError::Database)?;
            if phone.is_none_or(|v| v.is_empty()) {
                return Ok(Err("Укажите телефон в настройках профиля."));
            }
        }
        let status = booking::book(
            &mut db,
            id,
            user,
            slot.capacity,
            profiles.require_event_approval,
            false,
            now,
        )
        .map_err(|_| BotError::Database)?;
        Ok(Ok(match status.as_str() {
            "pending" => "Заявка отправлена тренеру",
            "booked" => "Вы записаны",
            _ => "Вы в листе ожидания",
        }
        .to_owned()))
    } else {
        if slot.start - now < 270 * 60 {
            return Ok(Err("Отмена недоступна: до старта меньше 4.5 часов"));
        }
        let status: Option<String> = db
            .query_row(
                "SELECT status FROM bookings WHERE training_id=?1 AND user_id=?2",
                params![id, user],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| BotError::Database)?;
        if !matches!(status.as_deref(), Some("pending" | "booked" | "waitlist")) {
            return Ok(Err("Вы не записаны на эту тренировку"));
        }
        booking::cancel(&mut db, id, user).map_err(|_| BotError::Database)?;
        Ok(Ok("Запись отменена".into()))
    }
}
async fn application<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    callback: &Value,
    now: i64,
) -> Result<(), BotError> {
    let cb = callback
        .get("id")
        .and_then(Value::as_str)
        .ok_or(BotError::Malformed)?;
    let actor = callback
        .pointer("/from/id")
        .and_then(Value::as_i64)
        .ok_or(BotError::Malformed)?;
    let data = callback
        .get("data")
        .and_then(Value::as_str)
        .ok_or(BotError::Malformed)?;
    let parts: Vec<_> = data.split(':').collect();
    let allowed = {
        let db = profiles.db.lock().map_err(|_| BotError::Database)?;
        actor == profiles.owner
            || db
                .query_row(
                    "SELECT is_manager FROM coaches WHERE telegram_id=?1",
                    [actor],
                    |r| r.get::<_, bool>(0),
                )
                .optional()
                .map_err(|_| BotError::Database)?
                .unwrap_or(false)
    };
    if !allowed {
        answer(bot, cb, "Недоступно", true).await?;
        return Ok(());
    }
    if parts.len() != 3 || !matches!(parts[1], "ok" | "no") {
        answer(bot, cb, "", false).await?;
        return Ok(());
    }
    let applicant = parts[2].parse::<i64>().map_err(|_| BotError::Malformed)?;
    let Some(chat) = callback.pointer("/message/chat/id").and_then(Value::as_i64) else {
        answer(bot, cb, "Сообщение недоступно", true).await?;
        return Ok(());
    };
    let approved = parts[1] == "ok";
    let processed = {
        let mut db = profiles.db.lock().map_err(|_| BotError::Database)?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| BotError::Database)?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM coach_applications WHERE telegram_id=?1",
                [applicant],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| BotError::Database)?;
        if status.as_deref() != Some("pending") {
            false
        } else {
            if approved {
                let inserted=tx.execute("INSERT OR IGNORE INTO coaches(telegram_id,added_by,created_at) VALUES(?1,?2,?3)",params![applicant,actor,now]).map_err(|_|BotError::Database)?;
                if inserted > 0 {
                    tx.execute(
                        "UPDATE users SET is_coach=1 WHERE telegram_id=?1",
                        [applicant],
                    )
                    .map_err(|_| BotError::Database)?;
                }
            }
            tx.execute("UPDATE coach_applications SET status=?1,decided_by=?2,decided_at=?3 WHERE telegram_id=?4",params![if approved{"approved"}else{"rejected"},actor,now,applicant]).map_err(|_|BotError::Database)?;
            tx.execute("UPDATE event_notifications SET delete_at=?1 WHERE kind='coach_application' AND entity_id=?2",params![now,applicant]).map_err(|_|BotError::Database)?;
            tx.commit().map_err(|_| BotError::Database)?;
            true
        }
    };
    if !processed {
        answer(bot, cb, "Заявка уже рассмотрена", true).await?;
        return Ok(());
    }
    bot::send(
        bot,
        chat,
        if approved {
            "✅ Заявка одобрена"
        } else {
            "❌ Заявка отклонена"
        },
        None,
    )
    .await?;
    answer(
        bot,
        cb,
        if approved {
            "Одобрено"
        } else {
            "Отклонено"
        },
        false,
    )
    .await?;
    let text = if approved {
        "🎉 Ваша заявка одобрена! Откройте приложение — появится вкладка «Тренер»."
    } else {
        "Ваша заявка на тренера отклонена."
    };
    if let Ok(sent) = bot::send(bot, applicant, text, None).await {
        if let Some(message_id) = sent
            .pointer("/result/message_id")
            .and_then(Value::as_i64)
            .filter(|id| *id > 0)
        {
            let db = profiles.db.lock().map_err(|_| BotError::Database)?;
            db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,sent_at) VALUES('notice',?1,?2,?2,?1,?3,?4,?5,?4) ON CONFLICT(kind,entity_id,chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,text=excluded.text,updated_at=excluded.updated_at,delete_at=excluded.delete_at",params![message_id,applicant,text,now,now+86400]).map_err(|_|BotError::Database)?;
        }
    }
    Ok(())
}
pub async fn handle_callback<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    callback: &Value,
    now: i64,
) -> Result<(), BotError> {
    let cb = callback
        .get("id")
        .and_then(Value::as_str)
        .ok_or(BotError::Malformed)?;
    let user = callback
        .pointer("/from/id")
        .and_then(Value::as_i64)
        .ok_or(BotError::Malformed)?;
    let data = callback
        .get("data")
        .and_then(Value::as_str)
        .ok_or(BotError::Unsupported)?;
    if data.starts_with("apply:") {
        return application(profiles, bot, callback, now).await;
    }
    let chat = callback.pointer("/message/chat/id").and_then(Value::as_i64);
    if data == "slots:list" || data == "attendees:list" {
        answer(bot, cb, "", false).await?;
        if let Some(chat) = chat {
            send_slots(profiles, bot, chat, user, now, data == "attendees:list").await?;
        }
        return Ok(());
    }
    let Some((kind, raw)) = data.rsplit_once(':') else {
        return Err(BotError::Unsupported);
    };
    if !matches!(
        kind,
        "slots:show" | "slots:book" | "slots:cancel" | "attendees:show"
    ) {
        return Err(BotError::Unsupported);
    }
    let id = raw.parse::<i64>().map_err(|_| BotError::Malformed)?;
    if kind == "slots:book" || kind == "slots:cancel" {
        match change(profiles, id, user, now, kind == "slots:book")? {
            Err(message) => {
                answer(bot, cb, message, true).await?;
            }
            Ok(message) => {
                answer(bot, cb, &message, false).await?;
                if let (Some(chat), Some(message_id), Some((text, markup))) = (
                    chat,
                    callback
                        .pointer("/message/message_id")
                        .and_then(Value::as_i64),
                    details(profiles, id, user, now)?,
                ) {
                    bot.request("editMessageText",json!({"chat_id":chat,"message_id":message_id,"text":text,"parse_mode":"HTML","reply_markup":markup})).await?;
                }
            }
        }
    } else {
        answer(bot, cb, "", false).await?;
        if let Some(chat) = chat {
            if kind == "slots:show" {
                match details(profiles, id, user, now)? {
                    Some((text, markup)) => {
                        bot::send(bot, chat, &text, Some(markup)).await?;
                    }
                    None => {
                        bot::send(bot, chat, "Тренировка не найдена.", None).await?;
                    }
                }
            } else {
                let text = attendees_text(profiles, id, user)?
                    .unwrap_or_else(|| "Тренировка не найдена.".into());
                bot::send(bot, chat, &text, None).await?;
            }
        }
    }
    Ok(())
}

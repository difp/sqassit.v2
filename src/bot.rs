//! Incremental Telegram update handlers. Receiving updates is not enabled yet.
use crate::{
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

const RULES:&str="<b>Правила SQASSIT</b>\n\n• Записывайтесь только при уверенности, что придёте. Отменяйте запись заранее — минимум за 4,5 часа.\n\n• Уважайте других игроков: соблюдайте договорённости, не опаздывайте и избегайте грубости.\n\n• Указывайте достоверные данные при создании матчей.\n\n• Личные данные доступны тренеру для модерации.\n\n• Нашли ошибку или есть идея? Напишите через «Обратную связь».\n\nПравила могут обновляться.";
#[derive(Debug)]
pub enum BotError {
    Database,
    Telegram,
    Malformed,
    Unsupported,
}
impl From<TelegramError> for BotError {
    fn from(_: TelegramError) -> Self {
        Self::Telegram
    }
}
pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
pub(crate) fn web_app_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        "https://t.me".to_owned()
    } else {
        format!("{base}{}v=46", if base.contains('?') { "&" } else { "?" })
    }
}
fn keyboard(base: &str) -> Value {
    let url = web_app_url(base);
    json!({"inline_keyboard":[[{"text":"🏸 Открыть тренировки","web_app":{"url":url}}],[{"text":"🗓 Выбрать слот","callback_data":"slots:list"},{"text":"👥 Кто записался","callback_data":"attendees:list"}]]})
}
fn phone_keyboard() -> Value {
    json!({"keyboard":[[{"text":"📱 Поделиться телефоном","request_contact":true}]],"resize_keyboard":true,"one_time_keyboard":true})
}
pub(crate) async fn send<T: BotTransport>(
    bot: &T,
    chat: i64,
    text: &str,
    markup: Option<Value>,
) -> Result<Value, BotError> {
    let mut payload = json!({"chat_id":chat,"text":text,"parse_mode":"HTML"});
    if let Some(markup) = markup {
        payload["reply_markup"] = markup;
    }
    bot.request("sendMessage", payload)
        .await
        .map_err(Into::into)
}
async fn rules<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    user: i64,
    now: i64,
) -> Result<(), BotError> {
    let existing = {
        let db = profiles.db.lock().map_err(|_| BotError::Database)?;
        db.query_row("SELECT message_id,text FROM event_notifications WHERE kind='rules' AND entity_id=0 AND chat_id=?1 AND user_id=0",[user],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).optional().map_err(|_|BotError::Database)?
    };
    let mut message = existing.as_ref().map(|r| r.0).unwrap_or(0);
    if let Some((id, text)) = &existing {
        if *id != 0 && text != RULES {
            let result = bot
                .request(
                    "editMessageText",
                    json!({"chat_id":user,"message_id":id,"text":RULES,"parse_mode":"HTML"}),
                )
                .await;
            if !matches!(result, Ok(_) | Err(TelegramError::NotModified)) {
                message = 0;
            }
        }
    }
    if message == 0 {
        message = send(bot, user, RULES, None)
            .await?
            .pointer("/result/message_id")
            .and_then(Value::as_i64)
            .unwrap_or(0);
    }
    if message != 0 {
        {
            let db = profiles.db.lock().map_err(|_| BotError::Database)?;
            db.execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,sent_at) VALUES('rules',0,?1,0,?2,?3,?4,?4) ON CONFLICT(kind,entity_id,chat_id,user_id) DO UPDATE SET message_id=excluded.message_id,text=excluded.text,updated_at=excluded.updated_at",params![user,message,RULES,now]).map_err(|_|BotError::Database)?;
        }
        let _ = bot
            .request(
                "pinChatMessage",
                json!({"chat_id":user,"message_id":message,"disable_notification":true}),
            )
            .await;
    }
    Ok(())
}
/// Handles the migrated commands, callbacks, contact and fallback text.
/// Unrelated updates return Unsupported and can be ignored by the receiver.
pub async fn handle_update<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    base_url: &str,
    notifier: &crate::event_notify::EventNotifier,
    update: &Value,
    now: i64,
) -> Result<(), BotError> {
    if let Some(callback) = update.get("callback_query") {
        return crate::bot_slots::handle_callback(profiles, bot, notifier, base_url, callback, now)
            .await;
    }
    let message = update.get("message").ok_or(BotError::Unsupported)?;
    let from = message.get("from").ok_or(BotError::Malformed)?;
    let id = from
        .get("id")
        .and_then(Value::as_i64)
        .ok_or(BotError::Malformed)?;
    let chat = message
        .pointer("/chat/id")
        .and_then(Value::as_i64)
        .ok_or(BotError::Malformed)?;
    if let Some(contact) = message.get("contact") {
        if contact.get("user_id").and_then(Value::as_i64) != Some(id) {
            send(bot, chat, "Поделитесь, пожалуйста, своим контактом.", None).await?;
            return Ok(());
        }
        let phone = contact
            .get("phone_number")
            .and_then(Value::as_str)
            .unwrap_or("");
        let normalized = crate::profile::normalize_phone(phone).unwrap_or_default();
        if normalized.is_empty() {
            send(
                bot,
                chat,
                "Не удалось распознать номер. Введите телефон в настройках профиля.",
                None,
            )
            .await?;
            return Ok(());
        }
        {
            let db = profiles.db.lock().map_err(|_| BotError::Database)?;
            db.execute(
                "UPDATE users SET phone=?1 WHERE telegram_id=?2",
                params![normalized, id],
            )
            .map_err(|_| BotError::Database)?;
        }
        send(
            bot,
            chat,
            "✅ Телефон сохранён. Его увидит только тренер.",
            Some(json!({"remove_keyboard":true})),
        )
        .await?;
        send(bot, chat, "Меню:", Some(keyboard(base_url))).await?;
        return Ok(());
    }
    let Some(text) = message.get("text").and_then(Value::as_str) else {
        return Err(BotError::Unsupported);
    };
    let command = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");
    match command {
        "/start" => {
            {
                let db = profiles.db.lock().map_err(|_| BotError::Database)?;
                profiles
                    .register(&db, id, from, now)
                    .map_err(|_| BotError::Database)?;
            }
            if let Ok(photos) = bot
                .request("getUserProfilePhotos", json!({"user_id":id,"limit":1}))
                .await
            {
                if let Some(file) = photos
                    .pointer("/result/photos/0")
                    .and_then(Value::as_array)
                    .and_then(|p| p.last())
                    .and_then(|p| p.get("file_id"))
                    .and_then(Value::as_str)
                {
                    let db = profiles.db.lock().map_err(|_| BotError::Database)?;
                    db.execute(
                        "UPDATE users SET avatar_file_id=?1 WHERE telegram_id=?2",
                        params![file, id],
                    )
                    .map_err(|_| BotError::Database)?;
                }
            }
            let _ = rules(profiles, bot, id, now).await;
            let name = from
                .get("first_name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("друг");
            send(bot,chat,&format!("Привет, {}!\n\nЗдесь можно записаться на сквош-тренировки: смотри расписание на 7 дней, записывайся в один тап и следи за свободными местами.\n\nНажми кнопку ниже, чтобы открыть приложение.",escape(name)),Some(keyboard(base_url))).await?;
            let no_phone = {
                let db = profiles.db.lock().map_err(|_| BotError::Database)?;
                db.query_row(
                    "SELECT phone='' FROM users WHERE telegram_id=?1",
                    [id],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(|_| BotError::Database)?
            };
            if no_phone {
                send(bot,chat,"📱 Телефон нужен тренеру для связи и модерации. Поделитесь контактом кнопкой ниже или введите номер в профиле.",Some(phone_keyboard())).await?;
            }
        }
        "/phone" => {
            send(bot,chat,"📱 Поделитесь телефоном, чтобы тренер мог связаться с вами. Можно также ввести номер вручную в настройках профиля.",Some(phone_keyboard())).await?;
        }
        "/slots" => {
            crate::bot_slots::send_slots(profiles, bot, chat, id, now, false).await?;
        }
        _ => {
            send(
                bot,
                chat,
                "Открой приложение кнопкой ниже 👇",
                Some(keyboard(base_url)),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
    }
    impl BotTransport for Fake {
        async fn request(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
            let mut calls = self.calls.lock().unwrap();
            calls.push((method.into(), payload));
            if method == "getUserProfilePhotos" {
                Ok(json!({"ok":true,"result":{"total_count":0,"photos":[]}}))
            } else {
                Ok(json!({"ok":true,"result":{"message_id":calls.len()}}))
            }
        }
    }
    fn profiles() -> Profiles {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "123:test".into(),
            owner: 123,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        }
    }
    #[tokio::test]
    async fn start_reuses_rules_and_contact_must_belong_to_sender() {
        let profiles = profiles();
        let bot = Fake::default();
        let start = json!({"message":{"from":{"id":42,"first_name":"<Иван>"},"chat":{"id":42},"text":"/start"}});
        handle_update(
            &profiles,
            &bot,
            "https://example.com",
            &crate::event_notify::EventNotifier::default(),
            &start,
            1000,
        )
        .await
        .unwrap();
        handle_update(
            &profiles,
            &bot,
            "https://example.com",
            &crate::event_notify::EventNotifier::default(),
            &start,
            1001,
        )
        .await
        .unwrap();
        {
            let calls = bot.calls.lock().unwrap();
            assert_eq!(
                calls
                    .iter()
                    .filter(|(method, p)| method == "sendMessage" && p["text"] == RULES)
                    .count(),
                1
            );
            assert!(calls
                .iter()
                .any(|(_, p)| p["text"].as_str().unwrap_or("").contains("&lt;Иван&gt;")));
            assert!(calls.iter().any(|(_, p)| p
                .pointer("/reply_markup/inline_keyboard/0/0/web_app/url")
                .and_then(Value::as_str)
                == Some("https://example.com?v=46")));
        }
        let other = json!({"message":{"from":{"id":42},"chat":{"id":42},"contact":{"user_id":99,"phone_number":"79991234567"}}});
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &other,
            1002,
        )
        .await
        .unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row("SELECT phone FROM users WHERE telegram_id=42", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            ""
        );
        let own = json!({"message":{"from":{"id":42},"chat":{"id":42},"contact":{"user_id":42,"phone_number":"+7 (999) 123-45-67"}}});
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &own,
            1003,
        )
        .await
        .unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row("SELECT phone FROM users WHERE telegram_id=42", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "+79991234567"
        );
        let slots = json!({"message":{"from":{"id":42},"chat":{"id":42},"text":"/slots"}});
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &slots,
            1004,
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn unchanged_rules_do_not_create_duplicate_messages() {
        struct Unchanged {
            calls: Mutex<Vec<String>>,
        }
        impl BotTransport for Unchanged {
            async fn request(&self, method: &str, _payload: Value) -> Result<Value, TelegramError> {
                self.calls.lock().unwrap().push(method.to_owned());
                if method == "editMessageText" {
                    Err(TelegramError::NotModified)
                } else {
                    Ok(json!({"ok":true}))
                }
            }
        }
        let profiles = profiles();
        let bot = Unchanged {
            calls: Mutex::new(Vec::new()),
        };
        profiles.db.lock().unwrap().execute("INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at) VALUES('rules',0,42,0,88,'old',0)",[]).unwrap();
        rules(&profiles, &bot, 42, 1000).await.unwrap();
        let calls = bot.calls.lock().unwrap();
        assert_eq!(*calls, vec!["editMessageText", "pinChatMessage"]);
        let db = profiles.db.lock().unwrap();
        let (message, text): (i64, String) = db
            .query_row(
                "SELECT message_id,text FROM event_notifications WHERE kind='rules'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(message, 88);
        assert_eq!(text, RULES);
    }
    #[tokio::test]
    async fn slot_callbacks_notify_booking_and_waitlist_promotion() {
        let profiles = profiles();
        let bot = Fake::default();
        let notifier = crate::event_notify::EventNotifier::default();
        {
            let db = profiles.db.lock().unwrap();
            db.execute_batch("INSERT INTO users(telegram_id,first_name,notify_trainings,created_at) VALUES(1,'First',1,0),(2,'Second',1,0); INSERT INTO trainings(id,title,training_type,start_at,capacity,created_at) VALUES(7,'x','Test',25000,1,0);").unwrap();
        }
        let callback = |user: i64, data: &str| json!({"callback_query":{"id":"cb","from":{"id":user},"data":data,"message":{"message_id":55,"chat":{"id":user}}}});
        for (user, data) in [(1, "slots:book:7"), (2, "slots:book:7")] {
            handle_update(
                &profiles,
                &bot,
                "https://example.test",
                &notifier,
                &callback(user, data),
                1000,
            )
            .await
            .unwrap();
        }
        {
            let db = profiles.db.lock().unwrap();
            let count: i64 = db.query_row("SELECT COUNT(*) FROM event_notifications WHERE (kind='training_booked' AND chat_id=1) OR (kind='training_waitlist' AND chat_id=2)", [], |r| r.get(0)).unwrap();
            assert_eq!(count, 2);
        }
        handle_update(
            &profiles,
            &bot,
            "https://example.test",
            &notifier,
            &callback(1, "slots:cancel:7"),
            1000,
        )
        .await
        .unwrap();
        {
            let db = profiles.db.lock().unwrap();
            let status: String = db
                .query_row(
                    "SELECT status FROM bookings WHERE training_id=7 AND user_id=2",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(status, "booked");
            let count: i64 = db.query_row("SELECT COUNT(*) FROM event_notifications WHERE kind='training_booked' AND chat_id=2", [], |r| r.get(0)).unwrap();
            assert_eq!(count, 1);
        }
        let before = bot.calls.lock().unwrap().len();
        handle_update(
            &profiles,
            &bot,
            "https://example.test",
            &notifier,
            &callback(1, "slots:book:999"),
            1000,
        )
        .await
        .unwrap();
        let calls = bot.calls.lock().unwrap();
        assert_eq!(calls.len(), before + 1);
        assert_eq!(calls.last().unwrap().0, "answerCallbackQuery");
    }
    #[tokio::test]
    async fn slot_callbacks_book_cancel_and_hide_private_details() {
        let profiles = profiles();
        let bot = Fake::default();
        {
            let db = profiles.db.lock().unwrap();
            db.execute("INSERT INTO users(telegram_id,first_name,username,phone,display_mode,created_at) VALUES(42,'Игрок',NULL,'','name',0)",[]).unwrap();
            db.execute("INSERT INTO users(telegram_id,first_name,username,phone,display_mode,created_at) VALUES(314,'Секрет','secret_handle','1234567890','animal',0)",[]).unwrap();
            db.execute("INSERT INTO trainings(id,title,training_type,start_at,capacity,created_at) VALUES(7,'x','Тест',25000,2,0)",[]).unwrap();
            db.execute("INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(7,314,'booked',0)",[]).unwrap();
        }
        let callback = |data: &str| json!({"callback_query":{"id":"cb","from":{"id":42},"data":data,"message":{"message_id":55,"chat":{"id":42}}}});
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &callback("slots:show:7"),
            1000,
        )
        .await
        .unwrap();
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &callback("slots:book:7"),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT status FROM bookings WHERE training_id=7 AND user_id=42",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "booked"
        );
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &callback("attendees:show:7"),
            1000,
        )
        .await
        .unwrap();
        {
            let calls = bot.calls.lock().unwrap();
            let text = calls
                .iter()
                .rev()
                .find(|(m, _)| m == "sendMessage")
                .unwrap()
                .1["text"]
                .as_str()
                .unwrap();
            assert!(!text.contains("Секрет"));
            assert!(!text.contains("secret_handle"));
            assert!(!text.contains("1234567890"));
        }
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &callback("slots:cancel:7"),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT status FROM bookings WHERE training_id=7 AND user_id=42",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "cancelled"
        );
        {
            let db = profiles.db.lock().unwrap();
            db.execute(
                "INSERT INTO users(telegram_id,first_name,created_at) VALUES(99,'Заявитель',0)",
                [],
            )
            .unwrap();
            db.execute("INSERT INTO coach_applications(telegram_id,status,created_at) VALUES(99,'pending',0)",[]).unwrap();
        }
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &callback("apply:ok:99"),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT status FROM coach_applications WHERE telegram_id=99",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "pending"
        );
        let admin = json!({"callback_query":{"id":"cb2","from":{"id":123},"data":"apply:ok:99","message":{"chat":{"id":123}}}});
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &admin,
            1001,
        )
        .await
        .unwrap();
        {
            let db = profiles.db.lock().unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT status FROM coach_applications WHERE telegram_id=99",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "approved"
            );
            assert_eq!(
                db.query_row("SELECT is_coach FROM users WHERE telegram_id=99", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        handle_update(
            &profiles,
            &bot,
            "",
            &crate::event_notify::EventNotifier::default(),
            &admin,
            1002,
        )
        .await
        .unwrap();
    }
}

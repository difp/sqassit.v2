//! Opt-in comparison of outgoing Telegram calls against the Python bot.
use serde_json::{json, Value};
use sqassit::{
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
};
use std::{
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn message(id: i64, text: Option<&str>, contact: Option<Value>) -> Value {
    let mut value = json!({
        "message_id": 10,
        "date": 0,
        "chat": {"id": id, "type": "private"},
        "from": {"id": id, "is_bot": false, "first_name": format!("User{id}")},
    });
    if let Some(text) = text {
        value["text"] = json!(text);
    }
    if let Some(contact) = contact {
        value["contact"] = contact;
    }
    value
}

fn callback(uid: i64, data: &str) -> Value {
    json!({
        "id": "cb-1",
        "from": {"id": uid, "is_bot": false, "first_name": format!("User{uid}")},
        "message": {
            "message_id": 50,
            "date": 0,
            "chat": {"id": uid, "type": "private"},
        },
        "chat_instance": "instance",
        "data": data,
    })
}

fn updates() -> Vec<Value> {
    vec![
        message(77, Some("/start"), None),
        message(77, Some("/phone"), None),
        message(77, Some("hello"), None),
        message(77, Some("/slots"), None),
        message(
            77,
            Some(""),
            Some(json!({"phone_number":"+7 (999) 123-45-67","first_name":"User77","user_id":77})),
        ),
        message(
            88,
            Some(""),
            Some(json!({"phone_number":"+7 (999) 000-00-00","first_name":"Other","user_id":77})),
        ),
        message(77, Some("/start"), None),
        message(123, Some("/start"), None),
        message(77, Some("/slots"), None),
        json!({"callback_query": callback(77, "slots:show:1")}),
        json!({"callback_query": callback(77, "slots:show:999")}),
        json!({"callback_query": callback(77, "slots:list")}),
        json!({"callback_query": callback(77, "attendees:list")}),
        json!({"callback_query": callback(77, "attendees:show:1")}),
        json!({"callback_query": callback(77, "attendees:show:999")}),
        json!({"callback_query": callback(77, "apply:ok:45")}),
        json!({"callback_query": callback(123, "apply:ok:44")}),
        json!({"callback_query": callback(123, "apply:ok:44")}),
        json!({"callback_query": callback(123, "apply:no:45")}),
    ]
}

struct Fake {
    calls: Mutex<Vec<Value>>,
    next_id: AtomicI64,
}

impl BotTransport for Fake {
    async fn request(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
        let mut params = payload;
        if let Some(object) = params.as_object_mut() {
            object.remove("parse_mode");
            if method == "answerCallbackQuery" {
                if object.get("text").and_then(Value::as_str) == Some("") {
                    object.remove("text");
                }
                if object.get("show_alert").and_then(Value::as_bool) == Some(false) {
                    object.remove("show_alert");
                }
            }
        }
        self.calls
            .lock()
            .unwrap()
            .push(json!({"method":method,"params":params}));
        let result = if method == "sendMessage" {
            json!({"message_id": self.next_id.fetch_add(1, Ordering::SeqCst) + 1})
        } else if method == "getUserProfilePhotos" {
            json!({"total_count":0,"photos":[]})
        } else {
            json!(true)
        };
        Ok(json!({"ok":true,"result":result}))
    }
}

fn rows(db: &rusqlite::Connection, sql: &str) -> Vec<Value> {
    let mut stmt = db.prepare(sql).unwrap();
    let names = stmt.column_names().map(str::to_owned).collect::<Vec<_>>();
    stmt.query_map([], |row| {
        let mut values = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            let value: rusqlite::types::Value = row.get(index)?;
            values.insert(
                name.clone(),
                match value {
                    rusqlite::types::Value::Null => Value::Null,
                    rusqlite::types::Value::Integer(v) => json!(v),
                    rusqlite::types::Value::Real(v) => json!(v),
                    rusqlite::types::Value::Text(v) => json!(v),
                    rusqlite::types::Value::Blob(_) => panic!("unexpected blob"),
                },
            );
        }
        Ok(Value::Object(values))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[tokio::test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
async fn same_bot_calls_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/bot_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python bot reference");
    assert!(
        output.status.success(),
        "Python bot reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute_batch(&format!("INSERT INTO coaches(telegram_id,created_at) VALUES(123,0); INSERT INTO users(telegram_id,first_name,is_coach,created_at) VALUES(123,'Тренер',1,0),(88,'Other',0,0); INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,created_by,created_at) VALUES(1,'Squash','Squash',{},60,2,123,{}); INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES(1,77,'booked',0,{}),(1,88,'waitlist',1,{}); INSERT INTO coach_applications(telegram_id,message,status,created_at) VALUES(44,'Хочу тренировать','pending',{}),(45,'И я','pending',{});", now + 10800, now, now, now, now, now)).unwrap();
    let profiles = Profiles {
        db: Arc::new(Mutex::new(db)),
        bot_token: "123456:TEST-TOKEN".into(),
        owner: 123,
        timezone: chrono_tz::Europe::Moscow,
        require_event_approval: false,
        phone_required_from: None,
        booking_limiter: Arc::new(Mutex::new(sqassit::rate_limit::RateLimiter::new(
            60,
            Duration::from_secs(60),
        ))),
    };
    let fake = Fake {
        calls: Mutex::new(Vec::new()),
        next_id: AtomicI64::new(100),
    };
    let notifier = sqassit::event_notify::EventNotifier::default();
    for (index, raw) in updates().into_iter().enumerate() {
        let update = if let Some(query) = raw.get("callback_query") {
            json!({"update_id": 1, "callback_query": query})
        } else {
            json!({"update_id": 1, "message": raw})
        };
        sqassit::bot::handle_update(&profiles, &fake, "", &notifier, &update, now)
            .await
            .unwrap_or_else(|error| panic!("update {index} {update}: {error:?}"));
    }
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(json!(calls), expected["calls"], "outgoing calls");

    let db = profiles.db.lock().unwrap();
    assert_eq!(
        json!(rows(
            &db,
            "SELECT telegram_id,first_name,phone,is_coach FROM users ORDER BY telegram_id"
        )),
        expected["users"],
        "users"
    );
    assert_eq!(
        json!(rows(
            &db,
            "SELECT kind,entity_id,chat_id,user_id,message_id,text FROM event_notifications ORDER BY kind,entity_id,chat_id,user_id"
        )),
        expected["notifications"],
        "notifications"
    );
    assert_eq!(
        json!(rows(
            &db,
            "SELECT telegram_id,added_by,is_manager FROM coaches ORDER BY telegram_id"
        )),
        expected["coaches"],
        "coaches"
    );
    assert_eq!(
        json!(rows(
            &db,
            "SELECT telegram_id,message,status,decided_by FROM coach_applications ORDER BY telegram_id"
        )),
        expected["applications"],
        "applications"
    );
}

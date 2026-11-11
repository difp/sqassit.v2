//! Opt-in comparison of coach-application notifications with the Python server.
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

struct Fake {
    calls: Mutex<Vec<Value>>,
    next_id: AtomicI64,
}

impl BotTransport for Fake {
    async fn request(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
        let mut params = payload;
        if let Some(object) = params.as_object_mut() {
            object.remove("parse_mode");
        }
        self.calls
            .lock()
            .unwrap()
            .push(json!({"method": method, "params": params}));
        if method == "sendMessage" {
            Ok(json!({
                "ok": true,
                "result": {"message_id": self.next_id.fetch_add(1, Ordering::SeqCst) + 1},
            }))
        } else {
            Ok(json!({"ok": true, "result": true}))
        }
    }
}

fn rows(db: &rusqlite::Connection) -> Vec<Value> {
    let mut stmt = db
        .prepare(
            "SELECT kind,entity_id,chat_id,user_id,message_id,text \
             FROM event_notifications ORDER BY chat_id,user_id",
        )
        .unwrap();
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
async fn same_application_notifications_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/application_notify_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python application-notification reference");
    assert!(
        output.status.success(),
        "Python application-notification reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO users(telegram_id,first_name,display_mode,notifications_enabled,notify_messages,created_at) VALUES \
         (1,'Owner','name',1,1,{now}),(2,'Secret Name','animal',1,1,{now}),\
         (3,'Manager','name',1,1,{now}),(4,'Quiet Manager','name',0,0,{now}); \
         INSERT INTO coaches(telegram_id,is_manager,created_at) VALUES (1,1,{now}),(3,1,{now}),(4,1,{now}); \
         INSERT INTO coach_applications(telegram_id,message,status,created_at) \
         VALUES(2,'Хочу тренировать','pending',{now});"
    ))
    .unwrap();
    let profiles = Profiles {
        db: Arc::new(Mutex::new(db)),
        bot_token: "123456:TEST-TOKEN".into(),
        owner: 1,
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
    sqassit::role_notify::application(
        &profiles,
        &fake,
        &sqassit::event_notify::EventNotifier::default(),
        "https://example.test",
        2,
        now,
    )
    .await
    .expect("application notification");

    let mut calls = fake.calls.lock().unwrap().clone();
    calls.sort_by_key(|call| call["params"]["chat_id"].as_i64());
    assert_eq!(json!(calls), expected["calls"], "calls");
    let db = profiles.db.lock().unwrap();
    assert_eq!(json!(rows(&db)), expected["notifications"], "notifications");
    let deadlines = {
        let mut stmt = db
            .prepare("SELECT chat_id,delete_at FROM event_notifications ORDER BY chat_id,user_id")
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    assert_eq!(
        deadlines,
        vec![(1, Some(now + 86_400)), (3, Some(now + 86_400))]
    );
}

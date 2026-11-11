//! Opt-in comparison of preliminary-to-final enrollment against the Python bot.
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
            "SELECT id,kind,entity_id,chat_id,user_id,message_id,text,delete_at,phase,final_at \
             FROM event_notifications ORDER BY id",
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
async fn same_final_enrollment_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/final_enrollment_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python final-enrollment reference");
    assert!(
        output.status.success(),
        "Python final-enrollment reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,created_at) \
         VALUES(123,'Тренер',1,1,{now}),(77,'Player',1,1,{now}),(88,'Quiet',0,0,{now}); \
         INSERT INTO coaches(telegram_id,created_at) VALUES(123,{now}); \
         INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,created_by,created_at) \
         VALUES(2,'Squash','Squash',{},60,4,123,{now}); \
         INSERT INTO bookings(training_id,user_id,status,position,created_at) \
         VALUES(2,77,'booked',0,{now}),(2,88,'booked',1,{now}); \
         INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,phase,sent_at,final_at) \
         VALUES('training_booked',2,77,77,111,'preliminary',{now},{},'preliminary',{now},{}),\
               ('training_booked',2,88,88,112,'preliminary',{now},{},'preliminary',{now},{});",
        now + 3600,
        now + 1000,
        now - 10,
        now + 1000,
        now - 10
    ))
    .unwrap();
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
    sqassit::training_status::tick(&profiles, &fake, &notifier, now)
        .await
        .expect("training status tick");
    let mut calls = fake.calls.lock().unwrap().clone();
    // The Python loop handles deletions and final enrollments in separate
    // passes, so relative ordering of unrelated users is not compared.
    calls.sort_by_key(|call| serde_json::to_string(call).unwrap());
    assert_eq!(json!(calls), expected["calls"], "calls");

    let db = profiles.db.lock().unwrap();
    assert_eq!(json!(rows(&db)), expected["rows"], "rows");
}

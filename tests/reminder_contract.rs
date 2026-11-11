//! Opt-in comparison of the training reminder tick against the Python bot.
use serde_json::{json, Value};
use sqassit::{
    profile::Profiles,
    telegram::{ReminderSender, TelegramError},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Fake {
    calls: Mutex<Vec<Value>>,
    next_id: Mutex<i64>,
}

impl ReminderSender for Fake {
    async fn send(&self, chat_id: i64, text: &str) -> Result<i64, TelegramError> {
        self.calls.lock().unwrap().push(json!({
            "method": "sendMessage",
            "params": {"chat_id": chat_id, "text": text},
        }));
        let mut id = self.next_id.lock().unwrap();
        *id += 1;
        Ok(*id)
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
async fn same_reminder_tick_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/reminder_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python reminder reference");
    assert!(
        output.status.success(),
        "Python reminder reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO users(telegram_id,first_name,created_at) VALUES(77,'Player',{now}); \
         INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,reminder_enabled,reminder_offset_min,created_by,created_at) \
         VALUES(1,'Squash <A&B>','Squash <A&B>',{},60,4,1,60,123,{now}); \
         INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES(1,77,'booked',0,{now});",
        now + 1800
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
        next_id: Mutex::new(100),
    };
    let report = sqassit::reminders::run_tick(&profiles, &fake, now)
        .await
        .expect("reminder tick");
    assert_eq!(report.sent, 1);
    assert_eq!(json!(fake.calls.lock().unwrap().clone()), expected["calls"]);

    let db = profiles.db.lock().unwrap();
    assert_eq!(
        json!(rows(
            &db,
            "SELECT id,reminder_sent FROM trainings ORDER BY id"
        )),
        expected["trainings"],
        "trainings"
    );
    assert_eq!(
        json!(rows(
            &db,
            "SELECT training_id,user_id FROM reminder_deliveries ORDER BY training_id,user_id"
        )),
        expected["deliveries"],
        "deliveries"
    );
    assert_eq!(
        json!(rows(
            &db,
            "SELECT kind,entity_id,chat_id,user_id,message_id,text,delete_at FROM event_notifications ORDER BY kind,entity_id,chat_id,user_id"
        )),
        expected["notifications"],
        "notifications"
    );
}

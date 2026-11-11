//! Opt-in comparison of match-feed refresh and removal with the Python server.
use serde_json::{json, Map, Value};
use sqassit::{
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<Value>>,
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
        Ok(json!({"ok": true, "result": true}))
    }
}

fn sorted(mut calls: Vec<Value>) -> Vec<Value> {
    calls.sort_by_key(|call| serde_json::to_string(call).unwrap());
    calls
}

fn rows(db: &rusqlite::Connection) -> Vec<Value> {
    let mut stmt = db
        .prepare(
            "SELECT kind,entity_id,chat_id,user_id,message_id,text \
             FROM event_notifications ORDER BY chat_id",
        )
        .unwrap();
    let names = stmt.column_names().map(str::to_owned).collect::<Vec<_>>();
    stmt.query_map([], |row| {
        let mut values = Map::new();
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
async fn same_match_feed_refresh_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/match_feed_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python match-feed reference");
    assert!(
        output.status.success(),
        "Python match-feed reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");
    let start = now + 7200;

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_matches,created_at) VALUES \
         (2,'Stale',1,1,{now}),(3,'Current',1,1,{now}),(4,'Quiet',0,0,{now}); \
         INSERT INTO matches(id,title,location,start_at,duration_min,max_players,created_by,created_at) VALUES \
         (9,'Feed <A&B>','Hall & Court',{start},60,4,1,{now});"
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
    // Seed the "current" feed with Python's expected text so an unchanged feed
    // must stay untouched in Rust too.
    let current = expected["first_rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["chat_id"] == 3)
        .unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned();
    profiles
        .db
        .lock()
        .unwrap()
        .execute_batch(&format!(
            "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,sent_at) VALUES \
             ('match_feed',0,2,2,401,'old feed',{now},{},{now}),\
             ('match_feed',0,3,3,402,'{}',{now},{},{now}),\
             ('match_feed',0,4,4,403,'quiet feed',{now},{},{now});",
            now + 86400,
            current.replace('\'', "''"),
            now + 86400,
            now + 86400
        ))
        .unwrap();

    let fake = Fake::default();
    let notifier = sqassit::event_notify::EventNotifier::default();
    sqassit::match_feed::refresh(&profiles, &fake, &notifier, now)
        .await
        .expect("first match-feed refresh");
    let first_calls = sorted(fake.calls.lock().unwrap().clone());
    fake.calls.lock().unwrap().clear();
    let first_rows = {
        let db = profiles.db.lock().unwrap();
        rows(&db)
    };

    sqassit::match_feed::refresh(&profiles, &fake, &notifier, now + 7201)
        .await
        .expect("second match-feed refresh");
    let second_calls = sorted(fake.calls.lock().unwrap().clone());
    let second_rows = {
        let db = profiles.db.lock().unwrap();
        rows(&db)
    };

    assert_eq!(json!(first_calls), expected["first_calls"], "first calls");
    assert_eq!(json!(first_rows), expected["first_rows"], "first rows");
    assert_eq!(
        json!(second_calls),
        expected["second_calls"],
        "second calls"
    );
    assert_eq!(json!(second_rows), expected["second_rows"], "second rows");
}

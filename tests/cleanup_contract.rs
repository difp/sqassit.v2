//! Opt-in comparison of the unified notification cleanup tick against the
//! Python notifications loop.
use serde_json::{json, Value};
use sqassit::{
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// One row per notification kind; offsets are seconds relative to `now`.
struct Seed {
    kind: &'static str,
    entity: i64,
    chat: i64,
    user: i64,
    message: i64,
    text: &'static str,
    delete: Option<i64>,
    phase: &'static str,
    final_at: Option<i64>,
}

fn seeds() -> Vec<Seed> {
    // Delete offsets are distinct and deliberately out of row order, so both
    // sides must sort by delete_at to produce the same call sequence.
    vec![
        Seed {
            kind: "training",
            entity: 1,
            chat: 77,
            user: 77,
            message: 101,
            text: "card",
            delete: Some(-5),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "chat",
            entity: 5,
            chat: 77,
            user: 77,
            message: 102,
            text: "chat",
            delete: Some(-9),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "match",
            entity: 2,
            chat: 77,
            user: 77,
            message: 103,
            text: "match",
            delete: Some(-1),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "match_feed",
            entity: 1,
            chat: 77,
            user: 77,
            message: 104,
            text: "feed",
            delete: Some(-7),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "coach_application",
            entity: 44,
            chat: 123,
            user: 123,
            message: 105,
            text: "application",
            delete: Some(-3),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "notice",
            entity: 0,
            chat: 77,
            user: 77,
            message: 106,
            text: "notice",
            delete: Some(-8),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "training_reminder",
            entity: 1,
            chat: 77,
            user: 77,
            message: 107,
            text: "reminder",
            delete: Some(-2),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "training_booked",
            entity: 1,
            chat: 77,
            user: 77,
            message: 108,
            text: "final booked",
            delete: Some(-10),
            phase: "final",
            final_at: None,
        },
        Seed {
            kind: "training_waitlist",
            entity: 1,
            chat: 88,
            user: 88,
            message: 0,
            text: "waitlist without message",
            delete: Some(-6),
            phase: "status",
            final_at: None,
        },
        Seed {
            kind: "training_rejected",
            entity: 1,
            chat: 99,
            user: 99,
            message: 110,
            text: "rejected",
            delete: Some(-4),
            phase: "status",
            final_at: None,
        },
        // Preliminary confirmation whose final message is not due yet: the
        // message is deleted, but the row is kept for the final confirmation.
        Seed {
            kind: "training_booked",
            entity: 2,
            chat: 77,
            user: 77,
            message: 111,
            text: "preliminary",
            delete: Some(-11),
            phase: "preliminary",
            final_at: Some(3600),
        },
        Seed {
            kind: "training",
            entity: 3,
            chat: 77,
            user: 77,
            message: 112,
            text: "future card",
            delete: Some(1000),
            phase: "",
            final_at: None,
        },
        Seed {
            kind: "chat",
            entity: 6,
            chat: 77,
            user: 77,
            message: 113,
            text: "no deadline",
            delete: None,
            phase: "",
            final_at: None,
        },
        // Deletion fails transiently on the first tick and succeeds on the second.
        Seed {
            kind: "notice",
            entity: 9,
            chat: 88,
            user: 88,
            message: 114,
            text: "transient",
            delete: Some(-12),
            phase: "",
            final_at: None,
        },
    ]
}

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<Value>>,
    fail_for: Mutex<Option<i64>>,
}

impl Fake {
    fn record(&self, method: &str, params: Value) -> Result<Value, TelegramError> {
        let message = params.get("message_id").and_then(Value::as_i64);
        self.calls
            .lock()
            .unwrap()
            .push(json!({"method": method, "params": params}));
        if method == "deleteMessage" && message == *self.fail_for.lock().unwrap() {
            return Err(TelegramError::Transport);
        }
        Ok(if method == "sendMessage" {
            json!({"ok": true, "result": {"message_id": 999}})
        } else {
            json!({"ok": true, "result": true})
        })
    }
    fn calls(&self) -> Vec<Value> {
        self.calls.lock().unwrap().clone()
    }
}

impl BotTransport for Fake {
    async fn request(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
        self.record(method, payload)
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
async fn same_cleanup_tick_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/cleanup_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python cleanup reference");
    assert!(
        output.status.success(),
        "Python cleanup reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    for seed in seeds() {
        db.execute(
            "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,\
             updated_at,delete_at,phase,sent_at,final_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?7,?10)",
            rusqlite::params![
                seed.kind,
                seed.entity,
                seed.chat,
                seed.user,
                seed.message,
                seed.text,
                now,
                seed.delete.map(|offset| now + offset),
                seed.phase,
                seed.final_at.map(|offset| now + offset),
            ],
        )
        .unwrap();
    }
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
    let fake = Fake::default();
    let notifier = sqassit::event_notify::EventNotifier::default();
    *fake.fail_for.lock().unwrap() = Some(114);

    // The tick now performs the whole Python notifications loop in one pass:
    // every kind is deleted in delete_at order, then due finals and stale
    // preliminary rows are processed, so the call order is compared exactly.
    let _ = sqassit::training_status::tick(&profiles, &fake, &notifier, now).await;
    assert_eq!(
        json!(fake.calls()),
        expected["first_calls"],
        "first tick calls"
    );
    {
        let db = profiles.db.lock().unwrap();
        assert_eq!(json!(rows(&db)), expected["first_rows"], "first tick rows");
    }
    fake.calls.lock().unwrap().clear();

    *fake.fail_for.lock().unwrap() = None;
    let _ = sqassit::training_status::tick(&profiles, &fake, &notifier, now).await;
    assert_eq!(
        json!(fake.calls()),
        expected["second_calls"],
        "second tick calls"
    );
    let db = profiles.db.lock().unwrap();
    assert_eq!(
        json!(rows(&db)),
        expected["second_rows"],
        "second tick rows"
    );
}

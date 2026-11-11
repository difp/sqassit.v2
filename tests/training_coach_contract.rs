//! Opt-in comparison of coach participant-action notifications with the Python server.
use serde_json::{json, Map, Value};
use sqassit::{
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
    training_notify::{self, TrainingChange},
};
use std::{
    collections::HashMap,
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
        if method == "sendMessage" {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
            self.calls.lock().unwrap().push(json!({
                "method": method,
                "params": params,
                "message_id": id,
            }));
            return Ok(json!({"ok": true, "result": {"message_id": id}}));
        }
        self.calls
            .lock()
            .unwrap()
            .push(json!({"method": method, "params": params}));
        Ok(json!({"ok": true, "result": true}))
    }
}

fn labels(calls: &[Value]) -> HashMap<i64, String> {
    calls
        .iter()
        .filter(|call| call["method"] == "sendMessage")
        .filter_map(|call| {
            let id = call["message_id"].as_i64()?;
            let chat = call["params"]["chat_id"].as_i64()?;
            let text = call["params"]["text"].as_str()?;
            Some((id, format!("{chat}|{text}")))
        })
        .collect()
}

fn normalize_calls(calls: &[Value]) -> Vec<Value> {
    let labels = labels(calls);
    // Python refreshes every existing card after each participant, which
    // re-sends text that was just delivered. Rust only edits changed text.
    let sent: HashMap<String, Value> = calls
        .iter()
        .filter(|call| call["method"] == "sendMessage")
        .filter_map(|call| {
            let chat = call["params"]["chat_id"].as_i64()?;
            let text = call["params"]["text"].as_str()?;
            let mut body = json!({"text": text});
            if let Some(markup) = call["params"].get("reply_markup") {
                body["reply_markup"] = markup.clone();
            }
            Some((format!("{chat}|{text}"), body))
        })
        .collect();
    let mut kept = Vec::new();
    for call in calls {
        let mut call = call.clone();
        if let Some(object) = call.as_object_mut() {
            object.remove("message_id");
        }
        if let Some(params) = call["params"].as_object_mut() {
            if let Some(label) = params
                .get("message_id")
                .and_then(Value::as_i64)
                .and_then(|id| labels.get(&id))
            {
                params.insert("message_id".into(), json!(label));
            }
        }
        if call["method"] == "editMessageText" {
            let mut body = json!({"text": call["params"].get("text")});
            if let Some(markup) = call["params"].get("reply_markup") {
                body["reply_markup"] = markup.clone();
            }
            if call["params"]["message_id"]
                .as_str()
                .and_then(|label| sent.get(label))
                == Some(&body)
            {
                continue;
            }
        }
        kept.push(call);
    }
    kept.sort_by_key(|call| serde_json::to_string(call).unwrap());
    kept.dedup();
    kept
}

fn normalize_rows(rows: &mut [Value], calls: &[Value]) {
    let labels = labels(calls);
    for row in rows.iter_mut() {
        for key in ["entity_id", "message_id"] {
            if let Some(label) = row[key].as_i64().and_then(|id| labels.get(&id)).cloned() {
                row[key] = json!(label);
            }
        }
    }
    rows.sort_by_key(|row| serde_json::to_string(row).unwrap());
}

fn rows(db: &rusqlite::Connection) -> Vec<Value> {
    let mut stmt = db
        .prepare(
            "SELECT kind,entity_id,chat_id,user_id,message_id,text \
             FROM event_notifications ORDER BY kind,entity_id,chat_id,user_id",
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
async fn same_coach_participant_notifications_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/training_coach_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python coach action reference");
    assert!(
        output.status.success(),
        "Python coach action reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    let deadline = now + 5000;
    db.execute_batch(&format!(
        "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,created_at) VALUES \
         (1,'Coach',1,1,{now}),(2,'Approved',1,1,{now}),(3,'Removed',1,1,{now}),\
         (4,'Waiter',1,1,{now}),(5,'Rejected',1,1,{now}),(6,'Coauthor',1,1,{now}); \
         INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,level,created_by,created_at) VALUES \
         (7,'Actions <A&B>','Squash <A&B>',{},60,2,'Beginner',1,{now}); \
         INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES \
         (7,2,'pending',0,{now}),(7,3,'booked',0,{now}),\
         (7,4,'waitlist',1,{now}),(7,5,'pending',2,{now}); \
         INSERT INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) VALUES \
         ('training',7,6,1,{now}); \
         INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,phase,sent_at) VALUES \
         ('training_booked',7,3,3,301,'old booked',{now},{deadline},'final',{now}),\
         ('training',7,3,3,302,'old card',{now},{deadline},'',{now});",
        now + 7200
    ))
    .unwrap();
    let profiles = Profiles {
        db: Arc::new(Mutex::new(db)),
        bot_token: "123456:TEST-TOKEN".into(),
        owner: 1,
        timezone: chrono_tz::Europe::Moscow,
        require_event_approval: true,
        phone_required_from: None,
        booking_limiter: Arc::new(Mutex::new(sqassit::rate_limit::RateLimiter::new(
            60,
            Duration::from_secs(60),
        ))),
    };
    let fake = Fake {
        calls: Mutex::new(Vec::new()),
        next_id: AtomicI64::new(1000),
    };
    let notifier = sqassit::event_notify::EventNotifier::default();
    let change = |user, status: &'static str, actor| TrainingChange {
        training: 7,
        user,
        status,
        base: "https://example.test",
        now,
        actor,
    };

    profiles
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE bookings SET status='booked',position=0 \
             WHERE training_id=7 AND user_id=2",
            [],
        )
        .unwrap();
    training_notify::notify(&profiles, &fake, &notifier, change(2, "booked", Some(1)))
        .await
        .expect("approval");

    profiles
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE bookings SET status='rejected',position=0 \
             WHERE training_id=7 AND user_id=5",
            [],
        )
        .unwrap();
    training_notify::notify(&profiles, &fake, &notifier, change(5, "rejected", Some(1)))
        .await
        .expect("rejection");

    profiles
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE bookings SET status='cancelled',position=0 \
             WHERE training_id=7 AND user_id=3",
            [],
        )
        .unwrap();
    // Python's cancel promotes the first waiter before scheduling notifications.
    profiles
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE bookings SET status='booked',position=0 \
             WHERE training_id=7 AND user_id=4",
            [],
        )
        .unwrap();
    training_notify::notify(&profiles, &fake, &notifier, change(3, "cancelled", Some(1)))
        .await
        .expect("participant removal");

    training_notify::notify(&profiles, &fake, &notifier, change(4, "promoted", None))
        .await
        .expect("promotion");

    profiles
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE bookings SET status='waitlist',position=2 \
             WHERE training_id=7 AND user_id=3",
            [],
        )
        .unwrap();
    training_notify::notify(&profiles, &fake, &notifier, change(3, "waitlist", Some(1)))
        .await
        .expect("participant restore");

    let calls = fake.calls.lock().unwrap().clone();
    let actual = normalize_calls(&calls);
    let expected_calls = normalize_calls(expected["calls"].as_array().unwrap());
    assert_eq!(actual, expected_calls, "calls");

    let db = profiles.db.lock().unwrap();
    let mut actual_rows = rows(&db);
    normalize_rows(&mut actual_rows, &calls);
    let mut expected_rows = expected["notifications"].as_array().unwrap().clone();
    normalize_rows(&mut expected_rows, expected["calls"].as_array().unwrap());
    assert_eq!(actual_rows, expected_rows, "notifications");
}

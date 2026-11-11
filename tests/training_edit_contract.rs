//! Opt-in comparison of training edit/restore notifications with the Python server.
use serde_json::{json, Map, Value};
use sqassit::{
    profile::Profiles,
    telegram::{BotTransport, TelegramError},
    training_notify::{self, Lifecycle},
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Python reuses the participant alert path while re-sending organizer cards,
/// so every edit also alerts managers about the player's own action. Rust
/// treats a training edit as a silent card update instead.
const ALERTS: [&str; 7] = [
    "Новая запись: ",
    "Новая заявка: ",
    "В лист ожидания: ",
    "Отмена записи: ",
    "Заявка отклонена: ",
    "Место освободилось: ",
    "Изменение записи: ",
];

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

fn alert(text: &str) -> bool {
    ALERTS.iter().any(|prefix| text.starts_with(prefix))
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
        if call["method"] == "sendMessage" && call["params"]["text"].as_str().is_some_and(alert) {
            continue;
        }
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

fn normalize_rows(rows: &mut Vec<Value>, calls: &[Value]) {
    let labels = labels(calls);
    for row in rows.iter_mut() {
        if row["kind"] == "notice" && row["text"].as_str().is_some_and(alert) {
            row["kind"] = Value::Null;
            continue;
        }
        for key in ["entity_id", "message_id"] {
            if let Some(label) = row[key].as_i64().and_then(|id| labels.get(&id)).cloned() {
                row[key] = json!(label);
            }
        }
    }
    rows.retain(|row| !row["kind"].is_null());
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
async fn same_training_edit_and_restore_notifications_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/training_edit_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python training edit reference");
    assert!(
        output.status.success(),
        "Python training edit reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    let deadline = now + 5000;
    db.execute_batch(&format!(
        "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,created_at) VALUES \
         (1,'Coach',1,1,{now}),(2,'Booked',1,1,{now}),(3,'Waiting',1,1,{now}),\
         (4,'Pending',1,1,{now}),(5,'Quiet',0,0,{now}),(6,'Coauthor',1,1,{now}),\
         (7,'Restored',1,1,{now}); \
         INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,level,created_by,created_at,deleted_at) VALUES \
         (7,'Edit <A&B>','Squash <A&B>',{},60,4,'Beginner',1,{now},NULL),\
         (8,'Restore','Squash',{},60,4,'Beginner',1,{now},{now}); \
         INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES \
         (7,2,'booked',0,{now}),(7,3,'waitlist',1,{now}),(7,4,'pending',2,{now}),\
         (7,5,'booked',3,{now}),(7,6,'booked',4,{now}),(8,7,'booked',0,{now}); \
         INSERT INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) VALUES \
         ('training',7,6,1,{now}); \
         INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,text,updated_at,delete_at,phase,sent_at) VALUES \
         ('training',7,1,0,201,'old organizer',{now},{deadline},'',{now}),\
         ('training',7,2,2,202,'old player',{now},{deadline},'',{now}),\
         ('training',7,3,3,203,'old waitlist card',{now},{deadline},'',{now}),\
         ('training',7,4,4,204,'old pending card',{now},{deadline},'',{now}),\
         ('training_booked',7,2,2,205,'old booked',{now},{deadline},'final',{now}),\
         ('training_waitlist',7,3,3,206,'old waitlist',{now},{deadline},'status',{now}),\
         ('training_reminder',7,2,2,207,'old reminder',{now},{deadline},'',{now}),\
         ('training',8,1,0,208,'deleted organizer',{now},{deadline},'',{now}),\
         ('training',8,7,7,209,'deleted player',{now},{deadline},'',{now});",
        now + 7200,
        now + 8000
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
        next_id: AtomicI64::new(1000),
    };
    let notifier = sqassit::event_notify::EventNotifier::default();

    // A title-only edit keeps existing status messages.
    profiles
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE trainings SET title='Squash <B>', training_type='Squash <B>' WHERE id=7",
            [],
        )
        .unwrap();
    training_notify::lifecycle(
        &profiles,
        &fake,
        &notifier,
        "https://example.test",
        7,
        Lifecycle::Edited {
            changed_time: false,
        },
        now,
    )
    .await
    .expect("training title edit");

    // Moving a training deletes and recreates status messages.
    profiles
        .db
        .lock()
        .unwrap()
        .execute("UPDATE trainings SET start_at=?1 WHERE id=7", [now + 10800])
        .unwrap();
    training_notify::lifecycle(
        &profiles,
        &fake,
        &notifier,
        "https://example.test",
        7,
        Lifecycle::Edited { changed_time: true },
        now,
    )
    .await
    .expect("training time edit");

    // Restoring edits old cards back and recreates the booked confirmation.
    profiles
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE trainings SET deleted_at=NULL, deleted_by=NULL WHERE id=8",
            [],
        )
        .unwrap();
    training_notify::lifecycle(
        &profiles,
        &fake,
        &notifier,
        "https://example.test",
        8,
        Lifecycle::Restored,
        now,
    )
    .await
    .expect("training restore");

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

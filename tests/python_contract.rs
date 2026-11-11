//! Opt-in comparison against the original Python application; no Telegram traffic.
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use chrono::{Datelike, Days, Utc};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqassit::{profile::Profiles, rate_limit::RateLimiter};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

fn signed(id: i64, name: &str, now: i64) -> String {
    let user = json!({"id":id,"first_name":name}).to_string();
    let data = format!("auth_date={now}\nuser={user}");
    let mut key = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
    key.update(b"123456:TEST-TOKEN");
    let mut signature = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes()).unwrap();
    signature.update(data.as_bytes());
    form_urlencoded::Serializer::new(String::new())
        .append_pair("auth_date", &now.to_string())
        .append_pair("user", &user)
        .append_pair("hash", &hex::encode(signature.finalize().into_bytes()))
        .finish()
}
fn normalize(value: &mut Value, test_start: i64) {
    match value {
        Value::String(text) => {
            // ICS files stamp the export time; the reference and candidate run seconds apart.
            if let Some(start) = text.find("DTSTAMP:") {
                let rest = &text[start + "DTSTAMP:".len()..];
                if let Some(end) = rest.find(['\r', '\n']) {
                    text.replace_range(start..start + "DTSTAMP:".len() + end, "DTSTAMP:<clock>");
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| normalize(v, test_start)),
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                match key.as_str() {
                    "seconds_left" => *item = json!("<clock>"),
                    "joined_at" | "created_at" | "last_at" => {
                        let joined = item.as_i64().expect("joined_at is an integer timestamp");
                        assert!(
                            (joined - test_start).abs() <= 60,
                            "joined_at is not near test time"
                        );
                        *item = json!("<clock>");
                    }
                    "ics_token" | "profile_token" => *item = json!("<scoped-token>"),
                    "avatar_url" => {
                        if let Some(url) = item.as_str() {
                            if let Some((path, _)) = url.split_once('?') {
                                *item = json!(format!("{path}?<token>"));
                            }
                        }
                    }
                    _ => normalize(item, test_start),
                }
            }
        }
        _ => {}
    }
}

fn normalize_deletion_times(value: &mut Value, test_start: i64) {
    if let Some(rows) = value.as_array_mut() {
        for row in rows {
            if let Some(timestamp) = row.get_mut("deleted_at") {
                if let Some(deleted) = timestamp.as_i64() {
                    assert!((deleted - test_start).abs() <= 60);
                    *timestamp = json!("<clock>");
                }
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
async fn same_profile_and_training_workflow_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let date = Utc::now()
        .with_timezone(&chrono_tz::Europe::Moscow)
        .date_naive()
        .checked_add_days(Days::new(2))
        .unwrap()
        .to_string();
    let weekday = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .expect("date")
        .weekday()
        .num_days_from_monday();
    let profile_token =
        sqassit::auth::issue_scoped_token("123456:TEST-TOKEN", "profile", 0, 42, now, 86400);
    let ics_token =
        sqassit::auth::issue_scoped_token("123456:TEST-TOKEN", "ics", 1, 42, now, 86400);
    let avatar_token =
        sqassit::auth::issue_scoped_token("123456:TEST-TOKEN", "avatar", 42, 42, now, 86400);
    let avatar_unknown =
        sqassit::auth::issue_scoped_token("123456:TEST-TOKEN", "avatar_real", 999, 999, now, 86400);
    let output = std::process::Command::new(python)
        .arg("tests/contract_oracle.py")
        .arg(now.to_string())
        .arg(&date)
        .arg(weekday.to_string())
        .output()
        .expect("run Python reference");
    assert!(
        output.status.success(),
        "Python reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");
    let expected_responses = std::mem::take(expected["responses"].as_array_mut().unwrap());
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute_batch("INSERT INTO coaches(telegram_id,created_at) VALUES(123,0); INSERT INTO users(telegram_id,first_name,is_coach,created_at) VALUES(123,'Тренер',1,0); INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES(2,43,'pending',0,0),(2,44,'pending',0,0); INSERT INTO match_players(match_id,user_id,status,created_at) VALUES(2,43,'pending',0),(2,44,'pending',0);").unwrap();
    let profiles = Profiles {
        db: Arc::new(Mutex::new(db)),
        bot_token: "123456:TEST-TOKEN".into(),
        owner: 123,
        timezone: chrono_tz::Europe::Moscow,
        require_event_approval: false,
        phone_required_from: None,
        booking_limiter: Arc::new(Mutex::new(RateLimiter::new(60, Duration::from_secs(60)))),
    };
    let app = sqassit::web::router(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
        false,
        Some(profiles.clone()),
        None,
    );
    let scenarios: Vec<(&str, &str, i64, Option<Value>)> = vec![
        ("GET", "/api/me", 0, None),
        ("GET", "/api/me", 42, None),
        (
            "PUT",
            "/api/me/profile",
            42,
            Some(json!({"about":"  Люблю сквош  ","display_mode":"animal","show_username":false})),
        ),
        (
            "PUT",
            "/api/me/notifications",
            42,
            Some(json!({"notify_matches":false})),
        ),
        ("POST", "/api/me/onboarding", 42, None),
        ("GET", "/api/me", 42, None),
        (
            "PUT",
            "/api/me/profile",
            42,
            Some(
                json!({"about":"Люблю сквош","display_mode":"animal","show_username":false,"phone":"+1"}),
            ),
        ),
        (
            "PUT",
            "/api/me/profile",
            42,
            Some(
                json!({"about":"Люблю сквош","display_mode":"animal","show_username":false,"phone":" +7 (999) 123-45-67 "}),
            ),
        ),
        ("GET", "/api/me", 42, None),
        // Avatar authorization and privacy without downloading from Telegram.
        ("GET", "/api/avatar/42?token=<avatar-token>", 0, None),
        ("GET", "/api/avatar/42/stale?token=<avatar-token>", 0, None),
        ("GET", "/api/avatar/42?token=bad", 0, None),
        ("GET", "/api/avatar/999?token=<avatar-unknown>", 0, None),
        ("GET", "/api/levels", 42, None),
        ("GET", "/api/coach/players", 42, None),
        ("POST", "/api/trainings/999/book", 42, None),
        ("POST", "/api/trainings/999/cancel", 42, None),
        ("POST", "/api/matches/999/join", 42, None),
        (
            "POST",
            "/api/coach/trainings",
            123,
            Some(json!({"date":date,"time":"12:00","training_type":"Squash","capacity":1})),
        ),
        ("GET", "/api/trainings?days=7", 42, None),
        ("POST", "/api/trainings/1/room", 42, None),
        ("POST", "/api/trainings/1/cancel", 42, None),
        ("POST", "/api/trainings/1/book", 42, None),
        ("GET", "/api/trainings?days=7", 42, None),
        ("POST", "/api/trainings/1/book", 43, None),
        ("POST", "/api/trainings/1/cancel", 42, None),
        ("GET", "/api/trainings?days=7", 43, None),
        ("POST", "/api/trainings/1/cancel", 43, None),
        (
            "POST",
            "/api/coach/trainings/1/participants",
            123,
            Some(json!({"user_id":42})),
        ),
        (
            "POST",
            "/api/coach/trainings/1/participants",
            123,
            Some(json!({"user_id":43})),
        ),
        ("GET", "/api/coach/trainings/1/participants", 123, None),
        (
            "POST",
            "/api/coach/trainings/1/participants/42/cancel",
            123,
            None,
        ),
        (
            "POST",
            "/api/coach/trainings/1/participants/42/restore",
            123,
            None,
        ),
        (
            "PATCH",
            "/api/coach/trainings/1/reminder",
            123,
            Some(json!({"enabled":true,"offset_min":60})),
        ),
        (
            "PATCH",
            "/api/coach/trainings/1",
            123,
            Some(json!({"date":date,"time":"14:00","training_type":"Updated"})),
        ),
        (
            "POST",
            "/api/coach/trainings",
            123,
            Some(json!({"date":date,"time":"14:00","training_type":"Updated"})),
        ),
        (
            "POST",
            "/api/coach/trainings",
            123,
            Some(json!({"date":"2000-01-01","time":"14:00"})),
        ),
        (
            "POST",
            "/api/coach/trainings",
            123,
            Some(json!({"date":date,"time":"18:00","training_type":"Levels","level":"Unknown"})),
        ),
        ("GET", "/calendar/1.ics?token=<ics-token>", 0, None),
        ("DELETE", "/api/coach/trainings/1", 123, None),
        ("POST", "/api/coach/trainings/1/restore", 123, None),
        ("GET", "/api/coach/trainings/1/participants", 123, None),
        (
            "POST",
            "/api/matches",
            42,
            Some(json!({"date":date,"time":"13:00","title":"Match"})),
        ),
        ("POST", "/api/matches/1/room", 43, None),
        ("DELETE", "/api/matches/1", 43, None),
        ("POST", "/api/matches/1/join", 43, None),
        ("GET", "/api/matches", 43, None),
        ("POST", "/api/matches/1/leave", 43, None),
        ("DELETE", "/api/matches/1", 42, None),
        ("POST", "/api/matches/1/join", 43, None),
        (
            "POST",
            "/api/messages/coach",
            42,
            Some(json!({"text":"  Привет тренер  ","training_id":1})),
        ),
        ("GET", "/api/coach/messages", 123, None),
        ("GET", "/api/coach/messages/42", 123, None),
        (
            "POST",
            "/api/coach/messages/42",
            123,
            Some(json!({"text":"Здравствуйте"})),
        ),
        ("GET", "/api/messages/thread", 42, None),
        ("GET", "/api/messages/thread", 43, None),
        ("GET", "/api/coach/messages/42", 43, None),
        ("GET", "/api/coach/players", 123, None),
        ("PUT", "/api/coach/players/42/notes", 123, Some(json!({}))),
        // Coach registry, club view, applications, coauthors and transfer.
        ("GET", "/api/coach/coaches", 123, None),
        ("GET", "/api/coach/club/trainings", 123, None),
        (
            "POST",
            "/api/coach/apply",
            43,
            Some(json!({"message":"Хочу тренировать"})),
        ),
        ("GET", "/api/coach/apply", 43, None),
        ("GET", "/api/coach/applications", 123, None),
        ("POST", "/api/coach/applications/43/approve", 123, None),
        ("GET", "/api/coach/apply", 43, None),
        ("GET", "/api/coach/coaches", 43, None),
        ("GET", "/api/coach/applications", 43, None),
        ("GET", "/api/players/<profile-token>", 43, None),
        (
            "PATCH",
            "/api/coach/coaches/43/manager",
            123,
            Some(json!({"enabled":true})),
        ),
        ("GET", "/api/coach/applications", 43, None),
        ("GET", "/api/coach/coaches", 123, None),
        ("DELETE", "/api/coach/coaches/123", 123, None),
        ("DELETE", "/api/coach/coaches/43", 123, None),
        ("GET", "/api/coach/coaches", 123, None),
        (
            "POST",
            "/api/coach/coaches",
            123,
            Some(json!({"telegram_id":43})),
        ),
        ("GET", "/api/coach/coaches", 43, None),
        ("GET", "/api/coach/trainings/1/coauthors", 123, None),
        (
            "POST",
            "/api/coach/trainings/1/coauthors",
            123,
            Some(json!({"telegram_id":43})),
        ),
        ("GET", "/api/coach/trainings/1/coauthors", 123, None),
        ("DELETE", "/api/coach/trainings/1/coauthors/43", 123, None),
        ("GET", "/api/coach/trainings/1/coauthors", 123, None),
        (
            "POST",
            "/api/coach/trainings/1/transfer",
            123,
            Some(json!({"telegram_id":43})),
        ),
        ("GET", "/api/coach/trainings", 43, None),
        (
            "POST",
            "/api/coach/trainings/1/transfer",
            43,
            Some(json!({"telegram_id":123})),
        ),
        ("GET", "/api/coach/trainings", 123, None),
        (
            "POST",
            "/api/matches",
            42,
            Some(json!({"date":date,"time":"15:00","title":"Second match"})),
        ),
        (
            "POST",
            "/api/matches/2/coauthors",
            42,
            Some(json!({"telegram_id":43})),
        ),
        ("GET", "/api/matches/2/coauthors", 43, None),
        ("DELETE", "/api/matches/2/coauthors/43", 42, None),
        ("POST", "/api/matches/2/requests/44/approve", 42, None),
        ("POST", "/api/matches/2/requests/43/reject", 42, None),
        ("GET", "/api/matches", 43, None),
        ("DELETE", "/api/matches/2", 42, None),
        (
            "POST",
            "/api/coach/apply",
            44,
            Some(json!({"message":"Ещё заявка"})),
        ),
        ("GET", "/api/coach/applications", 123, None),
        ("POST", "/api/coach/applications/44/reject", 123, None),
        ("GET", "/api/coach/apply", 44, None),
        ("POST", "/api/coach/applications/44/approve", 123, None),
        ("DELETE", "/api/coach/coaches/44", 123, None),
        ("GET", "/api/coach/coaches", 42, None),
        ("GET", "/api/coach/players/count", 123, None),
        // Coach profile, archives, batch creation and participant decisions.
        ("GET", "/api/coach/profile", 123, None),
        (
            "PUT",
            "/api/coach/profile",
            123,
            Some(json!({"bio":"  Кратко о тренере  "})),
        ),
        ("GET", "/api/coach/profile", 123, None),
        ("GET", "/api/coach/pending", 123, None),
        (
            "POST",
            "/api/coach/trainings/batch",
            123,
            Some(
                json!({"period_start":date,"period_end":date,"time":"16:00","weekdays":[weekday],"training_type":"Batch","capacity":1}),
            ),
        ),
        (
            "POST",
            "/api/coach/trainings/batch",
            123,
            Some(
                json!({"period_start":date,"period_end":"2000-01-01","time":"16:00","weekdays":[weekday]}),
            ),
        ),
        (
            "POST",
            "/api/coach/trainings/2/participants/44/approve",
            123,
            None,
        ),
        (
            "POST",
            "/api/coach/trainings/2/participants/43/reject",
            123,
            None,
        ),
        ("GET", "/api/coach/trainings/2/participants", 123, None),
        ("DELETE", "/api/coach/trainings/2", 123, None),
        ("GET", "/api/coach/deleted", 123, None),
        ("DELETE", "/api/coach/trainings/2/purge", 123, None),
        ("GET", "/api/coach/deleted", 123, None),
        ("GET", "/api/coach/history", 123, None),
        ("GET", "/api/coach/trainings", 123, None),
        ("GET", "/api/coach/pending", 123, None),
        // Feedback, export and confirmed account deletion.
        (
            "POST",
            "/api/feedback",
            42,
            Some(json!({"kind":"idea","text":"  Идея  ","page":"Матч"})),
        ),
        ("GET", "/api/me/export", 42, None),
        (
            "POST",
            "/api/feedback",
            42,
            Some(json!({"kind":"bug","text":"ok","page":"unknown"})),
        ),
        (
            "POST",
            "/api/feedback",
            42,
            Some(json!({"kind":"idea","text":"Третья идея","page":"Тренер"})),
        ),
        (
            "POST",
            "/api/feedback",
            42,
            Some(json!({"kind":"idea","text":"Четвёртая идея","page":"Тренер"})),
        ),
        ("DELETE", "/api/me", 44, None),
        ("DELETE", "/api/me?confirm=true", 44, None),
        // Authorization matrix: players, unauthenticated callers and non-manager coaches.
        ("GET", "/api/coach/trainings", 0, None),
        ("GET", "/api/coach/trainings", 42, None),
        ("GET", "/api/coach/pending", 42, None),
        ("GET", "/api/coach/history", 42, None),
        ("GET", "/api/coach/deleted", 42, None),
        ("GET", "/api/coach/profile", 42, None),
        ("PUT", "/api/coach/profile", 42, Some(json!({"bio":"x"}))),
        ("GET", "/api/coach/applications", 42, None),
        (
            "POST",
            "/api/coach/coaches",
            42,
            Some(json!({"telegram_id":42})),
        ),
        (
            "PATCH",
            "/api/coach/coaches/123/manager",
            42,
            Some(json!({"enabled":true})),
        ),
        (
            "POST",
            "/api/coach/coaches",
            43,
            Some(json!({"telegram_id":42})),
        ),
        (
            "PATCH",
            "/api/coach/coaches/123/manager",
            43,
            Some(json!({"enabled":true})),
        ),
        (
            "POST",
            "/api/coach/trainings/1/participants/42/approve",
            42,
            None,
        ),
        (
            "POST",
            "/api/coach/trainings/1/coauthors",
            42,
            Some(json!({"telegram_id":43})),
        ),
        (
            "POST",
            "/api/coach/trainings/1/transfer",
            42,
            Some(json!({"telegram_id":43})),
        ),
        ("DELETE", "/api/coach/trainings/1", 42, None),
        ("DELETE", "/api/coach/trainings/1/purge", 42, None),
        // Static pages and liveness; HTML is compared byte-for-byte.
        ("GET", "/", 0, None),
        ("GET", "/coach", 0, None),
        ("GET", "/match", 0, None),
        ("GET", "/tabs", 0, None),
        ("GET", "/healthz", 0, None),
    ];
    assert_eq!(scenarios.len(), expected_responses.len());
    for ((method, raw_path, id, body), mut reference) in
        scenarios.into_iter().zip(expected_responses)
    {
        let path = raw_path
            .replace("<profile-token>", &profile_token)
            .replace("<ics-token>", &ics_token)
            .replace("<avatar-token>", &avatar_token)
            .replace("<avatar-unknown>", &avatar_unknown);
        let mut request = Request::builder().method(method).uri(&path);
        if id != 0 {
            request = request.header(
                "x-init-data",
                signed(
                    id,
                    if id == 123 {
                        "Coach"
                    } else if id == 43 {
                        "Second"
                    } else if id == 44 {
                        "Applicant"
                    } else {
                        "Player"
                    },
                    now,
                ),
            );
        }
        let payload = if let Some(body) = body {
            request = request.header("content-type", "application/json");
            body.to_string()
        } else {
            String::new()
        };
        let response = app
            .clone()
            .oneshot(request.body(Body::from(payload)).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        let mut actual = json!({"status":status,"body":match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => value,
            Err(_) => Value::String(String::from_utf8_lossy(&bytes).into_owned()),
        }});
        normalize(&mut actual, now);
        normalize(&mut reference, now);
        assert_eq!(actual, reference, "{method} {path}");
    }
    let queries = [
        ("users", "SELECT telegram_id,first_name,is_coach,about,display_mode,show_username,phone,notifications_enabled,notify_matches,notify_trainings,notify_messages,onboarding_seen FROM users ORDER BY telegram_id"),
        ("trainings", "SELECT id,title,training_type,start_at,capacity,reminder_enabled,reminder_offset_min,reminder_sent,created_by,deleted_at FROM trainings ORDER BY id"),
        ("bookings", "SELECT training_id,user_id,status,position FROM bookings ORDER BY training_id,user_id"),
        ("matches", "SELECT id,title,description,location,start_at,duration_min,max_players,level,created_by,deleted_at FROM matches ORDER BY id"),
        ("match_players", "SELECT match_id,user_id,status FROM match_players ORDER BY match_id,user_id"),
        ("messages", "SELECT id,from_user,to_user,text FROM messages ORDER BY id"),
        ("coaches", "SELECT telegram_id,added_by,is_manager FROM coaches ORDER BY telegram_id"),
        ("coach_applications", "SELECT telegram_id,message,status,decided_by FROM coach_applications ORDER BY telegram_id"),
        ("event_coauthors", "SELECT event_type,event_id,user_id,added_by FROM event_coauthors ORDER BY event_type,event_id,user_id"),
        ("feedback", "SELECT user_id,kind,text,page FROM feedback ORDER BY id"),
    ];
    let db = profiles.db.lock().unwrap();
    for (name, sql) in queries {
        let mut statement = db.prepare(sql).unwrap();
        let names = statement
            .column_names()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let rows = statement
            .query_map([], |row| {
                let mut object = serde_json::Map::new();
                for (index, name) in names.iter().enumerate() {
                    let value = match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(v) => json!(v),
                        rusqlite::types::ValueRef::Real(v) => json!(v),
                        rusqlite::types::ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                        rusqlite::types::ValueRef::Blob(_) => panic!("unexpected blob in {name}"),
                    };
                    object.insert(name.clone(), value);
                }
                Ok(Value::Object(object))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        // Deletion timestamps are wall-clock dependent. The state must agree on
        // whether each record is deleted, while numeric timestamps are checked nearby.
        let mut actual = json!(rows);
        let mut reference = expected["state"][name].clone();
        normalize_deletion_times(&mut actual, now);
        normalize_deletion_times(&mut reference, now);
        assert_eq!(actual, reference, "database table {name}");
    }
}

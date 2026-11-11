//! Opt-in comparison against the original Python application; no Telegram traffic.
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use chrono::{Days, Utc};
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
    let output = std::process::Command::new(python)
        .arg("tests/contract_oracle.py")
        .arg(now.to_string())
        .arg(&date)
        .output()
        .expect("run Python reference");
    assert!(
        output.status.success(),
        "Python reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).expect("reference JSON");
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute_batch("INSERT INTO coaches(telegram_id,created_at) VALUES(123,0); INSERT INTO users(telegram_id,first_name,is_coach,created_at) VALUES(123,'Тренер',1,0);").unwrap();
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
        Some(profiles),
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
    ];
    assert_eq!(scenarios.len(), expected.len());
    for ((method, path, id, body), mut reference) in scenarios.into_iter().zip(expected) {
        let mut request = Request::builder().method(method).uri(path);
        if id != 0 {
            request = request.header(
                "x-init-data",
                signed(
                    id,
                    if id == 123 {
                        "Coach"
                    } else if id == 43 {
                        "Second"
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
        let mut actual =
            json!({"status":status,"body":serde_json::from_slice::<Value>(&bytes).unwrap()});
        normalize(&mut actual, now);
        normalize(&mut reference, now);
        assert_eq!(actual, reference, "{method} {path}");
    }
}

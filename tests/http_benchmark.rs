//! Opt-in timing for schedule responses on a synthetic in-memory database.
use axum::{
    body::Body,
    http::{header, Request},
};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection};
use sha2::Sha256;
use sqassit::profile::Profiles;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

type HmacSha256 = Hmac<Sha256>;

fn sign(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn init_data(token: &str, user: i64, now: i64) -> String {
    let user = format!(r#"{{"id":{user},"first_name":"Bench"}}"#);
    let auth_date = now.to_string();
    let check = format!("auth_date={auth_date}\nuser={user}");
    let secret = sign(b"WebAppData", token.as_bytes());
    let hash = hex::encode(sign(&secret, check.as_bytes()));
    let encoded = form_urlencoded::Serializer::new(String::new())
        .append_pair("auth_date", &auth_date)
        .append_pair("user", &user)
        .append_pair("hash", &hash)
        .finish();
    encoded
}

fn seed(db: &mut Connection) {
    let tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_matches,notify_trainings,notify_messages,created_at) \
         VALUES(1,'Coach',1,1,1,1,0)",
        [],
    )
    .unwrap();
    {
        let mut users = tx
            .prepare("INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_matches,notify_trainings,notify_messages,created_at) VALUES(?1,?2,1,1,1,1,0)")
            .unwrap();
        for id in 1000..6000 {
            users.execute(params![id, format!("Player {id}")]).unwrap();
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    {
        let mut trainings = tx
            .prepare("INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,level,created_by,created_at) VALUES(?1,?2,'Squash',?3,60,8,'Beginner',1,?4)")
            .unwrap();
        for id in 1..=2000 {
            trainings
                .execute(params![
                    id,
                    format!("Training {id}"),
                    now + 3600 + id * 300,
                    now
                ])
                .unwrap();
        }
    }
    {
        let mut bookings = tx
            .prepare("INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES(?1,?2,'booked',?3,?4)")
            .unwrap();
        for id in 1..=2000 {
            for slot in 0..6 {
                bookings
                    .execute(params![id, 1000 + (id * 6 + slot) % 5000, slot, now])
                    .unwrap();
            }
        }
    }
    tx.commit().unwrap();
}

#[tokio::test]
#[ignore = "manual timing run; use --release --ignored --nocapture"]
async fn schedule_response_latency() {
    let mut db = Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    seed(&mut db);
    let token = "123456:BENCH-TOKEN";
    let profiles = Profiles {
        db: Arc::new(Mutex::new(db)),
        bot_token: token.into(),
        owner: 1,
        timezone: chrono_tz::Europe::Moscow,
        require_event_approval: false,
        phone_required_from: None,
        booking_limiter: Arc::new(Mutex::new(sqassit::rate_limit::RateLimiter::new(
            100_000,
            Duration::from_secs(60),
        ))),
    };
    let static_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static");
    let app = sqassit::web::router(static_path, false, Some(profiles), None);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let init = init_data(token, 1000, now);
    for path in ["/api/me", "/api/trainings?days=7", "/api/trainings?days=30"] {
        let request = || {
            Request::builder()
                .uri(path)
                .header(header::HeaderName::from_static("x-init-data"), &init)
                .body(Body::empty())
                .unwrap()
        };
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let mut timings = Vec::new();
        for _ in 0..20 {
            let started = Instant::now();
            let response = app.clone().oneshot(request()).await.unwrap();
            assert_eq!(response.status(), 200, "{path}");
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert!(!body.is_empty());
            timings.push(started.elapsed());
        }
        timings.sort();
        let average = timings.iter().sum::<Duration>() / timings.len() as u32;
        println!(
            "{path}: avg {:.1} ms, p95 {:.1} ms, max {:.1} ms",
            average.as_secs_f64() * 1000.0,
            timings[18].as_secs_f64() * 1000.0,
            timings[19].as_secs_f64() * 1000.0
        );
    }
}

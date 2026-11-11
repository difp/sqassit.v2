//! Opt-in comparison of security and cache headers with the Python server.
use axum::{body::Body, http::Request};
use serde_json::{json, Value};
use sqassit::profile::Profiles;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

const PATHS: [&str; 8] = [
    "/",
    "/coach",
    "/match",
    "/tabs",
    "/healthz",
    "/api/me",
    "/static/telegram-web-app.js",
    "/static/squashtabs/index.html",
];
const HEADERS: [&str; 6] = [
    "content-security-policy",
    "x-content-type-options",
    "referrer-policy",
    "permissions-policy",
    "cache-control",
    "content-type",
];

#[tokio::test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
async fn same_security_headers_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let output = std::process::Command::new(python)
        .arg("tests/headers_oracle.py")
        .output()
        .expect("run Python header reference");
    assert!(
        output.status.success(),
        "Python header reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");

    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
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
    let static_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static");
    let app = sqassit::web::router(static_path, false, Some(profiles), None);
    let mut responses = Vec::new();
    for path in PATHS {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let headers = HEADERS
            .iter()
            .map(|name| {
                (
                    (*name).to_owned(),
                    match response.headers().get(*name) {
                        Some(value) => json!(value.to_str().unwrap()),
                        None => Value::Null,
                    },
                )
            })
            .collect::<serde_json::Map<_, _>>();
        responses.push(json!({
            "path": path,
            "status": response.status().as_u16(),
            "headers": headers,
        }));
    }
    assert_eq!(json!(responses), expected["responses"]);
}

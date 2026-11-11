//! Opt-in unauthenticated route matrix against the Python reference.
//!
//! The reference reads its own route decorators and requests every route
//! without initData. A route missing from the Rust router (or one that
//! authorizes differently) shows up as a status or body difference.
use axum::{body::Body, http::Request};
use serde_json::Value;
use sqassit::profile::Profiles;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

#[tokio::test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
async fn same_unauthenticated_route_matrix_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let output = std::process::Command::new(python)
        .arg("tests/authz_oracle.py")
        .output()
        .expect("run Python route reference");
    assert!(
        output.status.success(),
        "Python route reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("reference JSON");
    let responses = expected["responses"].as_array().expect("responses");
    assert_eq!(responses.len(), 75, "the reference route inventory changed");

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
    let app = sqassit::web::router(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
        false,
        Some(profiles),
        None,
    );
    for route in responses {
        let method = route["method"].as_str().unwrap();
        let path = route["path"].as_str().unwrap();
        let expected_status = route["status"].as_u64().unwrap();
        let expected_body = &route["json"];
        let request_body = match route["request_json"].as_object() {
            Some(_) => Body::from(serde_json::to_vec(&route["request_json"]).unwrap()),
            None => Body::empty(),
        };
        let mut request = Request::builder().method(method).uri(path);
        if route["request_json"].is_object() {
            request = request.header("content-type", "application/json");
        }
        let response = app
            .clone()
            .oneshot(request.body(request_body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16() as u64;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice::<Value>(&body).ok();
        assert_eq!(
            status, expected_status,
            "{method} {path} status: Python {expected_status}, Rust {status}, body {body:?}"
        );
        if expected_body.as_object().is_some() {
            assert_eq!(
                body.as_ref(),
                Some(expected_body),
                "{method} {path} body differs from Python"
            );
        }
    }
}

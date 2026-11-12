//! Opt-in route/role authorization matrix against the Python reference.
//!
//! The reference reads its own route decorators and requests every route
//! without initData and as a player, a non-manager coach and the manager
//! owner, each against a fresh database. A route missing from the Rust router
//! or one that authorizes a role differently shows up as a difference.
use axum::{body::Body, http::Request};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqassit::profile::Profiles;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

/// The same initData signature the reference oracle builds.
fn signed(id: i64, now: i64) -> String {
    let user = json!({"id":id,"first_name":"User"}).to_string();
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

fn reference() -> Value {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON")
        .expect("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let output = std::process::Command::new(python)
        .arg("tests/authz_oracle.py")
        .arg(now.to_string())
        .output()
        .expect("run Python route reference");
    assert!(
        output.status.success(),
        "Python route reference failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("reference JSON")
}

async fn app_for_role(role_id: i64) -> axum::Router {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    if role_id > 0 {
        db.execute_batch(
            "INSERT INTO coaches(telegram_id,created_at) VALUES(43,0),(123,0);
             INSERT INTO users(telegram_id,first_name,is_coach,created_at)
             VALUES(42,'Player',0,0),(43,'Second',1,0),(123,'Тренер',1,0);",
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
    sqassit::web::router(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
        false,
        Some(profiles),
        None,
    )
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    init_data: Option<&str>,
    request_body: Option<&Value>,
) -> (u64, Option<Value>) {
    let request_body = request_body.filter(|body| body.is_object());
    let mut request = Request::builder().method(method).uri(path);
    if let Some(init) = init_data {
        request = request.header("x-init-data", init);
    }
    if request_body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let body = match request_body {
        Some(body) => Body::from(serde_json::to_vec(body).unwrap()),
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16() as u64;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).ok())
}

#[tokio::test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
async fn same_unauthenticated_route_matrix_as_python() {
    let expected = reference();
    let responses: Vec<Value> = expected["responses"]
        .as_array()
        .expect("responses")
        .iter()
        .filter(|route| route["role"] == "none")
        .cloned()
        .collect();
    assert_eq!(responses.len(), 75, "the reference route inventory changed");

    let app = app_for_role(0).await;
    for route in &responses {
        let method = route["method"].as_str().unwrap();
        let path = route["path"].as_str().unwrap();
        let expected_status = route["status"].as_u64().unwrap();
        let expected_body = &route["json"];
        let (status, body) = send(&app, method, path, None, Some(&route["request_json"])).await;
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

/// Authenticated roles are compared by status: bodies already have deep
/// per-workflow comparisons in `python_contract`, and side effects here would
/// make body snapshots order-sensitive for little extra coverage.
#[tokio::test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
async fn same_authenticated_route_roles_as_python() {
    let expected = reference();
    for role in ["player", "coach", "manager"] {
        let role_id = match role {
            "player" => 42,
            "coach" => 43,
            _ => 123,
        };
        let routes: Vec<Value> = expected["responses"]
            .as_array()
            .expect("responses")
            .iter()
            .filter(|route| route["role"] == role)
            .cloned()
            .collect();
        assert_eq!(routes.len(), 75, "the reference route inventory changed");
        let app = app_for_role(role_id).await;
        let init = signed(
            role_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        );
        for route in &routes {
            let method = route["method"].as_str().unwrap();
            let path = route["path"].as_str().unwrap();
            let expected_status = route["status"].as_u64().unwrap();
            let (status, body) = send(
                &app,
                method,
                path,
                Some(&init),
                Some(&route["request_json"]),
            )
            .await;
            assert_eq!(
                status, expected_status,
                "[{role}] {method} {path} status: Python {expected_status}, Rust {status}, body {body:?}"
            );
        }
    }
}

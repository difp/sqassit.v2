//! Experimental HTTP shell for the migrated UI. The API and bot are unfinished.
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use minijinja::{context, Environment};
use serde_json::json;
use std::path::PathBuf;
use tower_http::services::ServeDir;

const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors https://telegram.org https://web.telegram.org https://*.telegram.org https://*.t.me";
const TABS_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src 'self' data: blob:; connect-src 'self' https://ntfy.sh; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'self' https://telegram.org https://web.telegram.org https://*.telegram.org https://*.t.me";

#[derive(Clone)]
pub struct WebState {
    pub allow_dev_init_data: bool,
    pub profiles: Option<crate::profile::Profiles>,
}

fn render(template: &str, allow_dev_init_data: bool) -> Result<String, minijinja::Error> {
    let mut env = Environment::new();
    env.add_template(
        "base.html",
        include_str!("../legacy/app/templates/base.html"),
    )?;
    env.add_template(
        "index.html",
        include_str!("../legacy/app/templates/index.html"),
    )?;
    env.add_template(
        "coach.html",
        include_str!("../legacy/app/templates/coach.html"),
    )?;
    env.add_template(
        "match.html",
        include_str!("../legacy/app/templates/match.html"),
    )?;
    env.get_template(template)?
        .render(context! { allow_dev_init_data })
}

async fn page(state: WebState, template: &'static str) -> Response {
    match render(template, state.allow_dev_init_data) {
        Ok(html) => Html(html).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Template rendering failed",
        )
            .into_response(),
    }
}
async fn player(State(state): State<WebState>) -> Response {
    page(state, "index.html").await
}
async fn coach(State(state): State<WebState>) -> Response {
    page(state, "coach.html").await
}
async fn matches(State(state): State<WebState>) -> Response {
    page(state, "match.html").await
}
async fn tabs() -> Response {
    let mut response =
        Html(include_str!("../legacy/app/static/squashtabs/index.html")).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(TABS_CSP),
    );
    response
}
async fn health() -> Json<serde_json::Value> {
    Json(json!({"ok":true}))
}
// Never claim readiness while bot, scheduler and API are missing.
async fn ready() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"detail":"Rust migration is incomplete."})),
    )
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn init_data(headers: &HeaderMap) -> String {
    headers
        .get("x-init-data")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}
async fn profile_work<F>(state: WebState, headers: HeaderMap, operation: F) -> Response
where
    F: FnOnce(
            crate::profile::Profiles,
            String,
            i64,
        ) -> Result<serde_json::Value, crate::profile::ProfileError>
        + Send
        + 'static,
{
    let Some(profiles) = state.profiles else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Profile service is not configured."})),
        )
            .into_response();
    };
    let init = init_data(&headers);
    match tokio::task::spawn_blocking(move || operation(profiles, init, now())).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(crate::profile::ProfileError::Forbidden(message))) => (StatusCode::FORBIDDEN, Json(json!({"detail":message}))).into_response(),
        Ok(Err(crate::profile::ProfileError::Unauthorized(message))) => {
            (StatusCode::UNAUTHORIZED, Json(json!({"detail":message}))).into_response()
        }
        Ok(Err(crate::profile::ProfileError::Duplicate(id))) => (StatusCode::CONFLICT, Json(json!({"ok":false,"error":"Такая тренировка уже есть в расписании.","duplicate_id":id}))).into_response(),
        Ok(Err(crate::profile::ProfileError::Rule(status, message))) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"ok":false,"error":message})),
        )
            .into_response(),
        Ok(Err(crate::profile::ProfileError::RateLimited(seconds))) => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, seconds.to_string())],
            Json(json!({"detail":"Слишком много запросов. Попробуйте позже."})),
        )
            .into_response(),
        Ok(Err(crate::profile::ProfileError::Invalid(message))) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"ok":false,"error":message})),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"detail":"Внутренняя ошибка сервера"})),
        )
            .into_response(),
    }
}
async fn api_me(State(state): State<WebState>, headers: HeaderMap) -> Response {
    profile_work(state, headers, |p, i, n| p.me(&i, n)).await
}
async fn api_onboarding(State(state): State<WebState>, headers: HeaderMap) -> Response {
    profile_work(state, headers, |p, i, n| p.onboarding(&i, n)).await
}
async fn api_notifications(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(payload): Json<crate::profile::NotificationSettings>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        p.notifications(&i, n, payload)
    })
    .await
}
async fn api_update_profile(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(payload): Json<crate::profile::PlayerProfile>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        p.update_profile(&i, n, payload)
    })
    .await
}
#[derive(serde::Deserialize)]
struct TrainingQuery {
    #[serde(default = "default_days")]
    days: i64,
}
fn default_days() -> i64 {
    7
}
async fn api_trainings(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<TrainingQuery>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::trainings::list(&p, &i, n, query.days, p.timezone)
    })
    .await
}
async fn api_book(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::trainings::book(&p, &i, n, id)
    })
    .await
}
async fn api_cancel(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::trainings::cancel(&p, &i, n, id)
    })
    .await
}
async fn coach_list(State(state): State<WebState>, headers: HeaderMap) -> Response {
    profile_work(state, headers, |p, i, n| {
        crate::coach::list_active(&p, &i, n)
    })
    .await
}
async fn coach_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<crate::coach::NewTraining>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach::create(&p, &i, n, body)
    })
    .await
}
async fn coach_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach::lifecycle(&p, &i, n, id, crate::coach::Action::Delete)
    })
    .await
}
async fn coach_restore(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach::lifecycle(&p, &i, n, id, crate::coach::Action::Restore)
    })
    .await
}
async fn coach_purge(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach::lifecycle(&p, &i, n, id, crate::coach::Action::Purge)
    })
    .await
}
async fn headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(CSP));
    h.entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    h.entry(header::REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.entry("permissions-policy")
        .or_insert(HeaderValue::from_static(
            "geolocation=(), microphone=(), camera=()",
        ));
    if path.starts_with("/static/") {
        h.entry(header::CACHE_CONTROL)
            .or_insert(HeaderValue::from_static("public, max-age=604800"));
    } else if path == "/" || path == "/coach" {
        h.entry(header::CACHE_CONTROL)
            .or_insert(HeaderValue::from_static("no-cache"));
    }
    response
}

pub fn router(
    static_path: PathBuf,
    allow_dev_init_data: bool,
    profiles: Option<crate::profile::Profiles>,
) -> Router {
    Router::new()
        .route("/", get(player))
        .route("/coach", get(coach))
        .route("/match", get(matches))
        .route("/tabs", get(tabs))
        .route("/api/me", get(api_me))
        .route("/api/trainings", get(api_trainings))
        .route("/api/coach/trainings", get(coach_list).post(coach_create))
        .route(
            "/api/coach/trainings/{id}",
            axum::routing::delete(coach_delete),
        )
        .route("/api/coach/trainings/{id}/restore", post(coach_restore))
        .route(
            "/api/coach/trainings/{id}/purge",
            axum::routing::delete(coach_purge),
        )
        .route("/api/trainings/{id}/book", post(api_book))
        .route("/api/trainings/{id}/cancel", post(api_cancel))
        .route("/api/me/onboarding", post(api_onboarding))
        .route("/api/me/profile", put(api_update_profile))
        .route("/api/me/notifications", put(api_notifications))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .nest_service("/static", ServeDir::new(static_path))
        .layer(middleware::from_fn(headers))
        .with_state(WebState {
            allow_dev_init_data,
            profiles,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_templates_render_with_expected_dev_flag() {
        for name in ["index.html", "coach.html", "match.html"] {
            let production = render(name, false).unwrap();
            let development = render(name, true).unwrap();
            assert!(production.contains("const DEV_INIT_DATA_ENABLED = false"));
            assert!(development.contains("const DEV_INIT_DATA_ENABLED = true"));
            assert!(production.contains("Telegram"));
        }
    }
    #[tokio::test]
    async fn routes_expose_preview_and_refuse_readiness() {
        use axum::body::{to_bytes, Body};
        use tower::ServiceExt;
        let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static");
        let app = router(static_dir, false, None);
        for (url, status) in [
            ("/", StatusCode::OK),
            ("/coach", StatusCode::OK),
            ("/match", StatusCode::OK),
            ("/tabs", StatusCode::OK),
            ("/healthz", StatusCode::OK),
            ("/readyz", StatusCode::SERVICE_UNAVAILABLE),
            ("/api/me", StatusCode::SERVICE_UNAVAILABLE),
            ("/static/telegram-web-app.js", StatusCode::OK),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{url}");
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            if url == "/tabs" {
                assert!(response.headers()[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap()
                    .contains("https://ntfy.sh"));
            }
            if url == "/readyz" {
                let body = to_bytes(response.into_body(), 4096).await.unwrap();
                assert!(std::str::from_utf8(&body).unwrap().contains("incomplete"));
            }
        }
    }
    #[tokio::test]
    async fn authenticated_profile_onboarding_and_notification_flow() {
        use axum::body::{to_bytes, Body};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        use tower::ServiceExt;
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO users(telegram_id,first_name,display_mode,created_at) VALUES (314,'Секретное имя','animal',0)", []).unwrap();
        db.execute(
            "INSERT INTO trainings(id,title,start_at,created_at) VALUES (7,'Вечерняя',?1,0)",
            [now() + 3600],
        )
        .unwrap();
        db.execute(
            "INSERT INTO bookings(training_id,user_id,status,created_at) VALUES (7,314,'booked',0)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO trainings(id,title,start_at,created_at) VALUES (8,'Утренняя',?1,0)",
            [now() + 6 * 3600],
        )
        .unwrap();
        let profiles = crate::profile::Profiles {
            db: std::sync::Arc::new(std::sync::Mutex::new(db)),
            bot_token: "123456:TEST-TOKEN".into(),
            owner: 123,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: std::sync::Arc::new(std::sync::Mutex::new(
                crate::rate_limit::RateLimiter::new(60, std::time::Duration::from_secs(60)),
            )),
        };
        let mut coach_profiles = profiles.clone();
        coach_profiles.owner = 42;
        let coach_app = router(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
            false,
            Some(coach_profiles),
        );
        let app = router(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
            false,
            Some(profiles),
        );
        let timestamp = now().to_string();
        let user = r#"{"id":42,"first_name":"Иван"}"#;
        let data = format!("auth_date={timestamp}\nuser={user}");
        let mut mac = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
        mac.update(b"123456:TEST-TOKEN");
        let mut mac = Hmac::<Sha256>::new_from_slice(&mac.finalize().into_bytes()).unwrap();
        mac.update(data.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());
        let signed = form_urlencoded::Serializer::new(String::new())
            .append_pair("auth_date", &timestamp)
            .append_pair("user", user)
            .append_pair("hash", &signature)
            .finish();
        async fn send(
            app: Router,
            method: &str,
            uri: &str,
            token: &str,
            body: &str,
        ) -> (StatusCode, serde_json::Value) {
            let req = Request::builder()
                .method(method)
                .uri(uri)
                .header("x-init-data", token)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap();
            let r = app.oneshot(req).await.unwrap();
            let status = r.status();
            let b = to_bytes(r.into_body(), 65536).await.unwrap();
            (status, serde_json::from_slice(&b).unwrap())
        }
        assert_eq!(
            send(app.clone(), "GET", "/api/me", "", "").await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body) = send(app.clone(), "GET", "/api/me", &signed, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["user"]["telegram_id"], 42);
        assert_eq!(body["user"]["onboarding_seen"], false);
        assert_eq!(body["user"]["notify_messages"], true);
        assert_eq!(
            send(app.clone(), "POST", "/api/me/onboarding", &signed, "")
                .await
                .0,
            StatusCode::OK
        );
        let (_, body) = send(
            app.clone(),
            "PUT",
            "/api/me/notifications",
            &signed,
            r#"{"notify_messages":false}"#,
        )
        .await;
        assert_eq!(body["notify_messages"], false);
        assert_eq!(body["notify_matches"], true);
        let (_, body) = send(app.clone(), "GET", "/api/me", &signed, "").await;
        assert_eq!(body["user"]["onboarding_seen"], true);
        assert_eq!(body["user"]["notify_messages"], false);
        let (status, body) = send(
            app.clone(),
            "PUT",
            "/api/me/profile",
            &signed,
            r#"{"about":"  Люблю сквош  ","display_mode":"animal","phone":"+7 (999) 123-45-67"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["profile"]["about"], "Люблю сквош");
        assert_eq!(body["profile"]["display_mode"], "animal");
        assert_eq!(body["profile"]["phone"], "+79991234567");
        assert_eq!(body["profile"]["notify_messages"], false);
        let (status, _) = send(
            app.clone(),
            "PUT",
            "/api/me/profile",
            &signed,
            r#"{"phone":"123"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (_, body) = send(app.clone(), "GET", "/api/me", &signed, "").await;
        assert_eq!(body["user"]["display_mode"], "animal");
        let (status, schedule) =
            send(app.clone(), "GET", "/api/trainings?days=7", &signed, "").await;
        assert_eq!(status, StatusCode::OK);
        let event = &schedule["trainings"][0];
        assert_eq!(event["title"], "Вечерняя");
        assert_eq!(event["booked_count"], 1);
        let person = &event["participants"][0];
        assert_eq!(person["anonymous"], true);
        assert_ne!(person["first_name"], "Секретное имя");
        assert_eq!(person["avatar_url"], "");
        assert!(person.get("telegram_id").is_none());
        let (status, booking) =
            send(app.clone(), "POST", "/api/trainings/7/book", &signed, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(booking["status"], "booked");
        assert_eq!(
            send(app.clone(), "POST", "/api/trainings/7/cancel", &signed, "")
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(app.clone(), "POST", "/api/trainings/8/book", &signed, "")
                .await
                .0,
            StatusCode::OK
        );
        let (status, cancelled) =
            send(app.clone(), "POST", "/api/trainings/8/cancel", &signed, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cancelled["promoted"], serde_json::Value::Null);
        assert_eq!(
            send(app.clone(), "POST", "/api/trainings/8/cancel", &signed, "")
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(app.clone(), "POST", "/api/trainings/999/book", &signed, "")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let date = (chrono::Utc::now() + chrono::Duration::days(2))
            .with_timezone(&chrono_tz::Europe::Moscow)
            .format("%Y-%m-%d")
            .to_string();
        let payload = json!({"date":date,"time":"12:00","training_type":"Тест"}).to_string();
        assert_eq!(
            send(
                app.clone(),
                "POST",
                "/api/coach/trainings",
                &signed,
                &payload
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let (status, created) = send(
            coach_app.clone(),
            "POST",
            "/api/coach/trainings",
            &signed,
            &payload,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let created_id = created["id"].as_i64().unwrap();
        assert_eq!(
            send(
                coach_app.clone(),
                "POST",
                "/api/coach/trainings",
                &signed,
                &payload
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let (_, listing) = send(
            coach_app.clone(),
            "GET",
            "/api/coach/trainings",
            &signed,
            "",
        )
        .await;
        assert!(listing["trainings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == created_id));
        let endpoint = format!("/api/coach/trainings/{created_id}");
        assert_eq!(
            send(coach_app.clone(), "DELETE", &endpoint, &signed, "")
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            send(
                coach_app,
                "POST",
                &format!("{endpoint}/restore"),
                &signed,
                ""
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            send(app, "GET", "/api/me", &signed.replace("42", "43"), "")
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
}

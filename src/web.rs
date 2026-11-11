//! Experimental HTTP shell for the migrated UI. The API and bot are unfinished.
use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
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

pub fn router(static_path: PathBuf, allow_dev_init_data: bool) -> Router {
    Router::new()
        .route("/", get(player))
        .route("/coach", get(coach))
        .route("/match", get(matches))
        .route("/tabs", get(tabs))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .nest_service("/static", ServeDir::new(static_path))
        .layer(middleware::from_fn(headers))
        .with_state(WebState {
            allow_dev_init_data,
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
        let app = router(static_dir, false);
        for (url, status) in [
            ("/", StatusCode::OK),
            ("/coach", StatusCode::OK),
            ("/match", StatusCode::OK),
            ("/tabs", StatusCode::OK),
            ("/healthz", StatusCode::OK),
            ("/readyz", StatusCode::SERVICE_UNAVAILABLE),
            ("/api/me", StatusCode::NOT_FOUND),
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
}

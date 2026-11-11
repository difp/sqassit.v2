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
    pub bot: Option<crate::updates::BotRuntime>,
    pub avatar: Option<(crate::avatar::AvatarCache, crate::telegram::TelegramClient)>,
    pub messages: Option<crate::messages::Messages>,
    pub match_api: crate::match_api::MatchApi,
    pub event_notifier: crate::event_notify::EventNotifier,
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
        Ok(result) => profile_response(result),
        Err(_) => profile_response(Err(crate::profile::ProfileError::Storage)),
    }
}
fn profile_response(result: Result<serde_json::Value, crate::profile::ProfileError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(crate::profile::ProfileError::NotFound(message)) => (StatusCode::NOT_FOUND,Json(json!({"detail":message}))).into_response(),
        Err(crate::profile::ProfileError::Forbidden(message)) => (StatusCode::FORBIDDEN, Json(json!({"detail":message}))).into_response(),
        Err(crate::profile::ProfileError::Unauthorized(message)) => {
            (StatusCode::UNAUTHORIZED, Json(json!({"detail":message}))).into_response()
        }
        Err(crate::profile::ProfileError::Duplicate(id)) => (StatusCode::CONFLICT, Json(json!({"ok":false,"error":"Такая тренировка уже есть в расписании.","duplicate_id":id}))).into_response(),
        Err(crate::profile::ProfileError::Rule(status, message)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"ok":false,"error":message})),
        )
            .into_response(),
        Err(crate::profile::ProfileError::RateLimited(seconds)) => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, seconds.to_string())],
            Json(json!({"detail":"Слишком много запросов. Попробуйте позже."})),
        )
            .into_response(),
        Err(crate::profile::ProfileError::Invalid(message)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"ok":false,"error":message})),
        )
            .into_response(),
        Err(crate::profile::ProfileError::Storage) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"detail":"Внутренняя ошибка сервера"})),
        )
            .into_response(),
    }
}
async fn message_work<F>(state: WebState, headers: HeaderMap, operation: F) -> Response
where
    F: FnOnce(
            crate::messages::Messages,
            String,
            i64,
        ) -> Result<serde_json::Value, crate::profile::ProfileError>
        + Send
        + 'static,
{
    let Some(service) = state.messages else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Message service is not configured."})),
        )
            .into_response();
    };
    let init = init_data(&headers);
    match tokio::task::spawn_blocking(move || operation(service, init, now())).await {
        Ok(result) => profile_response(result),
        Err(_) => profile_response(Err(crate::profile::ProfileError::Storage)),
    }
}
async fn message_send<F>(state: WebState, headers: HeaderMap, operation: F) -> Response
where
    F: FnOnce(
            crate::messages::Messages,
            String,
            i64,
        ) -> Result<crate::messages::Sent, crate::profile::ProfileError>
        + Send
        + 'static,
{
    let Some(service) = state.messages else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Message service is not configured."})),
        )
            .into_response();
    };
    let init = init_data(&headers);
    let profiles = service.profiles.clone();
    let bot = state.bot;
    match tokio::task::spawn_blocking(move || operation(service, init, now())).await {
        Ok(Ok(sent)) => {
            if let Some(bot) = bot {
                tokio::spawn(async move {
                    let _ = crate::chat_notify::notify(
                        &profiles,
                        &bot.client,
                        &sent,
                        &bot.base_url,
                        now(),
                    )
                    .await;
                });
            }
            Json(json!({"ok":true})).into_response()
        }
        Ok(Err(error)) => profile_response(Err(error)),
        Err(_) => profile_response(Err(crate::profile::ProfileError::Storage)),
    }
}
async fn player_message(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<crate::messages::MessageIn>,
) -> Response {
    message_send(state, headers, move |s, i, n| s.send_to_coach(&i, n, body)).await
}
async fn player_thread(State(state): State<WebState>, headers: HeaderMap) -> Response {
    message_work(state, headers, |s, i, n| s.thread(&i, n, None)).await
}
async fn coach_conversations(State(state): State<WebState>, headers: HeaderMap) -> Response {
    message_work(state, headers, |s, i, n| s.conversations(&i, n)).await
}
async fn coach_thread(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user): Path<i64>,
) -> Response {
    message_work(state, headers, move |s, i, n| s.thread(&i, n, Some(user))).await
}
async fn coach_message(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user): Path<i64>,
    Json(body): Json<crate::messages::MessageIn>,
) -> Response {
    message_send(state, headers, move |s, i, n| {
        s.coach_reply(&i, n, user, body)
    })
    .await
}
async fn api_me(State(state): State<WebState>, headers: HeaderMap) -> Response {
    profile_work(state, headers, |p, i, n| p.me(&i, n)).await
}
async fn api_public_profile(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        p.public_profile(&i, &token, n)
    })
    .await
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
#[derive(Default, serde::Deserialize)]
struct AvatarQuery {
    #[serde(default)]
    token: String,
}
async fn avatar_response(
    state: WebState,
    headers: HeaderMap,
    id: i64,
    version: Option<String>,
    query: AvatarQuery,
) -> Response {
    let Some(profiles) = state.profiles else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Profile service is not configured."})),
        )
            .into_response();
    };
    let Some((cache, client)) = state.avatar else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Avatar service is not configured."})),
        )
            .into_response();
    };
    match cache
        .get(
            &profiles,
            &client,
            id,
            &query.token,
            version.as_deref(),
            now(),
        )
        .await
    {
        Ok(avatar) => {
            let etag = format!("\"{}\"", avatar.version);
            let caching = if avatar.immutable {
                "private, max-age=604800, immutable"
            } else {
                "private, no-cache"
            };
            let matches = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(',').map(str::trim).any(|candidate| {
                        candidate == "*" || candidate == etag || candidate == format!("W/{etag}")
                    })
                });
            if matches {
                (
                    StatusCode::NOT_MODIFIED,
                    [
                        (header::CACHE_CONTROL, caching.to_owned()),
                        (header::ETAG, etag),
                    ],
                )
                    .into_response()
            } else {
                (
                    [
                        (
                            header::CONTENT_TYPE,
                            crate::avatar::media_type(&avatar.content).to_owned(),
                        ),
                        (header::CACHE_CONTROL, caching.to_owned()),
                        (header::ETAG, etag),
                    ],
                    avatar.content.as_ref().clone(),
                )
                    .into_response()
            }
        }
        Err(error) => {
            let (status, detail) = match error {
                crate::avatar::AvatarError::Unauthorized => {
                    (StatusCode::UNAUTHORIZED, "Требуется авторизация.")
                }
                crate::avatar::AvatarError::Hidden => (StatusCode::NOT_FOUND, "Аватарка скрыта."),
                crate::avatar::AvatarError::NotFound => {
                    (StatusCode::NOT_FOUND, "Аватарка недоступна.")
                }
                crate::avatar::AvatarError::TooLarge => {
                    (StatusCode::BAD_GATEWAY, "Аватарка слишком большая.")
                }
                crate::avatar::AvatarError::Unavailable => {
                    (StatusCode::BAD_GATEWAY, "Telegram недоступен.")
                }
                crate::avatar::AvatarError::Database => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Внутренняя ошибка сервера",
                ),
            };
            (status, Json(json!({"detail":detail}))).into_response()
        }
    }
}
async fn avatar_legacy(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<AvatarQuery>,
) -> Response {
    avatar_response(state, headers, id, None, query).await
}
async fn avatar_versioned(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path((id, version)): Path<(i64, String)>,
    Query(query): Query<AvatarQuery>,
) -> Response {
    avatar_response(state, headers, id, Some(version), query).await
}
#[derive(Default, serde::Deserialize)]
struct CalendarQuery {
    #[serde(default)]
    token: String,
    #[serde(default, rename = "initData")]
    init_data: String,
}
async fn api_calendar(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(filename): Path<String>,
    Query(query): Query<CalendarQuery>,
) -> Response {
    let Some(id) = filename
        .strip_suffix(".ics")
        .and_then(|v| v.parse::<i64>().ok())
    else {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    };
    let Some(profiles) = state.profiles else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Profile service is not configured."})),
        )
            .into_response();
    };
    let init = init_data(&headers);
    let allow_dev = state.allow_dev_init_data;
    match tokio::task::spawn_blocking(move || {
        crate::calendar::generate(
            &profiles,
            id,
            &query.token,
            &init,
            &query.init_data,
            allow_dev,
            now(),
        )
    })
    .await
    {
        Ok(Ok(ics)) => (
            [
                (
                    header::CONTENT_TYPE,
                    "text/calendar; charset=utf-8".to_owned(),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"squash-{id}.ics\""),
                ),
            ],
            ics,
        )
            .into_response(),
        Ok(Err(crate::profile::ProfileError::Unauthorized(message))) => {
            (StatusCode::UNAUTHORIZED, Json(json!({"detail":message}))).into_response()
        }
        Ok(Err(crate::profile::ProfileError::NotFound(message))) => {
            (StatusCode::NOT_FOUND, Json(json!({"detail":message}))).into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"detail":"Внутренняя ошибка сервера"})),
        )
            .into_response(),
    }
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
async fn match_join(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    match_attendance(state, headers, id, crate::match_api::Action::Join).await
}
async fn match_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<crate::match_api::NewMatch>,
) -> Response {
    let api = state.match_api.clone();
    profile_work(state, headers, move |p, i, n| api.create(&p, &i, n, body)).await
}
async fn match_room(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let api = state.match_api.clone();
    profile_work(state, headers, move |p, i, n| api.room(&p, &i, n, id)).await
}
async fn match_coauthors(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coauthors::list(&p, &i, n, id)
    })
    .await
}
async fn match_add_coauthor(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<crate::coauthors::CoauthorInput>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coauthors::add(&p, &i, n, id, body)
    })
    .await
}
async fn match_remove_coauthor(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path((id, user)): Path<(i64, i64)>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coauthors::remove(&p, &i, n, id, user)
    })
    .await
}
async fn match_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let api = state.match_api.clone();
    profile_work(state, headers, move |p, i, n| api.delete(&p, &i, n, id)).await
}
#[derive(serde::Deserialize)]
struct MatchQuery {
    #[serde(default = "match_default_days")]
    days: i64,
}
fn match_default_days() -> i64 {
    30
}
async fn match_list(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<MatchQuery>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::match_list::list(&p, &i, n, query.days)
    })
    .await
}
async fn match_leave(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    match_attendance(state, headers, id, crate::match_api::Action::Leave).await
}
async fn match_approve(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path((id, user)): Path<(i64, i64)>,
) -> Response {
    match_attendance(state, headers, id, crate::match_api::Action::Approve(user)).await
}
async fn match_reject(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path((id, user)): Path<(i64, i64)>,
) -> Response {
    match_attendance(state, headers, id, crate::match_api::Action::Reject(user)).await
}
async fn match_attendance(
    state: WebState,
    headers: HeaderMap,
    id: i64,
    action: crate::match_api::Action,
) -> Response {
    let Some(profiles) = state.profiles else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Profile service is not configured."})),
        )
            .into_response();
    };
    let api = state.match_api;
    let init = init_data(&headers);
    let worker_profiles = profiles.clone();
    let result = tokio::task::spawn_blocking(move || {
        let timestamp = now();
        let actor = worker_profiles.user_id(&init, timestamp)?.0;
        let (value, promoted) = api.attendance(&worker_profiles, &init, timestamp, id, action)?;
        Ok::<_, crate::profile::ProfileError>((value, promoted, actor))
    })
    .await;
    match result {
        Ok(Ok((value, promoted, actor))) => {
            if let Some(bot) = state.bot {
                let notifier = state.event_notifier;
                let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let (target, status, change) = match action {
                    crate::match_api::Action::Join if status == "pending" => (
                        actor,
                        "pending".to_owned(),
                        crate::match_notify::Change::Request,
                    ),
                    crate::match_api::Action::Join => (
                        actor,
                        "joined".to_owned(),
                        crate::match_notify::Change::Event,
                    ),
                    crate::match_api::Action::Leave => {
                        (actor, "left".to_owned(), crate::match_notify::Change::Event)
                    }
                    crate::match_api::Action::Approve(target) => (
                        target,
                        status.to_owned(),
                        crate::match_notify::Change::Event,
                    ),
                    crate::match_api::Action::Reject(target) => (
                        target,
                        "rejected".to_owned(),
                        crate::match_notify::Change::Event,
                    ),
                };
                tokio::spawn(async move {
                    if crate::match_notify::notify(
                        &profiles,
                        &bot.client,
                        &notifier,
                        crate::match_notify::MatchChange {
                            base: &bot.base_url,
                            id,
                            user: target,
                            status: &status,
                            change,
                            now: now(),
                        },
                    )
                    .await
                    .is_err()
                    {
                        eprintln!("Match notification delivery failed");
                    }
                    if let Some(target) = promoted {
                        if crate::match_notify::notify(
                            &profiles,
                            &bot.client,
                            &notifier,
                            crate::match_notify::MatchChange {
                                base: &bot.base_url,
                                id,
                                user: target,
                                status: "promoted",
                                change: crate::match_notify::Change::Event,
                                now: now(),
                            },
                        )
                        .await
                        .is_err()
                        {
                            eprintln!("Match promotion notification delivery failed");
                        }
                    }
                });
            }
            Json(value).into_response()
        }
        Ok(Err(error)) => profile_response(Err(error)),
        Err(_) => profile_response(Err(crate::profile::ProfileError::Storage)),
    }
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
async fn coach_edit(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<crate::coach::EditTraining>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach::edit(&p, &i, n, id, body)
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
async fn coach_pending(State(state): State<WebState>, headers: HeaderMap) -> Response {
    profile_work(state, headers, |p, i, n| {
        let list = crate::coach::list_active(&p, &i, n)?;
        let total = list["trainings"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v["pending_count"].as_i64())
                    .sum::<i64>()
            })
            .unwrap_or(0);
        Ok(json!({"ok":true,"pending_count":total}))
    })
    .await
}
async fn coach_participants(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach_participants::list(&p, &i, n, id)
    })
    .await
}
async fn coach_add_participant(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<crate::coach_participants::ParticipantIn>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach_participants::change(
            &p,
            &i,
            n,
            id,
            body.user_id,
            crate::coach_participants::Action::Add,
        )
    })
    .await
}
async fn coach_participant_action(
    state: WebState,
    headers: HeaderMap,
    id: i64,
    user: i64,
    action: crate::coach_participants::Action,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach_participants::change(&p, &i, n, id, user, action)
    })
    .await
}
macro_rules! participant_route {
    ($name:ident,$action:ident) => {
        async fn $name(
            State(state): State<WebState>,
            headers: HeaderMap,
            Path((id, user)): Path<(i64, i64)>,
        ) -> Response {
            coach_participant_action(
                state,
                headers,
                id,
                user,
                crate::coach_participants::Action::$action,
            )
            .await
        }
    };
}
participant_route!(coach_approve, Approve);
participant_route!(coach_reject, Reject);
participant_route!(coach_remove, Cancel);
participant_route!(coach_reinstate, Restore);
async fn coach_reminder(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<crate::coach_participants::ReminderIn>,
) -> Response {
    profile_work(state, headers, move |p, i, n| {
        crate::coach_participants::reminder(&p, &i, n, id, body)
    })
    .await
}
async fn telegram_webhook(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(update): Json<serde_json::Value>,
) -> Response {
    let Some(bot) = state.bot else {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    };
    let received = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (status, value) = crate::updates::webhook(
        &bot.profiles,
        &bot.client,
        crate::updates::WebhookConfig {
            mode: bot.mode,
            secret: &bot.secret,
            base_url: &bot.base_url,
        },
        received,
        &update,
        now(),
    )
    .await;
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(value),
    )
        .into_response()
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
    bot: Option<crate::updates::BotRuntime>,
) -> Router {
    let proxy = std::env::var("BOT_PROXY").unwrap_or_default();
    let messages = profiles
        .as_ref()
        .map(|p| crate::messages::Messages::new(p.clone()));
    let avatar = profiles
        .as_ref()
        .and_then(|p| crate::telegram::TelegramClient::new(p.bot_token.clone(), Some(&proxy)).ok())
        .map(|client| (crate::avatar::AvatarCache::default(), client));
    Router::new()
        .route("/", get(player))
        .route("/coach", get(coach))
        .route("/match", get(matches))
        .route("/tabs", get(tabs))
        .route("/api/me", get(api_me))
        .route("/api/messages/coach", post(player_message))
        .route("/api/messages/thread", get(player_thread))
        .route("/api/coach/messages", get(coach_conversations))
        .route(
            "/api/coach/messages/{user}",
            get(coach_thread).post(coach_message),
        )
        .route("/api/players/{token}", get(api_public_profile))
        .route("/api/avatar/{id}", get(avatar_legacy))
        .route("/api/avatar/{id}/{version}", get(avatar_versioned))
        .route("/api/trainings", get(api_trainings))
        .route("/api/matches", get(match_list).post(match_create))
        .route("/api/matches/{id}", axum::routing::delete(match_delete))
        .route("/api/matches/{id}/room", post(match_room))
        .route(
            "/api/matches/{id}/coauthors",
            get(match_coauthors).post(match_add_coauthor),
        )
        .route(
            "/api/matches/{id}/coauthors/{user}",
            axum::routing::delete(match_remove_coauthor),
        )
        .route("/api/matches/{id}/join", post(match_join))
        .route("/api/matches/{id}/leave", post(match_leave))
        .route(
            "/api/matches/{id}/requests/{user}/approve",
            post(match_approve),
        )
        .route(
            "/api/matches/{id}/requests/{user}/reject",
            post(match_reject),
        )
        .route("/calendar/{filename}", get(api_calendar))
        .route("/api/coach/trainings", get(coach_list).post(coach_create))
        .route("/api/coach/pending", get(coach_pending))
        .route(
            "/api/coach/trainings/{id}/participants",
            get(coach_participants).post(coach_add_participant),
        )
        .route(
            "/api/coach/trainings/{id}/participants/{user}/approve",
            post(coach_approve),
        )
        .route(
            "/api/coach/trainings/{id}/participants/{user}/reject",
            post(coach_reject),
        )
        .route(
            "/api/coach/trainings/{id}/participants/{user}/cancel",
            post(coach_remove),
        )
        .route(
            "/api/coach/trainings/{id}/participants/{user}/restore",
            post(coach_reinstate),
        )
        .route(
            "/api/coach/trainings/{id}/reminder",
            axum::routing::patch(coach_reminder),
        )
        .route(
            "/api/coach/trainings/{id}",
            axum::routing::delete(coach_delete).patch(coach_edit),
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
        .route("/telegram/webhook", post(telegram_webhook))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .nest_service("/static", ServeDir::new(static_path))
        .layer(middleware::from_fn(headers))
        .with_state(WebState {
            allow_dev_init_data,
            profiles,
            bot,
            avatar,
            messages,
            match_api: crate::match_api::MatchApi::default(),
            event_notifier: crate::event_notify::EventNotifier::default(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn match_attendance_requires_manager_and_promotes_waitlist() {
        use axum::body::{to_bytes, Body};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        use tower::ServiceExt;
        fn signed(id: i64) -> String {
            let timestamp = now().to_string();
            let user = format!(r#"{{"id":{id},"first_name":"Player"}}"#);
            let data = format!("auth_date={timestamp}\nuser={user}");
            let mut mac = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
            mac.update(b"test-token");
            let mut mac = Hmac::<Sha256>::new_from_slice(&mac.finalize().into_bytes()).unwrap();
            mac.update(data.as_bytes());
            form_urlencoded::Serializer::new(String::new())
                .append_pair("auth_date", &timestamp)
                .append_pair("user", &user)
                .append_pair("hash", &hex::encode(mac.finalize().into_bytes()))
                .finish()
        }
        async fn request(app: Router, uri: &str, token: &str) -> (StatusCode, serde_json::Value) {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("x-init-data", token)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), 65536).await.unwrap();
            (status, serde_json::from_slice(&body).unwrap())
        }
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute(
            "INSERT INTO matches(id,start_at,max_players,created_by,created_at) VALUES(1,?1,1,1,0)",
            [now() + 3600],
        )
        .unwrap();
        db.execute("INSERT INTO event_coauthors(event_type,event_id,user_id,created_at) VALUES('match',1,4,0)",[]).unwrap();
        let profiles = crate::profile::Profiles {
            db: std::sync::Arc::new(std::sync::Mutex::new(db)),
            bot_token: "test-token".into(),
            owner: 1,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: true,
            phone_required_from: None,
            booking_limiter: std::sync::Arc::new(std::sync::Mutex::new(
                crate::rate_limit::RateLimiter::new(60, std::time::Duration::from_secs(60)),
            )),
        };
        let app = router(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
            false,
            Some(profiles.clone()),
            None,
        );
        let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/matches")
                    .header("x-init-data", signed(1))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"date":"2099-01-01","time":"12:00"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let data: serde_json::Value =
            serde_json::from_slice(&to_bytes(created.into_body(), 65536).await.unwrap()).unwrap();
        let created_id = data["id"].as_i64().unwrap();
        let status: String = profiles
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT status FROM match_players WHERE match_id=?1 AND user_id=1",
                [created_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "booked");
        assert_eq!(
            request(app.clone(), "/api/matches/1/join", "").await.0,
            StatusCode::UNAUTHORIZED
        );
        for id in [2, 3] {
            assert_eq!(
                request(app.clone(), "/api/matches/1/join", &signed(id))
                    .await
                    .1["status"],
                "pending"
            );
        }
        profiles
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE users SET username='Example' WHERE telegram_id=3",
                [],
            )
            .unwrap();
        async fn coauthor_action(
            app: Router,
            method: &str,
            uri: &str,
            actor: i64,
            body: &str,
        ) -> (StatusCode, serde_json::Value) {
            let response = app
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("x-init-data", signed(actor))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_owned()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            (
                status,
                serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap())
                    .unwrap(),
            )
        }
        let endpoint = "/api/matches/1/coauthors";
        assert_eq!(
            coauthor_action(app.clone(), "POST", endpoint, 2, r#"{"telegram_id":3}"#)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let added = coauthor_action(
            app.clone(),
            "POST",
            endpoint,
            1,
            r#"{"username":"@example"}"#,
        )
        .await;
        assert_eq!(added.1["telegram_id"], 3);
        assert_eq!(added.1["created"], true);
        assert_eq!(
            coauthor_action(app.clone(), "POST", endpoint, 1, r#"{"telegram_id":3}"#)
                .await
                .1["created"],
            false
        );
        assert_eq!(
            coauthor_action(app.clone(), "DELETE", "/api/matches/1/coauthors/3", 3, "")
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            coauthor_action(app.clone(), "DELETE", "/api/matches/1/coauthors/3", 1, "")
                .await
                .1["removed"],
            true
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/room", &signed(2))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let code = request(app.clone(), "/api/matches/1/room", &signed(4))
            .await
            .1["code"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(code.len(), 11);
        assert!(code.starts_with("SQ-"));
        let authors = coauthor_action(app.clone(), "GET", endpoint, 2, "").await.1;
        assert_eq!(authors["can_manage"], false);
        assert_eq!(authors["coauthors"][0]["telegram_id"], 4);
        assert_eq!(
            request(app.clone(), "/api/matches/1/room", &signed(1))
                .await
                .1["code"],
            code
        );
        async fn listing(app: Router, token: &str) -> serde_json::Value {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/api/matches")
                        .header("x-init-data", token)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap()
        }
        let public = listing(app.clone(), &signed(2)).await;
        let card = &public["matches"][0];
        assert_eq!(card["pending_count"], 2);
        assert_eq!(card["pending_requests"].as_array().unwrap().len(), 0);
        let manager = listing(app.clone(), &signed(1)).await;
        assert_eq!(
            manager["matches"][0]["pending_requests"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            manager["matches"][0]["pending_requests"][0]["telegram_id"],
            2
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/requests/2/approve", &signed(3))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/requests/2/approve", &signed(1))
                .await
                .1["status"],
            "booked"
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/room", &signed(2))
                .await
                .1["code"],
            code
        );
        profiles
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE users SET display_mode='animal',first_name='Private' WHERE telegram_id=2",
                [],
            )
            .unwrap();
        let public = listing(app.clone(), &signed(3)).await;
        let person = &public["matches"][0]["participants"][0];
        assert_eq!(person["anonymous"], true);
        assert_ne!(person["first_name"], "Private");
        assert!(person.get("telegram_id").is_none());
        assert_eq!(
            request(app.clone(), "/api/matches/1/requests/3/approve", &signed(4))
                .await
                .1["status"],
            "waitlist"
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/leave", &signed(2))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/join", &signed(3))
                .await
                .1["status"],
            "booked"
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/requests/3/reject", &signed(1))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(app.clone(), "/api/matches/1/leave", &signed(2))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        async fn delete(app: Router, id: i64) -> StatusCode {
            app.oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/matches/1")
                    .header("x-init-data", signed(id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
        assert_eq!(delete(app.clone(), 4).await, StatusCode::NOT_FOUND);
        assert_eq!(delete(app.clone(), 1).await, StatusCode::OK);
        assert_eq!(delete(app.clone(), 1).await, StatusCode::NOT_FOUND);
        assert_eq!(
            request(app.clone(), "/api/matches/1/room", &signed(1))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
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
        let app = router(static_dir, false, None, None);
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
        let coach_db = coach_profiles.db.clone();
        let coach_app = router(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
            false,
            Some(coach_profiles),
            None,
        );
        let app = router(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
            false,
            Some(profiles),
            None,
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
        coach_db.lock().unwrap().execute("UPDATE users SET username='secret_handle',about='bio',show_username=0 WHERE telegram_id=314",[]).unwrap();
        let ics_token =
            crate::auth::issue_scoped_token("123456:TEST-TOKEN", "ics", 7, 42, now(), 86400);
        let calendar_request = |uri: String, init: &str| {
            Request::builder()
                .uri(uri)
                .header("x-init-data", init)
                .body(Body::empty())
                .unwrap()
        };
        let calendar = app
            .clone()
            .oneshot(calendar_request(
                format!("/calendar/7.ics?token={ics_token}"),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(calendar.status(), StatusCode::OK);
        assert_eq!(
            calendar.headers()[header::CONTENT_TYPE],
            "text/calendar; charset=utf-8"
        );
        assert_eq!(
            calendar.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"squash-7.ics\""
        );
        let body = to_bytes(calendar.into_body(), 65536).await.unwrap();
        let ics = std::str::from_utf8(&body).unwrap();
        assert!(ics.contains("UID:squash-training-7@squashbot\r\n"));
        assert!(ics.contains("TRIGGER:-PT30M\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert_eq!(
            app.clone()
                .oneshot(calendar_request(
                    "/calendar/7.ics?token=bad".into(),
                    &signed
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(calendar_request("/calendar/7.ics".into(), ""))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(calendar_request("/calendar/7.ics".into(), &signed))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("initData", &signed)
            .finish();
        assert_eq!(
            app.clone()
                .oneshot(calendar_request(format!("/calendar/7.ics?{query}"), ""))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let wrong =
            crate::auth::issue_scoped_token("123456:TEST-TOKEN", "ics", 8, 42, now(), 86400);
        assert_eq!(
            app.clone()
                .oneshot(calendar_request(
                    format!("/calendar/7.ics?token={wrong}"),
                    ""
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let missing =
            crate::auth::issue_scoped_token("123456:TEST-TOKEN", "ics", 999, 42, now(), 86400);
        assert_eq!(
            app.clone()
                .oneshot(calendar_request(
                    format!("/calendar/999.ics?token={missing}"),
                    ""
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let profile_token =
            crate::auth::issue_scoped_token("123456:TEST-TOKEN", "profile", 0, 314, now(), 86400);
        let profile_url = format!("/api/players/{profile_token}");
        let (status, public) = send(app.clone(), "GET", &profile_url, &signed, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(public["profile"]["anonymous"], true);
        assert_eq!(public["profile"]["username"], serde_json::Value::Null);
        assert!(public["profile"].get("telegram_id").is_none());
        assert_ne!(public["profile"]["first_name"], "Секретное имя");
        let (_, coach_view) = send(coach_app.clone(), "GET", &profile_url, &signed, "").await;
        assert_eq!(coach_view["profile"]["username"], "secret_handle");
        assert_eq!(
            send(app.clone(), "GET", "/api/players/invalid", &signed, "")
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(app.clone(), "GET", &profile_url, "", "").await.0,
            StatusCode::UNAUTHORIZED
        );

        fn sign_user(id: i64, at: i64) -> String {
            let user = format!("{{\"id\":{id},\"first_name\":\"U{id}\"}}");
            let date = at.to_string();
            let data = format!("auth_date={date}\nuser={user}");
            let mut key = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
            key.update(b"123456:TEST-TOKEN");
            let mut mac = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes()).unwrap();
            mac.update(data.as_bytes());
            let hash = hex::encode(mac.finalize().into_bytes());
            form_urlencoded::Serializer::new(String::new())
                .append_pair("auth_date", &date)
                .append_pair("user", &user)
                .append_pair("hash", &hash)
                .finish()
        }
        let admin = sign_user(123, now());
        let second_coach = sign_user(777, now());
        coach_db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO coaches(telegram_id,created_at) VALUES(777,0)",
                [],
            )
            .unwrap();
        assert_eq!(
            send(
                app.clone(),
                "POST",
                "/api/messages/coach",
                &signed,
                r#"{"text":"hello coach"}"#
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            send(app.clone(), "GET", "/api/coach/messages/42", &signed, "")
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let (_, private) = send(app.clone(), "GET", "/api/coach/messages/42", &admin, "").await;
        assert_eq!(private["messages"][0]["text"], "hello coach");
        let (_, isolated) = send(
            app.clone(),
            "GET",
            "/api/coach/messages/42",
            &second_coach,
            "",
        )
        .await;
        assert_eq!(isolated["messages"].as_array().unwrap().len(), 0);
        assert_eq!(
            send(
                app.clone(),
                "POST",
                "/api/coach/messages/42",
                &admin,
                r#"{"text":"private reply"}"#
            )
            .await
            .0,
            StatusCode::OK
        );
        let (_, thread) = send(app.clone(), "GET", "/api/messages/thread", &signed, "").await;
        assert_eq!(thread["messages"].as_array().unwrap().len(), 2);
        assert_eq!(thread["messages"][1]["text"], "private reply");
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
        let participants = format!("{endpoint}/participants");
        let (status, added) = send(
            coach_app.clone(),
            "POST",
            &participants,
            &signed,
            r#"{"user_id":314}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(added["status"], "booked");
        let (_, listed) = send(coach_app.clone(), "GET", &participants, &signed, "").await;
        assert_eq!(listed["participants"][0]["anonymous"], true);
        assert_ne!(listed["participants"][0]["first_name"], "Секретное имя");
        let remove = format!("{participants}/314/cancel");
        assert_eq!(
            send(coach_app.clone(), "POST", &remove, &signed, "")
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            send(
                coach_app.clone(),
                "POST",
                &format!("{participants}/314/restore"),
                &signed,
                ""
            )
            .await
            .0,
            StatusCode::OK
        );
        coach_db.lock().unwrap().execute("INSERT INTO bookings(training_id,user_id,status,created_at) VALUES (?1,42,'pending',0)",[created_id]).unwrap();
        let (_, pending) = send(coach_app.clone(), "GET", "/api/coach/pending", &signed, "").await;
        assert_eq!(pending["pending_count"], 1);
        let (status, approved) = send(
            coach_app.clone(),
            "POST",
            &format!("{participants}/42/approve"),
            &signed,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(approved["status"], "booked");
        assert_eq!(
            send(
                coach_app.clone(),
                "PATCH",
                &format!("{endpoint}/reminder"),
                &signed,
                r#"{"enabled":true,"offset_min":60}"#
            )
            .await
            .0,
            StatusCode::OK
        );
        let edit_body =
            json!({"date":date,"time":"13:00","training_type":"Новое время"}).to_string();
        assert_eq!(
            send(coach_app.clone(), "PATCH", &endpoint, &signed, &edit_body)
                .await
                .0,
            StatusCode::OK
        );
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
    #[tokio::test]
    async fn webhook_route_checks_secret_before_dispatch() {
        use axum::body::{to_bytes, Body};
        use tower::ServiceExt;
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        let profiles = crate::profile::Profiles {
            db: std::sync::Arc::new(std::sync::Mutex::new(db)),
            bot_token: "test-token".into(),
            owner: 1,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: std::sync::Arc::new(std::sync::Mutex::new(
                crate::rate_limit::RateLimiter::new(60, std::time::Duration::from_secs(60)),
            )),
        };
        let secret = "01234567890123456789012345678901";
        let bot = crate::updates::BotRuntime {
            profiles: profiles.clone(),
            client: crate::telegram::TelegramClient::new("test-token".into(), None).unwrap(),
            mode: crate::updates::BotMode::Webhook,
            secret: secret.into(),
            base_url: "https://example.test".into(),
        };
        let app = router(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("legacy/app/static"),
            false,
            Some(profiles),
            Some(bot),
        );
        let payload = r#"{"callback_query":{"id":"x","from":{"id":1},"data":"unknown:action"}}"#;
        let request = |token: &str| {
            Request::builder()
                .method("POST")
                .uri("/telegram/webhook")
                .header("content-type", "application/json")
                .header("x-telegram-bot-api-secret-token", token)
                .body(Body::from(payload))
                .unwrap()
        };
        let denied = app.clone().oneshot(request("bad")).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(denied.into_body(), 4096).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("Forbidden"));
        let retry = app.oneshot(request(secret)).await.unwrap();
        assert_eq!(retry.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    #[tokio::test]
    async fn avatar_response_enforces_privacy_and_conditional_cache() {
        struct Source;
        impl crate::avatar::AvatarSource for Source {
            async fn profile_photo(
                &self,
                _: i64,
            ) -> Result<Option<String>, crate::avatar::AvatarError> {
                Ok(None)
            }
            async fn download(
                &self,
                _: &str,
            ) -> Result<Option<Vec<u8>>, crate::avatar::AvatarError> {
                Ok(Some(b"GIF89atest".to_vec()))
            }
        }
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO users(telegram_id,avatar_file_id,display_mode,created_at) VALUES(42,'file','name',0)",[]).unwrap();
        let profiles = crate::profile::Profiles {
            db: std::sync::Arc::new(std::sync::Mutex::new(db)),
            bot_token: "test-token".into(),
            owner: 1,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: std::sync::Arc::new(std::sync::Mutex::new(
                crate::rate_limit::RateLimiter::new(60, std::time::Duration::from_secs(60)),
            )),
        };
        let cache = crate::avatar::AvatarCache::default();
        let token = crate::auth::issue_scoped_token("test-token", "avatar", 42, 42, now(), 604800);
        let version = profiles.avatar_version(42, Some("file"));
        cache
            .get(&profiles, &Source, 42, &token, Some(&version), now())
            .await
            .unwrap();
        let state = WebState {
            allow_dev_init_data: false,
            profiles: Some(profiles.clone()),
            bot: None,
            messages: None,
            match_api: crate::match_api::MatchApi::default(),
            event_notifier: crate::event_notify::EventNotifier::default(),
            avatar: Some((
                cache,
                crate::telegram::TelegramClient::new("test-token".into(), None).unwrap(),
            )),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str(&format!("W/\"{version}\"")).unwrap(),
        );
        let result = avatar_response(
            state.clone(),
            headers,
            42,
            Some(version.clone()),
            AvatarQuery {
                token: token.clone(),
            },
        )
        .await;
        assert_eq!(result.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            result.headers()[header::CACHE_CONTROL],
            "private, max-age=604800, immutable"
        );
        assert_eq!(
            avatar_response(
                state.clone(),
                HeaderMap::new(),
                42,
                None,
                AvatarQuery {
                    token: "bad".into()
                }
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        profiles
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE users SET display_mode='animal' WHERE telegram_id=42",
                [],
            )
            .unwrap();
        assert_eq!(
            avatar_response(
                state,
                HeaderMap::new(),
                42,
                Some(version),
                AvatarQuery { token }
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }
}

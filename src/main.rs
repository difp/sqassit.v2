//! Development-only server: deliberately reports not ready until migration completes.
use chrono::TimeZone;
use std::{env, net::SocketAddr, path::PathBuf};

fn phone_cutoff(
    raw: &str,
    timezone: chrono_tz::Tz,
) -> Result<Option<i64>, Box<dyn std::error::Error>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if let Ok(value) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(value.timestamp()));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).expect("valid midnight"))
        })?;
    let value = timezone
        .from_local_datetime(&naive)
        .earliest()
        .ok_or("PHONE_REQUIRED_FROM is an invalid local time")?;
    Ok(Some(value.timestamp()))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address: SocketAddr = env::var("SQASSIT_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8000".to_owned())
        .parse()?;
    let static_path = PathBuf::from(
        env::var("SQASSIT_STATIC_DIR").unwrap_or_else(|_| "legacy/app/static".to_owned()),
    );
    if !static_path.is_dir() {
        return Err(format!("Static directory not found: {}", static_path.display()).into());
    }
    let profiles = match (env::var("BOT_TOKEN"), env::var("COACH_TELEGRAM_ID")) {
        (Ok(token), Ok(owner)) if !token.trim().is_empty() => {
            let owner: i64 = owner.trim().parse()?;
            let timezone: chrono_tz::Tz = env::var("TIMEZONE")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Europe/Moscow".to_owned())
                .trim()
                .parse()?;
            let cutoff = phone_cutoff(
                &env::var("PHONE_REQUIRED_FROM").unwrap_or_default(),
                timezone,
            )?;
            let approval = env::var("REQUIRE_EVENT_APPROVAL")
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            let path = PathBuf::from(
                env::var("DB_PATH").unwrap_or_else(|_| "./data/squash.db".to_owned()),
            );
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let db = sqassit::storage::open(path)?;
            Some(sqassit::profile::Profiles {
                db: std::sync::Arc::new(std::sync::Mutex::new(db)),
                bot_token: token.trim().to_owned(),
                owner,
                timezone,
                require_event_approval: matches!(approval.as_str(), "1" | "true" | "yes" | "on"),
                phone_required_from: cutoff,
                booking_limiter: std::sync::Arc::new(std::sync::Mutex::new(
                    sqassit::rate_limit::RateLimiter::new(60, std::time::Duration::from_secs(60)),
                )),
            })
        }
        (Err(_), Err(_)) => None,
        _ => return Err("BOT_TOKEN and COACH_TELEGRAM_ID must both be set".into()),
    };
    let dev = env::var("ALLOW_DEV_INIT_DATA")
        .unwrap_or_default()
        .to_lowercase();
    let allow_dev = matches!(dev.as_str(), "1" | "true" | "yes" | "on");
    eprintln!("SQASSIT Rust migration preview: incomplete; do not use in production. Listening on {address}");
    let listener = tokio::net::TcpListener::bind(address).await?;
    // Explicit opt-in while the rest of the bot is unfinished.
    let enable_reminders = matches!(
        env::var("SQASSIT_ENABLE_REMINDERS")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    );
    let reminder_task = if enable_reminders {
        let service = profiles
            .clone()
            .ok_or("BOT_TOKEN and COACH_TELEGRAM_ID are required for reminders")?;
        let proxy = env::var("BOT_PROXY").unwrap_or_default();
        let client =
            sqassit::telegram::TelegramClient::new(service.bot_token.clone(), Some(&proxy))
                .map_err(|_| "Invalid Telegram client or BOT_PROXY")?;
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if sqassit::reminders::run_tick(&service, &client, now)
                    .await
                    .is_err()
                {
                    eprintln!("Reminder tick failed; it will be retried");
                }
                if sqassit::reminders::cleanup_expired(&service, &client, now)
                    .await
                    .is_err()
                {
                    eprintln!("Reminder cleanup failed; it will be retried");
                }
            }
        }))
    } else {
        None
    };
    let server_result = axum::serve(
        listener,
        sqassit::web::router(static_path, allow_dev, profiles),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await;
    if let Some(task) = reminder_task {
        task.abort();
        let _ = task.await;
    }
    server_result?;
    Ok(())
}

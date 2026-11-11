//! Development-only server: deliberately reports not ready until migration completes.
use std::{env, net::SocketAddr, path::PathBuf};

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
                timezone: env::var("TIMEZONE")
                    .unwrap_or_else(|_| "Europe/Moscow".to_owned())
                    .parse()?,
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
    axum::serve(
        listener,
        sqassit::web::router(static_path, allow_dev, profiles),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

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
    let dev = env::var("ALLOW_DEV_INIT_DATA")
        .unwrap_or_default()
        .to_lowercase();
    let allow_dev = matches!(dev.as_str(), "1" | "true" | "yes" | "on");
    eprintln!("SQASSIT Rust migration preview: incomplete; do not use in production. Listening on {address}");
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, sqassit::web::router(static_path, allow_dev))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

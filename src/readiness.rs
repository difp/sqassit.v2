//! Readiness probes with a short-lived Telegram Bot API cache.
use crate::telegram::TelegramClient;
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How long a Telegram readiness result may be reused, matching the reference app.
pub const TELEGRAM_TTL: Duration = Duration::from_secs(60);

/// Object-safe probe so tests can replace the Telegram transport.
pub trait TelegramHealth: Send + Sync {
    fn get_me(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

impl TelegramHealth for TelegramClient {
    fn get_me(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            use crate::telegram::BotTransport;
            self.request("getMe", json!({})).await.is_ok()
        })
    }
}

/// Caches the Telegram result for [`TELEGRAM_TTL`] so probes do not call Telegram on every request.
#[derive(Clone, Default)]
pub struct Readiness {
    telegram: Option<Arc<dyn TelegramHealth>>,
    cache: Arc<Mutex<Option<(Instant, bool)>>>,
}

impl Readiness {
    pub fn new(telegram: Arc<dyn TelegramHealth>) -> Self {
        Self {
            telegram: Some(telegram),
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Used when no bot token is configured: Telegram can never be ready.
    pub fn unconfigured() -> Self {
        Self::default()
    }

    pub async fn telegram_ready(&self) -> bool {
        self.telegram_ready_at(Instant::now()).await
    }

    async fn telegram_ready_at(&self, now: Instant) -> bool {
        let Some(telegram) = &self.telegram else {
            return false;
        };
        if let Ok(cache) = self.cache.lock() {
            if let Some((checked, ready)) = *cache {
                if now.duration_since(checked) < TELEGRAM_TTL {
                    return ready;
                }
            }
        }
        // Probe without holding the cache lock over network I/O.
        let ready = telegram.get_me().await;
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Some((now, ready));
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Fake {
        calls: AtomicUsize,
        ready: AtomicBool,
    }
    impl TelegramHealth for Fake {
        fn get_me(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let ready = self.ready.load(Ordering::SeqCst);
            Box::pin(async move { ready })
        }
    }

    fn probe(ready: bool) -> Arc<Fake> {
        Arc::new(Fake {
            calls: AtomicUsize::new(0),
            ready: AtomicBool::new(ready),
        })
    }

    #[tokio::test]
    async fn unconfigured_telegram_is_never_ready() {
        assert!(!Readiness::unconfigured().telegram_ready().await);
    }

    #[tokio::test]
    async fn result_is_cached_for_the_ttl_and_then_refreshed() {
        let fake = probe(true);
        let readiness = Readiness::new(fake.clone());
        let start = Instant::now();
        assert!(readiness.telegram_ready_at(start).await);
        assert!(
            readiness
                .telegram_ready_at(start + Duration::from_secs(59))
                .await
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        fake.ready.store(false, Ordering::SeqCst);
        assert!(
            !readiness
                .telegram_ready_at(start + Duration::from_secs(60))
                .await
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
        assert!(
            !readiness
                .telegram_ready_at(start + Duration::from_secs(120))
                .await
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    }
}

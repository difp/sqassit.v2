//! Opt-in Telegram update delivery. This remains a preview until parity is complete.
use crate::{
    bot::{self, BotError},
    profile::Profiles,
    telegram::{BotTransport, TelegramClient},
};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BotMode {
    Polling,
    Webhook,
}
#[derive(Clone)]
pub struct BotRuntime {
    pub profiles: Profiles,
    pub client: TelegramClient,
    pub mode: BotMode,
    pub secret: String,
    pub base_url: String,
    pub notifier: crate::event_notify::EventNotifier,
}

pub fn validate_webhook(base: &str, secret: &str) -> bool {
    let normalized = secret.trim().to_ascii_lowercase();
    !base.trim().is_empty()
        && secret.trim().len() >= 32
        && ![
            "change-me-to-random-string",
            "changeme",
            "change-me",
            "secret",
            "webhook-secret",
            "test",
        ]
        .contains(&normalized.as_str())
}
pub fn verify_secret(expected: &str, received: &str) -> bool {
    !expected.is_empty()
        && expected.len() == received.len()
        && bool::from(expected.as_bytes().ct_eq(received.as_bytes()))
}
pub struct WebhookConfig<'a> {
    pub mode: BotMode,
    pub secret: &'a str,
    pub base_url: &'a str,
    pub notifier: &'a crate::event_notify::EventNotifier,
}
/// Returns HTTP status and JSON. Never accepts an update in polling mode or with an invalid secret.
pub async fn webhook<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    config: WebhookConfig<'_>,
    received: &str,
    update: &Value,
    now: i64,
) -> (u16, Value) {
    if config.mode != BotMode::Webhook {
        return (404, json!({"detail":"Not Found"}));
    }
    if !verify_secret(config.secret, received) {
        return (403, json!({"ok":false,"error":"Forbidden"}));
    }
    match bot::handle_update(profiles, bot, config.base_url, config.notifier, update, now).await {
        Ok(()) | Err(BotError::Unsupported) => (200, json!({"ok":true})),
        Err(BotError::Database | BotError::Telegram) => (503, json!({"ok":false})),
        Err(BotError::Malformed) => (400, json!({"ok":false})),
    }
}
/// Processes an ordered batch and returns the next unacknowledged offset.
async fn process_batch<T: BotTransport>(
    profiles: &Profiles,
    bot: &T,
    base: &str,
    notifier: &crate::event_notify::EventNotifier,
    items: &[Value],
    mut offset: i64,
    now: i64,
) -> Result<i64, i64> {
    for update in items {
        let Some(id) = update.get("update_id").and_then(Value::as_i64) else {
            return Err(offset);
        };
        match bot::handle_update(profiles, bot, base, notifier, update, now).await {
            Ok(()) | Err(BotError::Unsupported) => {}
            Err(_) => return Err(offset),
        }
        offset = id.saturating_add(1);
    }
    Ok(offset)
}
/// Ignore unrelated updates; retain failed updates for retry.
pub async fn poll(runtime: BotRuntime) {
    let mut offset: i64 = 0;
    loop {
        let response=runtime.client.request("getUpdates",json!({"offset":offset,"timeout":25,"allowed_updates":["message","callback_query"]})).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let result = if let Ok(value) = response {
            if let Some(items) = value.get("result").and_then(Value::as_array) {
                process_batch(
                    &runtime.profiles,
                    &runtime.client,
                    &runtime.base_url,
                    &runtime.notifier,
                    items,
                    offset,
                    now,
                )
                .await
            } else {
                Err(offset)
            }
        } else {
            Err(offset)
        };
        match result {
            Ok(next) => offset = next,
            Err(next) => {
                offset = next;
                eprintln!("Telegram update processing paused; retrying in five seconds");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::TelegramError;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Fake(Mutex<usize>);
    impl BotTransport for Fake {
        async fn request(&self, _: &str, _: Value) -> Result<Value, TelegramError> {
            *self.0.lock().unwrap() += 1;
            Ok(json!({"ok":true,"result":{"message_id":1}}))
        }
    }
    fn service() -> Profiles {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "test".into(),
            owner: 1,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        }
    }
    #[tokio::test]
    async fn webhook_rejects_wrong_secret_and_wrong_mode() {
        let p = service();
        let bot = Fake::default();
        let secret = "a2345678901234567890123456789012";
        let update = json!({"message":{"from":{"id":42},"chat":{"id":42},"text":"/phone"}});
        assert!(!validate_webhook("", secret));
        assert!(!validate_webhook("https://example.test", "short"));
        assert!(validate_webhook("https://example.test", secret));
        assert_eq!(
            webhook(
                &p,
                &bot,
                WebhookConfig {
                    mode: BotMode::Polling,
                    secret,
                    base_url: "",
                    notifier: &crate::event_notify::EventNotifier::default()
                },
                secret,
                &update,
                1
            )
            .await
            .0,
            404
        );
        assert_eq!(
            webhook(
                &p,
                &bot,
                WebhookConfig {
                    mode: BotMode::Webhook,
                    secret,
                    base_url: "",
                    notifier: &crate::event_notify::EventNotifier::default()
                },
                "wrong",
                &update,
                1
            )
            .await
            .0,
            403
        );
        assert_eq!(*bot.0.lock().unwrap(), 0);
        assert_eq!(
            webhook(
                &p,
                &bot,
                WebhookConfig {
                    mode: BotMode::Webhook,
                    secret,
                    base_url: "",
                    notifier: &crate::event_notify::EventNotifier::default()
                },
                secret,
                &update,
                1
            )
            .await
            .0,
            200
        );
        assert_eq!(*bot.0.lock().unwrap(), 1);
        let unknown = json!({"callback_query":{"id":"x","from":{"id":42},"data":"future:action"}});
        assert_eq!(
            webhook(
                &p,
                &bot,
                WebhookConfig {
                    mode: BotMode::Webhook,
                    secret,
                    base_url: "",
                    notifier: &crate::event_notify::EventNotifier::default()
                },
                secret,
                &unknown,
                1
            )
            .await
            .0,
            200
        );
    }
    #[tokio::test]
    async fn polling_skips_unrelated_updates() {
        let p = service();
        let bot = Fake::default();
        let messages = vec![
            json!({"update_id":10,"message":{"from":{"id":42},"chat":{"id":42},"text":"/phone"}}),
            json!({"update_id":11,"callback_query":{"id":"x","from":{"id":42},"data":"future:action"}}),
            json!({"update_id":12,"message":{"from":{"id":42},"chat":{"id":42},"text":"/phone"}}),
        ];
        assert_eq!(
            process_batch(
                &p,
                &bot,
                "",
                &crate::event_notify::EventNotifier::default(),
                &messages,
                0,
                1
            )
            .await,
            Ok(13)
        );
        assert_eq!(*bot.0.lock().unwrap(), 2);
    }
    #[tokio::test]
    async fn polling_keeps_transport_failure_unacknowledged() {
        struct Failing(Mutex<usize>);
        impl BotTransport for Failing {
            async fn request(&self, _: &str, _: Value) -> Result<Value, TelegramError> {
                let mut count = self.0.lock().unwrap();
                *count += 1;
                if *count == 2 {
                    Err(TelegramError::Transport)
                } else {
                    Ok(json!({"ok":true}))
                }
            }
        }
        let p = service();
        let bot = Failing(Mutex::new(0));
        let messages = (10..13).map(|id| json!({"update_id":id,"message":{"from":{"id":42},"chat":{"id":42},"text":"/phone"}})).collect::<Vec<_>>();
        assert_eq!(
            process_batch(
                &p,
                &bot,
                "",
                &crate::event_notify::EventNotifier::default(),
                &messages,
                0,
                1
            )
            .await,
            Err(11)
        );
        assert_eq!(*bot.0.lock().unwrap(), 2);
    }
}

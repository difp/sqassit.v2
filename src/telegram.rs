//! Minimal Telegram Bot API transport. Never log request URLs: they contain the bot token.
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug)]
pub enum TelegramError {
    Configuration,
    Transport,
    Api,
}

#[derive(Clone)]
pub struct TelegramClient {
    client: reqwest::Client,
    token: String,
}
impl TelegramClient {
    pub fn new(token: String, proxy: Option<&str>) -> Result<Self, TelegramError> {
        if token.trim().is_empty() {
            return Err(TelegramError::Configuration);
        }
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(30));
        if let Some(proxy) = proxy.filter(|p| !p.trim().is_empty()) {
            builder = builder
                .proxy(reqwest::Proxy::all(proxy).map_err(|_| TelegramError::Configuration)?);
        }
        Ok(Self {
            client: builder.build().map_err(|_| TelegramError::Configuration)?,
            token,
        })
    }
    pub async fn send_html(&self, chat_id: i64, text: &str) -> Result<i64, TelegramError> {
        let endpoint = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let response = self
            .client
            .post(endpoint)
            .json(&json!({"chat_id":chat_id,"text":text,"parse_mode":"HTML"}))
            .send()
            .await
            .map_err(|_| TelegramError::Transport)?;
        if !response.status().is_success() {
            return Err(TelegramError::Api);
        }
        let value: Value = response.json().await.map_err(|_| TelegramError::Api)?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(TelegramError::Api);
        }
        value
            .pointer("/result/message_id")
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .ok_or(TelegramError::Api)
    }
}

pub trait ReminderSender: Send + Sync {
    fn send(
        &self,
        chat_id: i64,
        text: &str,
    ) -> impl std::future::Future<Output = Result<i64, TelegramError>> + Send;
}
impl ReminderSender for TelegramClient {
    async fn send(&self, chat_id: i64, text: &str) -> Result<i64, TelegramError> {
        self.send_html(chat_id, text).await
    }
}

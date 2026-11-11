//! Minimal Telegram Bot API transport. Never log request URLs: they contain the bot token.
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug)]
pub enum TelegramError {
    Configuration,
    Transport,
    Api,
    NotModified,
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
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = proxy.filter(|p| !p.trim().is_empty()) {
            builder = builder
                .proxy(reqwest::Proxy::all(proxy).map_err(|_| TelegramError::Configuration)?);
        }
        Ok(Self {
            client: builder.build().map_err(|_| TelegramError::Configuration)?,
            token,
        })
    }
    async fn call(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
        let endpoint = format!("https://api.telegram.org/bot{}/{method}", self.token);
        let response = self
            .client
            .post(endpoint)
            .json(&payload)
            .send()
            .await
            .map_err(|_| TelegramError::Transport)?;
        let status = response.status();
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(TelegramError::Transport);
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| TelegramError::Transport)?;
        if !status.is_success() || value.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = value
                .get("error_code")
                .and_then(Value::as_i64)
                .unwrap_or(i64::from(status.as_u16()));
            if code == 429 || code >= 500 {
                return Err(TelegramError::Transport);
            }
            if value
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|s| s.to_lowercase().contains("message is not modified"))
            {
                return Err(TelegramError::NotModified);
            }
            return Err(TelegramError::Api);
        }
        Ok(value)
    }
    pub async fn send_html(&self, chat_id: i64, text: &str) -> Result<i64, TelegramError> {
        let value = self
            .call(
                "sendMessage",
                json!({"chat_id":chat_id,"text":text,"parse_mode":"HTML"}),
            )
            .await?;
        value
            .pointer("/result/message_id")
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .ok_or(TelegramError::Transport)
    }
    pub async fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<(), TelegramError> {
        let value = self
            .call(
                "deleteMessage",
                json!({"chat_id":chat_id,"message_id":message_id}),
            )
            .await?;
        if value.get("result").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(TelegramError::Transport)
        }
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

pub trait ReminderDeleter: Send + Sync {
    fn delete(
        &self,
        chat_id: i64,
        message_id: i64,
    ) -> impl std::future::Future<Output = Result<(), TelegramError>> + Send;
}
impl ReminderDeleter for TelegramClient {
    async fn delete(&self, chat_id: i64, message_id: i64) -> Result<(), TelegramError> {
        self.delete_message(chat_id, message_id).await
    }
}

/// Narrow Bot API interface, injectable for tests without contacting Telegram.
pub trait BotTransport: Send + Sync {
    fn request(
        &self,
        method: &str,
        payload: Value,
    ) -> impl std::future::Future<Output = Result<Value, TelegramError>> + Send;
}
impl BotTransport for TelegramClient {
    async fn request(&self, method: &str, payload: Value) -> Result<Value, TelegramError> {
        self.call(method, payload).await
    }
}

impl crate::avatar::AvatarSource for TelegramClient {
    async fn profile_photo(&self, user: i64) -> Result<Option<String>, crate::avatar::AvatarError> {
        let value = self
            .call("getUserProfilePhotos", json!({"user_id":user,"limit":1}))
            .await
            .map_err(|_| crate::avatar::AvatarError::Unavailable)?;
        Ok(value
            .pointer("/result/photos/0")
            .and_then(Value::as_array)
            .and_then(|a| a.last())
            .and_then(|p| p.get("file_id"))
            .and_then(Value::as_str)
            .map(str::to_owned))
    }
    async fn download(&self, file: &str) -> Result<Option<Vec<u8>>, crate::avatar::AvatarError> {
        let result = self
            .call("getFile", json!({"file_id":file}))
            .await
            .map_err(|_| crate::avatar::AvatarError::Unavailable)?;
        let Some(path) = result.pointer("/result/file_path").and_then(Value::as_str) else {
            return Ok(None);
        };
        // Telegram paths are relative; never permit a response to redirect the bot token to another origin.
        if path.is_empty()
            || path.starts_with('/')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.'))
        {
            return Err(crate::avatar::AvatarError::Unavailable);
        }
        let url = format!("https://api.telegram.org/file/bot{}/{path}", self.token);
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| crate::avatar::AvatarError::Unavailable)?;
        if !response.status().is_success() {
            return Err(crate::avatar::AvatarError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|n| n > crate::avatar::MAX_BYTES as u64)
        {
            return Err(crate::avatar::AvatarError::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| crate::avatar::AvatarError::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > crate::avatar::MAX_BYTES {
                return Err(crate::avatar::AvatarError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            Ok(None)
        } else {
            Ok(Some(bytes))
        }
    }
}

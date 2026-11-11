//! Privacy-aware avatar proxy with bounded in-memory caching.
use crate::{auth, profile::Profiles};
use rusqlite::OptionalExtension;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

pub const MAX_BYTES: usize = 10 * 1024 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub enum AvatarError {
    Unauthorized,
    Hidden,
    NotFound,
    Unavailable,
    TooLarge,
    Database,
}
pub trait AvatarSource: Send + Sync {
    fn profile_photo(
        &self,
        user: i64,
    ) -> impl std::future::Future<Output = Result<Option<String>, AvatarError>> + Send;
    fn download(
        &self,
        file: &str,
    ) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, AvatarError>> + Send;
}
#[derive(Default)]
struct Cache {
    entries: HashMap<(i64, String), Arc<Vec<u8>>>,
    order: VecDeque<(i64, String)>,
    bytes: usize,
    missing: HashMap<(i64, String), Instant>,
}
#[derive(Clone, Default)]
pub struct AvatarCache {
    inner: Arc<tokio::sync::Mutex<Cache>>,
}
pub struct Avatar {
    pub content: Arc<Vec<u8>>,
    pub version: String,
    pub immutable: bool,
}
fn row(p: &Profiles, id: i64, real: bool) -> Result<Option<String>, AvatarError> {
    let db = p.db.lock().map_err(|_| AvatarError::Database)?;
    let user: Option<(Option<String>, String)> = db
        .query_row(
            "SELECT avatar_file_id,display_mode FROM users WHERE telegram_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| AvatarError::Database)?;
    let (file, mode) = user.ok_or(AvatarError::NotFound)?;
    if !real && mode == "animal" {
        return Err(AvatarError::Hidden);
    }
    Ok(file.filter(|f| !f.is_empty()))
}
pub fn media_type(content: &[u8]) -> &'static str {
    if content.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if content.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if content.starts_with(b"GIF87a") || content.starts_with(b"GIF89a") {
        "image/gif"
    } else if content.starts_with(b"RIFF") && content.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}
impl AvatarCache {
    pub async fn get<S: AvatarSource>(
        &self,
        p: &Profiles,
        source: &S,
        id: i64,
        token: &str,
        requested: Option<&str>,
        now: i64,
    ) -> Result<Avatar, AvatarError> {
        let real =
            auth::verify_scoped_token(&p.bot_token, token, "avatar_real", id, now) == Some(id);
        if !real && auth::verify_scoped_token(&p.bot_token, token, "avatar", id, now) != Some(id) {
            return Err(AvatarError::Unauthorized);
        }
        // A single bounded lock deduplicates concurrent requests. It does not hold the DB mutex over network I/O.
        let mut cache = self.inner.lock().await;
        for _ in 0..2 {
            let mut file = row(p, id, real)?;
            let mut version = p.avatar_version(id, file.as_deref());
            let mut key = (id, version.clone());
            if cache
                .missing
                .get(&key)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(86400))
            {
                return Err(AvatarError::NotFound);
            }
            if file.is_none() {
                let fetched = source.profile_photo(id).await?;
                if let Some(fetched) = fetched {
                    let db = p.db.lock().map_err(|_| AvatarError::Database)?;
                    db.execute("UPDATE users SET avatar_file_id=?1 WHERE telegram_id=?2 AND (avatar_file_id IS NULL OR avatar_file_id='')",rusqlite::params![fetched,id]).map_err(|_|AvatarError::Database)?;
                }
                file = row(p, id, real)?;
                version = p.avatar_version(id, file.as_deref());
                key = (id, version.clone());
                if file.is_none() {
                    if cache.missing.len() >= 1024 {
                        cache.missing.clear();
                    }
                    cache.missing.insert(key, Instant::now());
                    return Err(AvatarError::NotFound);
                }
            }
            if let Some(content) = cache.entries.get(&key).cloned() {
                cache.order.retain(|k| k != &key);
                cache.order.push_back(key);
                return Ok(Avatar {
                    content,
                    immutable: requested == Some(version.as_str()),
                    version,
                });
            }
            let content = source
                .download(file.as_deref().expect("file present"))
                .await?;
            // Recheck the live privacy setting and version after awaiting Telegram.
            let current = row(p, id, real)?;
            if current != file {
                continue;
            }
            let Some(content) = content else {
                if cache.missing.len() >= 1024 {
                    cache.missing.clear();
                }
                cache.missing.insert(key, Instant::now());
                return Err(AvatarError::NotFound);
            };
            if content.len() > MAX_BYTES {
                return Err(AvatarError::TooLarge);
            }
            let content = Arc::new(content);
            cache.bytes += content.len();
            cache.entries.insert(key.clone(), content.clone());
            cache.order.push_back(key);
            while cache.entries.len() > 128 || cache.bytes > 32 * 1024 * 1024 {
                if let Some(old) = cache.order.pop_front() {
                    if let Some(data) = cache.entries.remove(&old) {
                        cache.bytes = cache.bytes.saturating_sub(data.len());
                    }
                } else {
                    break;
                }
            }
            return Ok(Avatar {
                content,
                immutable: requested == Some(version.as_str()),
                version,
            });
        }
        Err(AvatarError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fake {
        calls: AtomicUsize,
        hide: bool,
        db: Arc<std::sync::Mutex<rusqlite::Connection>>,
    }
    impl AvatarSource for Fake {
        async fn profile_photo(&self, _: i64) -> Result<Option<String>, AvatarError> {
            Ok(None)
        }
        async fn download(&self, _: &str) -> Result<Option<Vec<u8>>, AvatarError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hide {
                self.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE users SET display_mode='animal' WHERE telegram_id=42",
                        [],
                    )
                    .unwrap();
            }
            Ok(Some(b"\x89PNG\r\n\x1a\nimage".to_vec()))
        }
    }
    fn service() -> Profiles {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO users(telegram_id,avatar_file_id,display_mode,created_at) VALUES(42,'file-a','name',0)",[]).unwrap();
        Profiles {
            db: Arc::new(std::sync::Mutex::new(db)),
            bot_token: "secret".into(),
            owner: 1,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(std::sync::Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                Duration::from_secs(60),
            ))),
        }
    }
    #[tokio::test]
    async fn tokens_cache_and_live_privacy_are_enforced() {
        let p = service();
        let source = Fake {
            calls: AtomicUsize::new(0),
            hide: false,
            db: p.db.clone(),
        };
        let cache = AvatarCache::default();
        assert!(matches!(
            cache.get(&p, &source, 42, "bad", None, 1000).await,
            Err(AvatarError::Unauthorized)
        ));
        let token = auth::issue_scoped_token("secret", "avatar", 42, 42, 1000, 86400);
        let version = p.avatar_version(42, Some("file-a"));
        let first = cache
            .get(&p, &source, 42, &token, Some(&version), 1000)
            .await
            .unwrap();
        assert!(first.immutable);
        assert_eq!(media_type(&first.content), "image/png");
        let stale = cache
            .get(&p, &source, 42, &token, Some("old"), 1000)
            .await
            .unwrap();
        assert!(!stale.immutable);
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        p.db.lock()
            .unwrap()
            .execute(
                "UPDATE users SET display_mode='animal' WHERE telegram_id=42",
                [],
            )
            .unwrap();
        assert!(matches!(
            cache
                .get(&p, &source, 42, &token, Some(&version), 1000)
                .await,
            Err(AvatarError::Hidden)
        ));
        let real = auth::issue_scoped_token("secret", "avatar_real", 42, 42, 1000, 86400);
        assert!(cache
            .get(&p, &source, 42, &real, Some(&version), 1000)
            .await
            .is_ok());
    }
    #[tokio::test]
    async fn privacy_change_during_download_prevents_disclosure() {
        let p = service();
        let source = Fake {
            calls: AtomicUsize::new(0),
            hide: true,
            db: p.db.clone(),
        };
        let token = auth::issue_scoped_token("secret", "avatar", 42, 42, 1000, 86400);
        assert!(matches!(
            AvatarCache::default()
                .get(&p, &source, 42, &token, None, 1000)
                .await,
            Err(AvatarError::Hidden)
        ));
    }
}

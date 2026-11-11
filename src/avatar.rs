//! Privacy-aware avatar proxy with bounded in-memory and optional disk caching.
use crate::{auth, profile::Profiles};
use rusqlite::OptionalExtension;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};

pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// Matches the reference disk budget and its 80% cleanup target.
pub const MAX_DISK_BYTES: usize = 200 * 1024 * 1024;
const CLEANUP_TARGET_PERCENT: usize = 80;
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
struct Disk {
    root: PathBuf,
    bytes: AtomicUsize,
    next_tmp: AtomicU64,
}
#[derive(Clone, Default)]
pub struct AvatarCache {
    inner: Arc<tokio::sync::Mutex<Cache>>,
    disk: Option<Arc<Disk>>,
}
pub struct Avatar {
    pub content: Arc<Vec<u8>>,
    pub version: String,
    pub immutable: bool,
}

fn cache_files(root: &Path) -> Vec<(SystemTime, usize, PathBuf)> {
    fn visit(directory: &Path, out: &mut Vec<(SystemTime, usize, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "img") {
                if let Ok(metadata) = entry.metadata() {
                    out.push((
                        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        metadata.len() as usize,
                        path,
                    ));
                }
            }
        }
    }
    let mut files = Vec::new();
    visit(root, &mut files);
    files
}

fn prune_empty_dirs(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

fn disk_usage(root: &Path) -> usize {
    cache_files(root).iter().map(|(_, size, _)| size).sum()
}

/// Remove the oldest files until the cache is at or below the cleanup target.
fn cleanup_disk(root: &Path, max_bytes: usize) -> usize {
    let mut files = cache_files(root);
    let mut total: usize = files.iter().map(|(_, size, _)| size).sum();
    let target = max_bytes.saturating_mul(CLEANUP_TARGET_PERCENT) / 100;
    if total > target {
        files.sort_by_key(|(modified, _, _)| *modified);
        for (_, size, path) in files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
    prune_empty_dirs(root);
    total
}

fn remove_user_dir(root: &Path, id: i64) -> usize {
    let directory = root.join(id.to_string());
    let removed = disk_usage(&directory);
    let _ = std::fs::remove_dir_all(&directory);
    removed
}

impl AvatarCache {
    /// Disk-backed cache. Scans existing files at startup and trims them if needed.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut bytes = disk_usage(&root);
        if bytes > MAX_DISK_BYTES {
            bytes = cleanup_disk(&root, MAX_DISK_BYTES);
        }
        Self {
            inner: Arc::default(),
            disk: Some(Arc::new(Disk {
                root,
                bytes: AtomicUsize::new(bytes),
                next_tmp: AtomicU64::new(0),
            })),
        }
    }

    async fn disk_read(&self, id: i64, version: &str) -> Option<Arc<Vec<u8>>> {
        let disk = self.disk.as_ref()?;
        let path = disk
            .root
            .join(id.to_string())
            .join(format!("{version}.img"));
        let content = tokio::fs::read(path).await.ok()?;
        if content.len() > MAX_BYTES {
            return None;
        }
        Some(Arc::new(content))
    }

    async fn disk_write(&self, id: i64, version: &str, content: &[u8]) {
        let Some(disk) = self.disk.as_ref() else {
            return;
        };
        let directory = disk.root.join(id.to_string());
        if tokio::fs::create_dir_all(&directory).await.is_err() {
            return;
        }
        let target = directory.join(format!("{version}.img"));
        let tmp = directory.join(format!(
            ".{version}.{}.{}.tmp",
            std::process::id(),
            disk.next_tmp.fetch_add(1, Ordering::Relaxed)
        ));
        if tokio::fs::write(&tmp, content).await.is_err() {
            return;
        }
        if tokio::fs::rename(&tmp, &target).await.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
            return;
        }
        disk.bytes.fetch_add(content.len(), Ordering::Relaxed);
        if disk.bytes.load(Ordering::Relaxed) > MAX_DISK_BYTES {
            self.trim_disk().await;
        }
    }

    async fn trim_disk(&self) {
        let Some(disk) = self.disk.as_ref().cloned() else {
            return;
        };
        let root = disk.root.clone();
        if let Ok(total) =
            tokio::task::spawn_blocking(move || cleanup_disk(&root, MAX_DISK_BYTES)).await
        {
            disk.bytes.store(total, Ordering::Relaxed);
        }
    }
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
    /// Remove all cached bytes and negative entries belonging to a deleted account.
    pub async fn remove_user(&self, id: i64) {
        {
            let mut cache = self.inner.lock().await;
            let removed: usize = cache
                .entries
                .iter()
                .filter(|((user, _), _)| *user == id)
                .map(|(_, value)| value.len())
                .sum();
            cache.entries.retain(|(user, _), _| *user != id);
            cache.order.retain(|(user, _)| *user != id);
            cache.missing.retain(|(user, _), _| *user != id);
            cache.bytes = cache.bytes.saturating_sub(removed);
        }
        if let Some(disk) = self.disk.as_ref().cloned() {
            let root = disk.root.clone();
            if let Ok(removed) =
                tokio::task::spawn_blocking(move || remove_user_dir(&root, id)).await
            {
                let current = disk.bytes.load(Ordering::Relaxed);
                disk.bytes
                    .store(current.saturating_sub(removed), Ordering::Relaxed);
            }
        }
    }
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
                // Avatar lookup is best-effort: the reference logs Telegram failures
                // and falls through to the negative cache instead of returning 502.
                let fetched = source.profile_photo(id).await.unwrap_or(None);
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
            if let Some(content) = self.disk_read(id, &version).await {
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
            self.disk_write(id, &version, &content).await;
            cache.missing.remove(&key);
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
        cache.remove_user(42).await;
        assert!(cache
            .get(&p, &source, 42, &token, Some(&version), 1000)
            .await
            .is_ok());
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
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
    #[tokio::test]
    async fn disk_cache_survives_instances_and_is_deleted_with_the_user() {
        let root = std::env::temp_dir().join(format!(
            "sqassit-avatar-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = service();
        let token = auth::issue_scoped_token("secret", "avatar", 42, 42, 1000, 86400);
        let version = p.avatar_version(42, Some("file-a"));
        let source = Fake {
            calls: AtomicUsize::new(0),
            hide: false,
            db: p.db.clone(),
        };
        let cache = AvatarCache::new(&root);
        let first = cache
            .get(&p, &source, 42, &token, Some(&version), 1000)
            .await
            .unwrap();
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        assert!(root.join("42").join(format!("{version}.img")).exists());

        let cold_source = Fake {
            calls: AtomicUsize::new(0),
            hide: false,
            db: p.db.clone(),
        };
        let second = AvatarCache::new(&root)
            .get(&p, &cold_source, 42, &token, Some(&version), 1000)
            .await
            .unwrap();
        assert_eq!(cold_source.calls.load(Ordering::SeqCst), 0);
        assert_eq!(first.content, second.content);

        cache.remove_user(42).await;
        assert!(!root.join("42").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
    #[tokio::test]
    async fn profile_photo_failure_falls_back_to_the_negative_cache() {
        struct Failing;
        impl AvatarSource for Failing {
            async fn profile_photo(&self, _: i64) -> Result<Option<String>, AvatarError> {
                Err(AvatarError::Unavailable)
            }
            async fn download(&self, _: &str) -> Result<Option<Vec<u8>>, AvatarError> {
                unreachable!("no file id was stored")
            }
        }
        let p = service();
        p.db.lock()
            .unwrap()
            .execute(
                "UPDATE users SET avatar_file_id=NULL WHERE telegram_id=42",
                [],
            )
            .unwrap();
        let token = auth::issue_scoped_token("secret", "avatar_real", 42, 42, 1000, 86400);
        assert!(matches!(
            AvatarCache::default()
                .get(&p, &Failing, 42, &token, None, 1000)
                .await,
            Err(AvatarError::NotFound)
        ));
    }
    #[test]
    fn disk_cleanup_trims_oldest_files_to_the_target() {
        let root = std::env::temp_dir().join(format!(
            "sqassit-avatar-cleanup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for id in 1..=3 {
            let directory = root.join(id.to_string());
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("v.img"), vec![7u8; 10]).unwrap();
        }
        assert_eq!(disk_usage(&root), 30);
        assert!(cleanup_disk(&root, 20) <= 16);
        assert!(disk_usage(&root) <= 16);
        let _ = std::fs::remove_dir_all(&root);
    }
}

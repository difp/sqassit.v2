//! Authenticated profile persistence and response serialization.
use crate::{auth, identity};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Profiles {
    pub db: Arc<Mutex<Connection>>,
    pub bot_token: String,
    pub owner: i64,
    pub timezone: chrono_tz::Tz,
    pub require_event_approval: bool,
    pub phone_required_from: Option<i64>,
    pub booking_limiter: Arc<Mutex<crate::rate_limit::RateLimiter>>,
}

#[derive(Debug)]
pub enum ProfileError {
    Unauthorized(&'static str),
    Forbidden(&'static str),
    Storage,
    Invalid(&'static str),
    Rule(u16, &'static str),
    RateLimited(u64),
    Duplicate(i64),
}

#[derive(Default, Deserialize)]
pub struct NotificationSettings {
    pub enabled: Option<bool>,
    pub notify_matches: Option<bool>,
    pub notify_trainings: Option<bool>,
    pub notify_messages: Option<bool>,
}

#[derive(Deserialize)]
pub struct PlayerProfile {
    #[serde(default)]
    pub about: String,
    #[serde(default = "default_mode")]
    pub display_mode: String,
    #[serde(default)]
    pub show_username: bool,
    pub phone: Option<String>,
    pub notifications_enabled: Option<bool>,
    pub notify_matches: Option<bool>,
    pub notify_trainings: Option<bool>,
    pub notify_messages: Option<bool>,
}
fn default_mode() -> String {
    "name".into()
}
pub(crate) fn normalize_phone(phone: &str) -> Result<String, ProfileError> {
    use unicode_general_category::{get_general_category, GeneralCategory};
    if phone.trim().is_empty() {
        return Ok(String::new());
    }
    let cleaned: String = phone
        .trim()
        .chars()
        .filter(|c| *c == '+' || get_general_category(*c) == GeneralCategory::DecimalNumber)
        .collect();
    let digits = cleaned.strip_prefix('+').unwrap_or(&cleaned);
    if !(10..=15).contains(&digits.chars().count())
        || !digits
            .chars()
            .all(|c| get_general_category(c) == GeneralCategory::DecimalNumber)
    {
        return Err(ProfileError::Invalid("Введите телефон из 10–15 цифр."));
    }
    Ok(cleaned)
}

impl Profiles {
    pub(crate) fn user_id(&self, init: &str, now: i64) -> Result<(i64, Value), ProfileError> {
        let data = auth::validate_init_data(init, &self.bot_token, now, 86_400).map_err(|e| {
            ProfileError::Unauthorized(match e {
                auth::AuthError::EmptyInitData => "Пустой initData.",
                auth::AuthError::EmptySecret => "Токен бота не задан.",
                auth::AuthError::MissingHash => "В initData отсутствует поле hash.",
                auth::AuthError::InvalidSignature => "Неверная подпись initData.",
                auth::AuthError::InvalidAuthDate => "Некорректный auth_date.",
                auth::AuthError::Expired => "initData просрочен.",
                auth::AuthError::MissingUser => "В initData отсутствует пользователь.",
                auth::AuthError::InvalidUser => "Некорректные данные пользователя.",
            })
        })?;
        let id = data
            .user
            .get("id")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .ok_or(ProfileError::Unauthorized("Некорректный telegram_id."))?;
        Ok((id, data.user))
    }
    pub(crate) fn register(
        &self,
        db: &Connection,
        id: i64,
        user: &Value,
        now: i64,
    ) -> Result<(), ProfileError> {
        let coach: bool = id == self.owner
            || db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(|_| ProfileError::Storage)?;
        let username = user.get("username").and_then(Value::as_str);
        let first_name = user.get("first_name").and_then(Value::as_str);
        db.execute("INSERT INTO users (telegram_id, username, first_name, is_coach, created_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(telegram_id) DO UPDATE SET username=excluded.username, first_name=excluded.first_name, is_coach=excluded.is_coach WHERE username IS NOT excluded.username OR first_name IS NOT excluded.first_name OR is_coach IS NOT excluded.is_coach", params![id, username, first_name, coach, now]).map_err(|_| ProfileError::Storage)?;
        Ok(())
    }
    pub(crate) fn avatar_url(&self, id: i64, file: Option<&str>, real: bool, now: i64) -> String {
        let payload = format!(
            "avatar-version:{id}:{}",
            file.filter(|s| !s.is_empty()).unwrap_or("none")
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(self.bot_token.as_bytes()).expect("HMAC key");
        mac.update(payload.as_bytes());
        let version = hex::encode(mac.finalize().into_bytes());
        let kind = if real { "avatar_real" } else { "avatar" };
        let token = auth::issue_scoped_token(&self.bot_token, kind, id, id, now, 604_800);
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("token", &token)
            .finish();
        format!("/api/avatar/{id}/{}?{query}", &version[..12])
    }
    pub fn me(&self, init: &str, now: i64) -> Result<Value, ProfileError> {
        let (id, user) = self.user_id(init, now)?;
        let db = self.db.lock().map_err(|_| ProfileError::Storage)?;
        self.register(&db, id, &user, now)?;
        let owner = id == self.owner;
        let manager = owner
            || db
                .query_row(
                    "SELECT is_manager FROM coaches WHERE telegram_id=?1",
                    [id],
                    |r| r.get::<_, bool>(0),
                )
                .optional()
                .map_err(|_| ProfileError::Storage)?
                .unwrap_or(false);
        let (emoji, name) = identity::animal_identity(id);
        db.query_row("SELECT first_name,is_coach,notifications_enabled,phone,notify_matches,notify_trainings,notify_messages,avatar_file_id,about,display_mode,show_username,onboarding_seen FROM users WHERE telegram_id=?1", [id], |r| {
            let file: Option<String> = r.get(7)?;
            Ok(json!({"ok":true,"user":{
                "telegram_id":id,"first_name":r.get::<_,Option<String>>(0)?,"is_coach":r.get::<_,bool>(1)?,"is_owner":owner,"is_manager":manager,
                "notifications_enabled":r.get::<_,bool>(2)?,"phone":r.get::<_,String>(3)?,"notify_matches":r.get::<_,bool>(4)?,"notify_trainings":r.get::<_,bool>(5)?,"notify_messages":r.get::<_,bool>(6)?,
                "avatar_url":self.avatar_url(id, file.as_deref(), true, now),"about":r.get::<_,String>(8)?,"display_mode":r.get::<_,String>(9)?,"show_username":r.get::<_,bool>(10)?,"onboarding_seen":r.get::<_,bool>(11)?,"animal":{"emoji":emoji,"name":name}
            }}))
        }).map_err(|_| ProfileError::Storage)
    }
    pub fn onboarding(&self, init: &str, now: i64) -> Result<Value, ProfileError> {
        let (id, user) = self.user_id(init, now)?;
        let db = self.db.lock().map_err(|_| ProfileError::Storage)?;
        self.register(&db, id, &user, now)?;
        db.execute(
            "UPDATE users SET onboarding_seen=1 WHERE telegram_id=?1",
            [id],
        )
        .map_err(|_| ProfileError::Storage)?;
        Ok(json!({"ok":true}))
    }
    pub fn notifications(
        &self,
        init: &str,
        now: i64,
        settings: NotificationSettings,
    ) -> Result<Value, ProfileError> {
        let (id, user) = self.user_id(init, now)?;
        let db = self.db.lock().map_err(|_| ProfileError::Storage)?;
        self.register(&db, id, &user, now)?;
        db.execute("UPDATE users SET notifications_enabled=COALESCE(?1,notifications_enabled),notify_matches=COALESCE(?2,notify_matches),notify_trainings=COALESCE(?3,notify_trainings),notify_messages=COALESCE(?4,notify_messages) WHERE telegram_id=?5",params![settings.enabled,settings.notify_matches,settings.notify_trainings,settings.notify_messages,id]).map_err(|_| ProfileError::Storage)?;
        db.query_row("SELECT notifications_enabled,notify_matches,notify_trainings,notify_messages FROM users WHERE telegram_id=?1",[id],|r| Ok(json!({"ok":true,"notifications_enabled":r.get::<_,bool>(0)?,"notify_matches":r.get::<_,bool>(1)?,"notify_trainings":r.get::<_,bool>(2)?,"notify_messages":r.get::<_,bool>(3)?}))).map_err(|_|ProfileError::Storage)
    }
    pub fn update_profile(
        &self,
        init: &str,
        now: i64,
        profile: PlayerProfile,
    ) -> Result<Value, ProfileError> {
        let (id, user) = self.user_id(init, now)?;
        if profile.about.chars().count() > 450
            || profile
                .phone
                .as_ref()
                .is_some_and(|p| p.chars().count() > 32)
            || !matches!(profile.display_mode.as_str(), "name" | "animal")
        {
            return Err(ProfileError::Invalid("Некорректные данные профиля."));
        }
        let phone = profile.phone.as_deref().map(normalize_phone).transpose()?;
        let db = self.db.lock().map_err(|_| ProfileError::Storage)?;
        self.register(&db, id, &user, now)?;
        db.execute("UPDATE users SET about=?1,display_mode=?2,show_username=?3,phone=COALESCE(?4,phone),notifications_enabled=COALESCE(?5,notifications_enabled),notify_matches=COALESCE(?6,notify_matches),notify_trainings=COALESCE(?7,notify_trainings),notify_messages=COALESCE(?8,notify_messages) WHERE telegram_id=?9",params![profile.about.trim(),profile.display_mode,profile.show_username,phone,profile.notifications_enabled,profile.notify_matches,profile.notify_trainings,profile.notify_messages,id]).map_err(|_|ProfileError::Storage)?;
        let (emoji, name) = identity::animal_identity(id);
        db.query_row("SELECT about,display_mode,show_username,phone,notifications_enabled,notify_matches,notify_trainings,notify_messages FROM users WHERE telegram_id=?1",[id],|r| Ok(json!({"ok":true,"profile":{"about":r.get::<_,String>(0)?,"display_mode":r.get::<_,String>(1)?,"show_username":r.get::<_,bool>(2)?,"phone":r.get::<_,String>(3)?,"notifications_enabled":r.get::<_,bool>(4)?,"notify_matches":r.get::<_,bool>(5)?,"notify_trainings":r.get::<_,bool>(6)?,"notify_messages":r.get::<_,bool>(7)?,"animal":{"emoji":emoji,"name":name}}}))).map_err(|_|ProfileError::Storage)
    }
}

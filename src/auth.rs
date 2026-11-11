//! Telegram Mini App authentication and resource-scoped tokens.
//! Wire formats match `legacy/app/secure_auth.py`.

use form_urlencoded;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::collections::BTreeMap;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    EmptyInitData,
    EmptySecret,
    MissingHash,
    InvalidSignature,
    InvalidAuthDate,
    Expired,
    MissingUser,
    InvalidUser,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InitData {
    pub user: Value,
    pub auth_date: i64,
    pub raw: BTreeMap<String, String>,
}

fn sign(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts arbitrary key lengths");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn verify_hex(key: &[u8], data: &[u8], signature: &str) -> bool {
    // Python compares the lowercase hex digest as a string, not case-insensitively.
    if signature.len() != 64
        || !signature
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return false;
    }
    let Ok(bytes) = hex::decode(signature) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts arbitrary key lengths");
    mac.update(data);
    mac.verify_slice(&bytes).is_ok()
}

/// Validate initData at a supplied Unix time, making expiry deterministic in tests.
pub fn validate_init_data(
    input: &str,
    bot_token: &str,
    now: i64,
    max_age_seconds: i64,
) -> Result<InitData, AuthError> {
    if input.is_empty() {
        return Err(AuthError::EmptyInitData);
    }
    if bot_token.is_empty() {
        return Err(AuthError::EmptySecret);
    }
    let mut pairs: BTreeMap<String, String> = form_urlencoded::parse(input.as_bytes())
        .into_owned()
        .collect();
    let signature = pairs
        .remove("hash")
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::MissingHash)?;
    let data = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let key = sign(b"WebAppData", bot_token.as_bytes());
    if !verify_hex(&key, data.as_bytes(), &signature) {
        return Err(AuthError::InvalidSignature);
    }
    let raw_date = pairs.get("auth_date").ok_or(AuthError::InvalidAuthDate)?;
    if raw_date.is_empty() || !raw_date.bytes().all(|c| c.is_ascii_digit()) {
        return Err(AuthError::InvalidAuthDate);
    }
    let auth_date: i64 = raw_date.parse().map_err(|_| AuthError::InvalidAuthDate)?;
    if now.saturating_sub(auth_date) > max_age_seconds {
        return Err(AuthError::Expired);
    }
    let raw_user = pairs
        .get("user")
        .filter(|v| !v.is_empty())
        .ok_or(AuthError::MissingUser)?;
    let user: Value = serde_json::from_str(raw_user).map_err(|_| AuthError::InvalidUser)?;
    if !user.as_object().is_some_and(|obj| obj.contains_key("id")) {
        return Err(AuthError::InvalidUser);
    }
    Ok(InitData {
        user,
        auth_date,
        raw: pairs,
    })
}

fn scoped_key(secret: &str) -> Vec<u8> {
    sign(secret.as_bytes(), b"sqassit-scoped-token-v1")
}

/// Produces the same stable, period-rounded token as the Python implementation.
pub fn issue_scoped_token(
    secret: &str,
    kind: &str,
    resource_id: i64,
    user_id: i64,
    now: i64,
    ttl_seconds: i64,
) -> String {
    let period = ttl_seconds.max(1);
    let expires = (now.div_euclid(period) + 2) * period;
    let payload = format!("{kind}:{resource_id}:{user_id}:{expires}");
    format!(
        "{payload}:{}",
        hex::encode(sign(&scoped_key(secret), payload.as_bytes()))
    )
}

pub fn verify_scoped_token(
    secret: &str,
    token: &str,
    kind: &str,
    resource_id: i64,
    now: i64,
) -> Option<i64> {
    if secret.is_empty() || token.is_empty() || token.chars().count() > 256 {
        return None;
    }
    let parts: Vec<_> = token.split(':').collect();
    if parts.len() != 5 || parts[0] != kind {
        return None;
    }
    let resource: i64 = parts[1].trim().parse().ok()?;
    let user: i64 = parts[2].trim().parse().ok()?;
    let expires: i64 = parts[3].trim().parse().ok()?;
    if resource != resource_id || expires < now {
        return None;
    }
    let payload = format!("{}:{resource}:{user}:{expires}", parts[0]);
    verify_hex(&scoped_key(secret), payload.as_bytes(), parts[4]).then_some(user)
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "123456:TEST-TOKEN";
    // Generated independently by legacy/app/secure_auth.py at Unix time 1000000.
    const INIT: &str = "auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    const SCOPED: &str =
        "avatar:555:42:1004400:d1b68c888b0ff0bc50b28f052b2105021cf0888fcc9aec803bf7ec3e4567effa";

    #[test]
    fn python_init_data_vector_and_expiry() {
        let data = validate_init_data(INIT, KEY, 1_000_000, 86_400).unwrap();
        assert_eq!(data.user["first_name"], "Иван");
        assert_eq!(data.user["id"], 42);
        assert_eq!(
            validate_init_data(INIT, KEY, 1_086_401, 86_400),
            Err(AuthError::Expired)
        );
        assert_eq!(
            validate_init_data(INIT, "wrong", 1_000_000, 86_400),
            Err(AuthError::InvalidSignature)
        );
        assert_eq!(
            validate_init_data(
                &INIT.replace("query_id=AA", "query_id=AB"),
                KEY,
                1_000_000,
                86_400
            ),
            Err(AuthError::InvalidSignature)
        );
    }
    #[test]
    fn python_scoped_vector_and_boundaries() {
        assert_eq!(
            issue_scoped_token(KEY, "avatar", 555, 42, 1_000_000, 3600),
            SCOPED
        );
        assert_eq!(
            verify_scoped_token(KEY, SCOPED, "avatar", 555, 1_004_400),
            Some(42)
        );
        assert_eq!(
            verify_scoped_token(KEY, SCOPED, "avatar", 555, 1_004_401),
            None
        );
        assert_eq!(
            verify_scoped_token(KEY, SCOPED, "avatar", 556, 1_000_000),
            None
        );
        assert_eq!(
            verify_scoped_token(KEY, SCOPED, "ics", 555, 1_000_000),
            None
        );
        assert_eq!(
            verify_scoped_token("wrong", SCOPED, "avatar", 555, 1_000_000),
            None
        );
    }
}

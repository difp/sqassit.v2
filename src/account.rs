//! User-owned data export. Account deletion is handled separately.
use crate::profile::{ProfileError, Profiles};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

pub fn export(p: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let (id, user) = p.user_id(init, now)?;
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    p.register(&db, id, &user, now)?;
    let profile=db.query_row("SELECT username,first_name,phone,notifications_enabled,notify_matches,notify_trainings,notify_messages,notes,about,display_mode,show_username,onboarding_seen FROM users WHERE telegram_id=?1",[id],|r|Ok(json!({"telegram_id":id,"username":r.get::<_,Option<String>>(0)?,"first_name":r.get::<_,Option<String>>(1)?,"phone":r.get::<_,String>(2)?,"notifications_enabled":r.get::<_,bool>(3)?,"notify_matches":r.get::<_,bool>(4)?,"notify_trainings":r.get::<_,bool>(5)?,"notify_messages":r.get::<_,bool>(6)?,"notes":r.get::<_,String>(7)?,"about":r.get::<_,String>(8)?,"display_mode":r.get::<_,String>(9)?,"show_username":r.get::<_,bool>(10)?,"onboarding_seen":r.get::<_,bool>(11)?}))).map_err(|_|ProfileError::Storage)?;
    let mut stmt=db.prepare("SELECT b.training_id,b.status,b.position,b.created_at,t.training_type,t.start_at,t.deleted_at FROM bookings b JOIN trainings t ON t.id=b.training_id WHERE b.user_id=?1 ORDER BY t.start_at DESC").map_err(|_|ProfileError::Storage)?;
    let rows=stmt.query_map([id],|r|Ok(json!({"training_id":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"position":r.get::<_,i64>(2)?,"created_at":r.get::<_,i64>(3)?,"training_type":r.get::<_,String>(4)?,"start_at":r.get::<_,i64>(5)?,"deleted_at":r.get::<_,Option<i64>>(6)?}))).map_err(|_|ProfileError::Storage)?;
    let bookings = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ProfileError::Storage)?;
    let mut stmt=db.prepare("SELECT id,to_user,text,created_at FROM messages WHERE from_user=?1 ORDER BY created_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows=stmt.query_map([id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"to_user":r.get::<_,i64>(1)?,"text":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?}))).map_err(|_|ProfileError::Storage)?;
    let messages = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ProfileError::Storage)?;
    let mut stmt=db.prepare("SELECT id,kind,text,page,created_at FROM feedback WHERE user_id=?1 ORDER BY created_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows=stmt.query_map([id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?,"page":r.get::<_,String>(3)?,"created_at":r.get::<_,i64>(4)?}))).map_err(|_|ProfileError::Storage)?;
    let feedback = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ProfileError::Storage)?;
    let application=db.query_row("SELECT telegram_id,message,status,created_at,decided_by,decided_at FROM coach_applications WHERE telegram_id=?1",[id],|r|Ok(json!({"telegram_id":r.get::<_,i64>(0)?,"message":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?,"decided_by":r.get::<_,Option<i64>>(4)?,"decided_at":r.get::<_,Option<i64>>(5)?}))).optional().map_err(|_|ProfileError::Storage)?;
    Ok(
        json!({"ok":true,"user":profile,"bookings":bookings,"messages":messages,"feedback":feedback,"application":application}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    const INIT:&str="auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    #[test]
    fn export_contains_only_own_records() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO users(telegram_id,phone,about,created_at) VALUES(42,'+12345678901','About',0),(99,'','',0); INSERT INTO trainings(id,title,start_at,created_at) VALUES(1,'one',200,0),(2,'two',100,0); INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(1,42,'booked',1),(2,99,'booked',1); INSERT INTO messages(from_user,to_user,text,created_at) VALUES(42,99,'own',1),(99,42,'private incoming',2); INSERT INTO feedback(user_id,kind,text,created_at) VALUES(42,'idea','own feedback',1),(99,'bug','other feedback',1); INSERT INTO coach_applications(telegram_id,status,message,created_at) VALUES(42,'rejected','mine',1),(99,'pending','other',1);").unwrap();
        let p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "123456:TEST-TOKEN".into(),
            owner: 99,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let result = export(&p, INIT, 1_000_000).unwrap();
        assert_eq!(result["user"]["phone"], "+12345678901");
        assert_eq!(result["bookings"].as_array().unwrap().len(), 1);
        assert_eq!(result["messages"].as_array().unwrap().len(), 1);
        assert_eq!(result["messages"][0]["text"], "own");
        assert_eq!(result["feedback"].as_array().unwrap().len(), 1);
        assert_eq!(result["application"]["message"], "mine");
        assert!(!result.to_string().contains("private incoming"));
    }
}

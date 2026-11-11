//! Read-only schedule for other coaches.
use crate::{
    auth, coach,
    profile::{ProfileError, Profiles},
};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub fn list(p: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    let viewer = coach::coach(p, &db, init, now)?;
    let mut stmt=db.prepare("SELECT t.id,t.title,t.training_type,t.start_at,t.duration_min,t.capacity,t.level,t.reminder_enabled,t.reminder_offset_min,t.created_by,(SELECT count(*) FROM bookings b WHERE b.training_id=t.id AND b.status='booked'),(SELECT count(*) FROM bookings b WHERE b.training_id=t.id AND b.status='waitlist'),(SELECT count(*) FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=t.id AND b.status='pending') FROM trainings t WHERE t.start_at>=?1 AND t.deleted_at IS NULL AND t.created_by!=?2 ORDER BY t.start_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map(params![now, viewer], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, i64>(11)?,
                r.get::<_, i64>(12)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut trainings = Vec::new();
    for row in rows {
        let (
            id,
            title,
            typ,
            start,
            duration,
            capacity,
            level,
            reminder,
            offset,
            owner,
            booked,
            waitlist,
            pending,
        ) = row.map_err(|_| ProfileError::Storage)?;
        let local = DateTime::<Utc>::from_timestamp(start, 0)
            .ok_or(ProfileError::Storage)?
            .with_timezone(&p.timezone);
        let trainer:Option<Value>=db.query_row("SELECT first_name,avatar_file_id,bio FROM users WHERE telegram_id=?1",[owner],|r|{let name:Option<String>=r.get(0)?;let file:Option<String>=r.get(1)?;let bio:String=r.get(2)?;Ok(json!({"telegram_id":owner,"first_name":name.filter(|s|!s.is_empty()).unwrap_or_else(||"Тренер".into()),"avatar_url":p.avatar_url(owner,file.as_deref(),true,now),"bio":bio}))}).optional().map_err(|_|ProfileError::Storage)?;
        let seconds = start - now;
        trainings.push(json!({"id":id,"title":title,"type":typ,"start_at":start,"date":local.format("%Y-%m-%d").to_string(),"time":local.format("%H:%M").to_string(),"datetime_local":local.format("%d.%m.%Y %H:%M").to_string(),"duration_min":duration,"capacity":capacity,"level":level,"booked_count":booked,"waitlist_count":waitlist,"reminder_enabled":reminder,"reminder_offset_min":offset,"deleted":false,"can_self_cancel":seconds>=270*60,"is_booked":false,"is_waitlisted":false,"is_pending":false,"is_past":seconds<=0,"seconds_left":seconds,"cancel_lock_minutes":270,"ics_token":auth::issue_scoped_token(&p.bot_token,"ics",id,viewer,now,86400),"coach":trainer,"pending_count":pending}));
    }
    Ok(json!({"ok":true,"trainings":trainings}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    const INIT:&str="auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    #[test]
    fn club_schedule_exposes_other_coaches_but_not_edit_access() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO users(telegram_id,first_name,bio,created_at) VALUES(77,'Другой','Опыт',0),(88,'Игрок','',0); INSERT INTO trainings(id,title,start_at,created_at,created_by,deleted_at) VALUES(1,'mine',2000000,0,42,NULL),(2,'other',2000001,0,77,NULL),(3,'past',999999,0,77,NULL),(4,'deleted',2000001,0,77,1),(5,'unknown',2000001,0,NULL,NULL); INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(2,88,'pending',0);").unwrap();
        let mut p = Profiles {
            db: Arc::new(Mutex::new(db)),
            bot_token: "123456:TEST-TOKEN".into(),
            owner: 42,
            timezone: chrono_tz::UTC,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: Arc::new(Mutex::new(crate::rate_limit::RateLimiter::new(
                60,
                std::time::Duration::from_secs(60),
            ))),
        };
        let result = list(&p, INIT, 1_000_000).unwrap();
        let items = result["trainings"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], 2);
        assert_eq!(items[0]["pending_count"], 1);
        assert_eq!(items[0]["coach"]["first_name"], "Другой");
        assert!(items[0]["coach"].get("username").is_none());
        let edit = coach::EditTraining {
            date: "2001-09-10".into(),
            time: "12:00".into(),
            training_type: "Test".into(),
        };
        assert!(matches!(
            coach::edit(&p, INIT, 1_000_000, 2, edit),
            Err(ProfileError::Rule(404, _))
        ));
        p.owner = 99;
        assert!(matches!(
            list(&p, INIT, 1_000_000),
            Err(ProfileError::Forbidden(_))
        ));
    }
}

//! Coach-owned training management. Notifications are not implemented yet.
use crate::profile::{ProfileError, Profiles};
use chrono::{LocalResult, Offset, TimeZone};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};

fn default_type() -> String {
    "Тренировка".into()
}
fn default_capacity() -> i64 {
    4
}
fn default_duration() -> i64 {
    60
}
fn default_offset() -> i64 {
    270
}
fn default_true() -> bool {
    true
}
#[derive(Deserialize)]
pub struct NewTraining {
    pub date: String,
    pub time: String,
    #[serde(default = "default_type")]
    pub training_type: String,
    #[serde(default = "default_capacity")]
    pub capacity: i64,
    #[serde(default = "default_duration")]
    pub duration_min: i64,
    #[serde(default)]
    pub level: String,
    #[serde(default = "default_true")]
    pub reminder_enabled: bool,
    #[serde(default = "default_offset")]
    pub reminder_offset_min: i64,
    #[serde(default)]
    pub force: bool,
}
fn coach(profiles: &Profiles, db: &Connection, init: &str, now: i64) -> Result<i64, ProfileError> {
    let (id, user) = profiles.user_id(init, now)?;
    profiles.register(db, id, &user, now)?;
    let allowed: bool = id == profiles.owner
        || db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| ProfileError::Storage)?;
    if !allowed {
        return Err(ProfileError::Forbidden("Доступ только для тренера."));
    }
    Ok(id)
}
fn local_timestamp(date: &str, time: &str, tz: chrono_tz::Tz) -> Result<i64, ProfileError> {
    if date.len() != 10 || time.len() != 5 {
        return Err(ProfileError::Rule(422, "Некорректные дата/время."));
    }
    let parsed = chrono::NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M")
        .map_err(|_| ProfileError::Rule(422, "Некорректные дата/время."))?;
    Ok(match tz.from_local_datetime(&parsed) {
        LocalResult::Single(v) | LocalResult::Ambiguous(v, _) => v.timestamp(),
        LocalResult::None => {
            let previous = tz
                .from_local_datetime(&(parsed - chrono::Duration::hours(3)))
                .earliest()
                .ok_or(ProfileError::Storage)?;
            parsed.and_utc().timestamp() - i64::from(previous.offset().fix().local_minus_utc())
        }
    })
}
pub fn create(
    profiles: &Profiles,
    init: &str,
    now: i64,
    value: NewTraining,
) -> Result<Value, ProfileError> {
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let id = coach(profiles, &db, init, now)?;
    let name_len = value.training_type.chars().count();
    if !(1..=100).contains(&name_len)
        || !(1..=100).contains(&value.capacity)
        || !(15..=600).contains(&value.duration_min)
        || value.level.chars().count() > 60
        || !(0..=2880).contains(&value.reminder_offset_min)
    {
        return Err(ProfileError::Invalid("Некорректные данные тренировки."));
    }
    let start = local_timestamp(&value.date, &value.time, profiles.timezone)?;
    if start <= now {
        return Err(ProfileError::Rule(
            400,
            "Нельзя создать тренировку в прошлом",
        ));
    }
    let level = value.level.trim();
    if !level.is_empty()
        && ![
            "Новичок (Into)",
            "Начинающий (Beginner)",
            "Уверенный любитель (С)",
        ]
        .contains(&level)
    {
        return Err(ProfileError::Rule(422, "Неизвестный уровень игры"));
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    if !value.force {
        let duplicate: Option<i64>=tx.query_row("SELECT id FROM trainings WHERE deleted_at IS NULL AND start_at=?1 AND training_type=?2 AND created_by=?3 LIMIT 1",params![start,value.training_type,id],|r|r.get(0)).optional().map_err(|_|ProfileError::Storage)?;
        if let Some(existing) = duplicate {
            return Err(ProfileError::Duplicate(existing));
        }
    }
    tx.execute("INSERT INTO trainings(title,training_type,start_at,duration_min,capacity,level,reminder_offset_min,reminder_enabled,created_by,created_at) VALUES(?1,?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![value.training_type,start,value.duration_min,value.capacity,level,value.reminder_offset_min,value.reminder_enabled,id,now]).map_err(|_|ProfileError::Storage)?;
    let training_id = tx.last_insert_rowid();
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true,"id":training_id}))
}
#[derive(Clone, Copy)]
pub enum Action {
    Delete,
    Restore,
    Purge,
}
pub fn lifecycle(
    profiles: &Profiles,
    init: &str,
    now: i64,
    training_id: i64,
    action: Action,
) -> Result<Value, ProfileError> {
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let id = coach(profiles, &db, init, now)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    let row: Option<(Option<i64>, Option<i64>)> = tx
        .query_row(
            "SELECT created_by,deleted_at FROM trainings WHERE id=?1",
            [training_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?;
    let found = row
        .filter(|(owner, deleted)| {
            *owner == Some(id)
                && match action {
                    Action::Delete => deleted.unwrap_or(0) == 0,
                    _ => deleted.unwrap_or(0) != 0,
                }
        })
        .is_some();
    if !found {
        return Err(ProfileError::Rule(
            404,
            match action {
                Action::Delete => "Тренировка не найдена",
                Action::Restore => "Тренировка не найдена в корзине",
                Action::Purge => "В корзине такой тренировки нет",
            },
        ));
    }
    match action {
        Action::Delete => {tx.execute("UPDATE trainings SET deleted_at=?1,deleted_by=?2 WHERE id=?3",params![now,id,training_id]).map_err(|_|ProfileError::Storage)?;},
        Action::Restore => {tx.execute("UPDATE trainings SET deleted_at=NULL,deleted_by=NULL WHERE id=?1",[training_id]).map_err(|_|ProfileError::Storage)?;},
        Action::Purge => {for sql in ["DELETE FROM reminder_deliveries WHERE training_id=?1","DELETE FROM bookings WHERE training_id=?1","DELETE FROM trainings WHERE id=?1","DELETE FROM event_notifications WHERE entity_id=?1 AND kind IN ('training','training_booked','training_waitlist','training_rejected')"] {tx.execute(sql,[training_id]).map_err(|_|ProfileError::Storage)?;}},
    }
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(match action {
        Action::Delete => json!({"ok":true,"restorable":true}),
        _ => json!({"ok":true}),
    })
}

pub fn list_active(profiles: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let id = coach(profiles, &db, init, now)?;
    let mut stmt=db.prepare("SELECT t.id,t.title,t.training_type,t.start_at,t.duration_min,t.capacity,t.level,t.reminder_enabled,t.reminder_offset_min,t.created_by,(SELECT count(*) FROM bookings b WHERE b.training_id=t.id AND b.status='booked'),(SELECT count(*) FROM bookings b WHERE b.training_id=t.id AND b.status='waitlist'),(SELECT count(*) FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=t.id AND b.status='pending') FROM trainings t WHERE t.start_at>=?1 AND t.deleted_at IS NULL AND (t.created_by=?2 OR EXISTS(SELECT 1 FROM event_coauthors c WHERE c.event_type='training' AND c.event_id=t.id AND c.user_id=?2)) ORDER BY t.start_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map(params![now, id], |r| {
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
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, i64>(11)?,
                r.get::<_, i64>(12)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut items = Vec::new();
    for row in rows {
        let (
            training_id,
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
        let local = chrono::DateTime::<chrono::Utc>::from_timestamp(start, 0)
            .ok_or(ProfileError::Storage)?
            .with_timezone(&profiles.timezone);
        let seconds = start - now;
        items.push(json!({"id":training_id,"title":title,"type":typ,"start_at":start,"date":local.format("%Y-%m-%d").to_string(),"time":local.format("%H:%M").to_string(),"datetime_local":local.format("%d.%m.%Y %H:%M").to_string(),"duration_min":duration,"capacity":capacity,"level":level,"booked_count":booked,"waitlist_count":waitlist,"reminder_enabled":reminder,"reminder_offset_min":offset,"deleted":false,"can_self_cancel":seconds>=270*60,"is_booked":false,"is_waitlisted":false,"is_pending":false,"is_past":seconds<=0,"seconds_left":seconds,"cancel_lock_minutes":270,"ics_token":crate::auth::issue_scoped_token(&profiles.bot_token,"ics",training_id,id,now,86_400),"is_owner":owner==Some(id),"can_manage":true,"pending_count":pending}));
    }
    Ok(json!({"ok":true,"trainings":items}))
}

#[cfg(test)]
mod tests {
    use super::*;
    const INIT: &str = "auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    fn service(owner: i64) -> Profiles {
        let mut db = Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        Profiles {
            db: std::sync::Arc::new(std::sync::Mutex::new(db)),
            bot_token: "123456:TEST-TOKEN".into(),
            owner,
            timezone: chrono_tz::Europe::Moscow,
            require_event_approval: false,
            phone_required_from: None,
            booking_limiter: std::sync::Arc::new(std::sync::Mutex::new(
                crate::rate_limit::RateLimiter::new(60, std::time::Duration::from_secs(60)),
            )),
        }
    }
    fn training(force: bool) -> NewTraining {
        serde_json::from_value(json!({"date":"2001-09-10","time":"12:00","force":force})).unwrap()
    }
    #[test]
    fn owner_create_duplicate_delete_restore_and_purge() {
        let profiles = service(42);
        let id = create(&profiles, INIT, 1_000_000, training(false)).unwrap()["id"]
            .as_i64()
            .unwrap();
        {
            let db = profiles.db.lock().unwrap();
            db.execute("INSERT INTO trainings(id,title,start_at,created_at,created_by) VALUES (500,'Shared',2000000,0,77)",[]).unwrap();
            db.execute("INSERT INTO trainings(id,title,start_at,created_at,created_by) VALUES (501,'Private',2000000,0,77)",[]).unwrap();
            db.execute("INSERT INTO event_coauthors(event_type,event_id,user_id,created_at) VALUES ('training',500,42,0)",[]).unwrap();
        }
        let rows = list_active(&profiles, INIT, 1_000_000).unwrap();
        let items = rows["trainings"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let shared = items.iter().find(|r| r["id"] == 500).unwrap();
        assert_eq!(shared["is_owner"], false);
        assert_eq!(shared["can_manage"], true);
        assert!(matches!(
            lifecycle(&profiles, INIT, 1_000_000, 500, Action::Delete),
            Err(ProfileError::Rule(404, _))
        ));
        assert!(
            matches!(create(&profiles,INIT,1_000_000,training(false)),Err(ProfileError::Duplicate(x)) if x==id)
        );
        assert!(create(&profiles, INIT, 1_000_000, training(true)).is_ok());
        profiles.db.lock().unwrap().execute("INSERT INTO bookings(training_id,user_id,status,created_at) VALUES (?1,2,'booked',0)",[id]).unwrap();
        assert_eq!(
            lifecycle(&profiles, INIT, 1_000_000, id, Action::Delete).unwrap()["restorable"],
            true
        );
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM bookings WHERE training_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert!(lifecycle(&profiles, INIT, 1_000_000, id, Action::Restore).is_ok());
        assert!(lifecycle(&profiles, INIT, 1_000_000, id, Action::Purge).is_err());
        lifecycle(&profiles, INIT, 1_000_000, id, Action::Delete).unwrap();
        lifecycle(&profiles, INIT, 1_000_000, id, Action::Purge).unwrap();
        assert_eq!(
            profiles
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM bookings WHERE training_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    #[test]
    fn player_cannot_manage_training() {
        let profiles = service(123);
        assert!(matches!(
            create(&profiles, INIT, 1_000_000, training(false)),
            Err(ProfileError::Forbidden(_))
        ));
        assert!(matches!(
            lifecycle(&profiles, INIT, 1_000_000, 1, Action::Delete),
            Err(ProfileError::Forbidden(_))
        ));
    }
}

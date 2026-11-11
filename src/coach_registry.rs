//! Coach roster and permissions. Telegram role-change notifications remain to be migrated.
use crate::{
    coach,
    profile::{ProfileError, Profiles},
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CoachInput {
    telegram_id: Option<i64>,
    username: Option<String>,
}
#[derive(Deserialize)]
pub struct ManagerInput {
    enabled: bool,
}
pub(crate) fn manager(
    profiles: &Profiles,
    db: &Connection,
    init: &str,
    now: i64,
) -> Result<i64, ProfileError> {
    let actor = coach::coach(profiles, db, init, now)?;
    let allowed = actor == profiles.owner
        || db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1 AND is_manager=1)",
                [actor],
                |r| r.get::<_, bool>(0),
            )
            .map_err(|_| ProfileError::Storage)?;
    if !allowed {
        return Err(ProfileError::Forbidden("Недостаточно прав."));
    }
    Ok(actor)
}
fn resolve(db: &Connection, value: CoachInput) -> Result<Option<i64>, ProfileError> {
    if value
        .username
        .as_ref()
        .is_some_and(|s| s.chars().count() > 64)
    {
        return Err(ProfileError::Invalid("Некорректный username."));
    }
    if value.telegram_id.is_some() {
        return Ok(value.telegram_id);
    }
    match value.username {
        Some(name) if !name.is_empty() => db
            .query_row(
                "SELECT telegram_id FROM users WHERE username=?1 COLLATE NOCASE",
                [name.trim_start_matches('@')],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| ProfileError::Storage),
        _ => Ok(None),
    }
}
pub fn list(profiles: &Profiles, init: &str, now: i64) -> Result<Value, ProfileError> {
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    coach::coach(profiles, &db, init, now)?;
    let mut stmt=db.prepare("SELECT c.telegram_id,c.is_manager,u.first_name,u.username,u.show_username,u.avatar_file_id FROM coaches c LEFT JOIN users u ON u.telegram_id=c.telegram_id ORDER BY c.created_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<bool>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut coaches = Vec::new();
    for row in rows {
        let (id, manager, name, username, visible, file) =
            row.map_err(|_| ProfileError::Storage)?;
        coaches.push(json!({"telegram_id":id,"first_name":name.filter(|s|!s.is_empty()).unwrap_or_else(||"Тренер".into()),"username":username,"username_visible":visible.unwrap_or(false),"avatar_url":profiles.avatar_url(id,file.as_deref(),true,now),"is_owner":id==profiles.owner,"is_manager":manager}));
    }
    Ok(json!({"ok":true,"coaches":coaches}))
}
pub fn add(
    profiles: &Profiles,
    init: &str,
    now: i64,
    value: CoachInput,
) -> Result<Value, ProfileError> {
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let actor = manager(profiles, &db, init, now)?;
    let via_username =
        value.telegram_id.is_none() && value.username.as_ref().is_some_and(|s| !s.is_empty());
    let target = resolve(&db, value)?.ok_or(if via_username {
        ProfileError::Rule(
            404,
            "Пользователь не найден. Попросите его сначала запустить бота (/start).",
        )
    } else {
        ProfileError::Rule(422, "Укажите telegram_id или @username.")
    })?;
    if target == actor {
        return Err(ProfileError::Rule(400, "Это ваш собственный аккаунт."));
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    let created = tx
        .execute(
            "INSERT OR IGNORE INTO coaches(telegram_id,added_by,created_at) VALUES(?1,?2,?3)",
            params![target, actor, now],
        )
        .map_err(|_| ProfileError::Storage)?
        != 0;
    if created {
        tx.execute("UPDATE users SET is_coach=1 WHERE telegram_id=?1", [target])
            .map_err(|_| ProfileError::Storage)?;
        tx.execute("UPDATE coach_applications SET status='approved',decided_by=?1,decided_at=?2 WHERE telegram_id=?3",params![actor,now,target]).map_err(|_|ProfileError::Storage)?;
    }
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true,"created":created,"telegram_id":target}))
}
pub fn remove(
    profiles: &Profiles,
    init: &str,
    now: i64,
    target: i64,
) -> Result<Value, ProfileError> {
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    manager(profiles, &db, init, now)?;
    if target == profiles.owner {
        return Err(ProfileError::Rule(400, "Нельзя снять владельца."));
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    tx.execute("DELETE FROM coaches WHERE telegram_id=?1", [target])
        .map_err(|_| ProfileError::Storage)?;
    tx.execute(
        "DELETE FROM event_coauthors WHERE event_type='training' AND user_id=?1",
        [target],
    )
    .map_err(|_| ProfileError::Storage)?;
    tx.execute("UPDATE users SET is_coach=0 WHERE telegram_id=?1", [target])
        .map_err(|_| ProfileError::Storage)?;
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true}))
}
pub fn set_manager(
    profiles: &Profiles,
    init: &str,
    now: i64,
    target: i64,
    value: ManagerInput,
) -> Result<Value, ProfileError> {
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let actor = coach::coach(profiles, &db, init, now)?;
    if actor != profiles.owner {
        return Err(ProfileError::Forbidden("Доступ только для владельца."));
    }
    if target == profiles.owner {
        return Err(ProfileError::Rule(
            400,
            "Владелец и так управляет реестром.",
        ));
    }
    let changed = db
        .execute(
            "UPDATE coaches SET is_manager=?1 WHERE telegram_id=?2",
            params![value.enabled, target],
        )
        .map_err(|_| ProfileError::Storage)?;
    if changed == 0 {
        return Err(ProfileError::Rule(404, "Сначала добавьте тренера."));
    }
    Ok(json!({"ok":true}))
}

/// Transfer retains the training and its existing bookings. Telegram alert is pending migration.
pub fn transfer(
    profiles: &Profiles,
    init: &str,
    now: i64,
    training: i64,
    value: CoachInput,
) -> Result<Value, ProfileError> {
    let mut db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    let actor = coach::coach(profiles, &db, init, now)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    let creator = tx
        .query_row(
            "SELECT created_by FROM trainings WHERE id=?1 AND (deleted_at IS NULL OR deleted_at=0)",
            [training],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?
        .ok_or(ProfileError::Rule(404, "Тренировка не найдена"))?;
    if creator != Some(actor) && actor != profiles.owner {
        return Err(ProfileError::Rule(404, "Тренировка не найдена"));
    }
    let target =
        resolve(&tx, value)?.ok_or(ProfileError::Rule(422, "Укажите тренера (telegram_id)."))?;
    let coach: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
            [target],
            |r| r.get(0),
        )
        .map_err(|_| ProfileError::Storage)?;
    if !coach {
        return Err(ProfileError::Rule(400, "Получатель должен быть тренером."));
    }
    if creator == Some(target) {
        return Err(ProfileError::Rule(
            400,
            "Занятие уже принадлежит этому тренеру.",
        ));
    }
    tx.execute(
        "UPDATE trainings SET created_by=?1 WHERE id=?2",
        params![target, training],
    )
    .map_err(|_| ProfileError::Storage)?;
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(json!({"ok":true,"telegram_id":target}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    const INIT:&str="auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
    #[test]
    fn owner_and_manager_roles_and_removal_are_enforced() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO coaches(telegram_id,is_manager,created_at) VALUES(42,0,0); INSERT INTO users(telegram_id,username,created_at) VALUES(77,'Coach77',0); INSERT INTO coach_applications(telegram_id,status,created_at) VALUES(77,'pending',0);").unwrap();
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
        let input = || serde_json::from_value(json!({"username":"@coach77"})).unwrap();
        assert_eq!(add(&p, INIT, 1_000_000, input()).unwrap()["created"], true);
        assert_eq!(add(&p, INIT, 1_000_000, input()).unwrap()["created"], false);
        {
            let db = p.db.lock().unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT status FROM coach_applications WHERE telegram_id=77",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "approved"
            );
            db.execute("INSERT INTO event_coauthors(event_type,event_id,user_id,created_at) VALUES('training',1,77,0)",[]).unwrap();
        }
        assert!(set_manager(&p, INIT, 1_000_000, 77, ManagerInput { enabled: true }).is_ok());
        assert!(matches!(
            remove(&p, INIT, 1_000_000, 42),
            Err(ProfileError::Rule(400, _))
        ));
        p.owner = 999;
        assert!(matches!(
            add(
                &p,
                INIT,
                1_000_000,
                serde_json::from_value(json!({"telegram_id":78})).unwrap()
            ),
            Err(ProfileError::Forbidden(_))
        ));
        p.db.lock()
            .unwrap()
            .execute("UPDATE coaches SET is_manager=1 WHERE telegram_id=42", [])
            .unwrap();
        p.db.lock().unwrap().execute_batch("INSERT INTO trainings(id,title,start_at,created_at,created_by) VALUES(1,'Training',2000000,0,77); INSERT INTO bookings(training_id,user_id,status,created_at) VALUES(1,77,'booked',0);").unwrap();
        let target = || serde_json::from_value(json!({"telegram_id":42})).unwrap();
        assert!(matches!(
            transfer(&p, INIT, 1_000_000, 1, target()),
            Err(ProfileError::Rule(404, _))
        ));
        p.owner = 42;
        transfer(&p, INIT, 1_000_000, 1, target()).unwrap();
        assert_eq!(
            p.db.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM bookings WHERE training_id=1",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        p.owner = 999;
        assert!(matches!(
            set_manager(&p, INIT, 1_000_000, 77, ManagerInput { enabled: false }),
            Err(ProfileError::Forbidden(_))
        ));
        assert!(matches!(
            remove(&p, INIT, 1_000_000, 999),
            Err(ProfileError::Rule(400, _))
        ));
        remove(&p, INIT, 1_000_000, 77).unwrap();
        let db = p.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM event_coauthors WHERE user_id=77",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT is_coach FROM users WHERE telegram_id=77", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

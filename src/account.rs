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

/// Result of account deletion, including promoted match players for best-effort notices.
pub struct DeletedAccount {
    pub user: i64,
    pub promoted_matches: Vec<(i64, i64)>,
    pub promoted_trainings: Vec<(i64, i64)>,
}
/// Atomically erase account records and fill newly available places.
pub fn delete(
    p: &Profiles,
    init: &str,
    now: i64,
    confirm: bool,
) -> Result<DeletedAccount, ProfileError> {
    use rusqlite::{params, TransactionBehavior};
    let (id, user) = p.user_id(init, now)?;
    let mut db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    p.register(&db, id, &user, now)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ProfileError::Storage)?;
    let coach: bool = id == p.owner
        || tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coaches WHERE telegram_id=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| ProfileError::Storage)?;
    if coach {
        return Err(ProfileError::Rule(
            403,
            "Сначала снимите роль тренера (занятия нужно передать другому).",
        ));
    }
    if !confirm {
        return Err(ProfileError::Rule(
            400,
            "Подтвердите удаление: ?confirm=true",
        ));
    }
    let owns:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM matches WHERE created_by=?1 AND deleted_at IS NULL AND start_at>?2)",params![id,now],|r|r.get(0)).map_err(|_|ProfileError::Storage)?;
    if owns {
        return Err(ProfileError::Rule(
            409,
            "Сначала отмените созданные вами предстоящие матчи.",
        ));
    }
    let matches = {
        let mut stmt=tx.prepare("SELECT p.match_id,p.status,m.deleted_at FROM match_players p LEFT JOIN matches m ON m.id=p.match_id WHERE p.user_id=?1 ORDER BY p.match_id").map_err(|_|ProfileError::Storage)?;
        let rows = stmt
            .query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(|_| ProfileError::Storage)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| ProfileError::Storage)?
    };
    let mut promoted_matches = Vec::new();
    for (event, status, deleted) in matches {
        tx.execute(
            "DELETE FROM match_players WHERE match_id=?1 AND user_id=?2",
            params![event, id],
        )
        .map_err(|_| ProfileError::Storage)?;
        if status == "booked" {
            let next:Option<i64>=tx.query_row("SELECT user_id FROM match_players WHERE match_id=?1 AND status='waitlist' ORDER BY created_at,id LIMIT 1",[event],|r|r.get(0)).optional().map_err(|_|ProfileError::Storage)?;
            if let Some(next) = next {
                tx.execute(
                    "UPDATE match_players SET status='booked' WHERE match_id=?1 AND user_id=?2",
                    params![event, next],
                )
                .map_err(|_| ProfileError::Storage)?;
                if deleted.unwrap_or(0) == 0 {
                    promoted_matches.push((event, next));
                }
            }
        }
    }
    let bookings = {
        let mut stmt=tx.prepare("SELECT b.training_id,t.capacity FROM bookings b JOIN trainings t ON t.id=b.training_id WHERE b.user_id=?1 AND b.status IN ('booked','waitlist') AND (t.deleted_at IS NULL OR t.deleted_at=0)").map_err(|_|ProfileError::Storage)?;
        let rows = stmt
            .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|_| ProfileError::Storage)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| ProfileError::Storage)?
    };
    let mut promoted_trainings = Vec::new();
    for (training, capacity) in bookings {
        tx.execute(
            "UPDATE bookings SET status='cancelled',position=0 WHERE training_id=?1 AND user_id=?2",
            params![training, id],
        )
        .map_err(|_| ProfileError::Storage)?;
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM bookings WHERE training_id=?1 AND status='booked'",
                [training],
                |r| r.get(0),
            )
            .map_err(|_| ProfileError::Storage)?;
        if count < capacity {
            let next:Option<i64>=tx.query_row("SELECT user_id FROM bookings WHERE training_id=?1 AND status='waitlist' ORDER BY position ASC LIMIT 1",[training],|r|r.get(0)).optional().map_err(|_|ProfileError::Storage)?;
            if let Some(next) = next {
                tx.execute("UPDATE bookings SET status='booked',position=0 WHERE training_id=?1 AND user_id=?2",params![training,next]).map_err(|_|ProfileError::Storage)?;
                promoted_trainings.push((training, next));
            }
        }
    }
    for sql in [
        "DELETE FROM messages WHERE from_user=?1 OR to_user=?1",
        "DELETE FROM coach_applications WHERE telegram_id=?1",
        "DELETE FROM feedback WHERE user_id=?1",
        "DELETE FROM event_coauthors WHERE user_id=?1",
        "UPDATE event_coauthors SET added_by=NULL WHERE added_by=?1",
        "DELETE FROM bookings WHERE user_id=?1",
        "DELETE FROM match_players WHERE user_id=?1",
        "DELETE FROM reminder_deliveries WHERE user_id=?1",
        "DELETE FROM event_notifications WHERE chat_id=?1 OR user_id=?1",
        "UPDATE matches SET created_by=0 WHERE created_by=?1",
        "UPDATE match_players SET decided_by=NULL WHERE decided_by=?1",
        "UPDATE coach_applications SET decided_by=NULL WHERE decided_by=?1",
        "UPDATE coaches SET added_by=NULL WHERE added_by=?1",
        "UPDATE trainings SET created_by=NULL WHERE created_by=?1",
        "UPDATE trainings SET deleted_by=NULL WHERE deleted_by=?1",
        "DELETE FROM users WHERE telegram_id=?1",
    ] {
        tx.execute(sql, [id]).map_err(|_| ProfileError::Storage)?;
    }
    tx.commit().map_err(|_| ProfileError::Storage)?;
    Ok(DeletedAccount {
        user: id,
        promoted_matches,
        promoted_trainings,
    })
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
    #[test]
    fn deletion_guards_rolls_back_and_promotes_waitlists() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute_batch("INSERT INTO users(telegram_id,created_at) VALUES(42,0),(55,0),(56,0); INSERT INTO trainings(id,title,start_at,capacity,created_at,deleted_at) VALUES(1,'active',2000000,1,0,NULL),(2,'deleted',2000000,1,0,1); INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES(1,42,'booked',0,0),(1,55,'waitlist',1,1),(2,42,'booked',0,0),(2,56,'waitlist',1,1); INSERT INTO matches(id,start_at,created_by,created_at) VALUES(1,2000000,99,0),(2,2000000,42,0); INSERT INTO match_players(match_id,user_id,status,created_at) VALUES(1,42,'booked',0),(1,55,'waitlist',1); INSERT INTO messages(from_user,to_user,text,created_at) VALUES(42,55,'private',0),(55,42,'incoming',0); INSERT INTO feedback(user_id,kind,text,created_at) VALUES(42,'bug','private',0);").unwrap();
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
        assert!(matches!(
            delete(&p, INIT, 1_000_000, true),
            Err(ProfileError::Rule(403, _))
        ));
        p.owner = 99;
        assert!(matches!(
            delete(&p, INIT, 1_000_000, false),
            Err(ProfileError::Rule(400, _))
        ));
        assert!(matches!(
            delete(&p, INIT, 1_000_000, true),
            Err(ProfileError::Rule(409, _))
        ));
        {
            let db = p.db.lock().unwrap();
            db.execute("UPDATE matches SET deleted_at=1 WHERE id=2", [])
                .unwrap();
            db.execute_batch("CREATE TRIGGER fail_user_delete BEFORE DELETE ON users BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
        }
        assert!(matches!(
            delete(&p, INIT, 1_000_000, true),
            Err(ProfileError::Storage)
        ));
        {
            let db = p.db.lock().unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT status FROM bookings WHERE training_id=1 AND user_id=55",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "waitlist"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM users WHERE telegram_id=42", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            db.execute_batch("DROP TRIGGER fail_user_delete").unwrap();
        }
        let result = delete(&p, INIT, 1_000_000, true).unwrap();
        assert_eq!(result.user, 42);
        assert_eq!(result.promoted_matches, vec![(1, 55)]);
        assert_eq!(result.promoted_trainings, vec![(1, 55)]);
        let db = p.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT status FROM bookings WHERE training_id=1 AND user_id=55",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "booked"
        );
        assert_eq!(
            db.query_row(
                "SELECT status FROM bookings WHERE training_id=2 AND user_id=56",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "waitlist"
        );
        assert_eq!(
            db.query_row(
                "SELECT status FROM match_players WHERE match_id=1 AND user_id=55",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "booked"
        );
        for sql in [
            "SELECT count(*) FROM users WHERE telegram_id=42",
            "SELECT count(*) FROM bookings WHERE user_id=42",
            "SELECT count(*) FROM messages WHERE from_user=42 OR to_user=42",
            "SELECT count(*) FROM feedback WHERE user_id=42",
        ] {
            assert_eq!(db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        assert_eq!(
            db.query_row("SELECT created_by FROM matches WHERE id=2", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

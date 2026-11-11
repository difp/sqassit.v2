//! Transactional match attendance primitives. HTTP authorization is handled separately.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

/// Add a player, or return their existing status. A full match does not create a row.
pub fn join(
    db: &mut Connection,
    match_id: i64,
    user_id: i64,
    approval: bool,
    now: i64,
) -> rusqlite::Result<String> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT status FROM match_players WHERE match_id=?1 AND user_id=?2",
            params![match_id, user_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(status) = existing {
        return Ok(status);
    }
    let capacity: i64 = tx.query_row(
        "SELECT max_players FROM matches WHERE id=?1",
        [match_id],
        |r| r.get(0),
    )?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM match_players WHERE match_id=?1 AND status='booked'",
        [match_id],
        |r| r.get(0),
    )?;
    let status = if approval {
        "pending"
    } else if count >= capacity {
        "full"
    } else {
        "booked"
    };
    if status != "full" {
        tx.execute(
            "INSERT INTO match_players(match_id,user_id,status,created_at) VALUES(?1,?2,?3,?4)",
            params![match_id, user_id, status, now],
        )?;
    }
    tx.commit()?;
    Ok(status.to_owned())
}

/// Approve a pending request, putting it in the queue if no place remains.
pub fn approve(
    db: &mut Connection,
    match_id: i64,
    user_id: i64,
    actor: i64,
    now: i64,
) -> rusqlite::Result<Option<String>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: Option<String> = tx
        .query_row(
            "SELECT status FROM match_players WHERE match_id=?1 AND user_id=?2",
            params![match_id, user_id],
            |r| r.get(0),
        )
        .optional()?;
    if current.as_deref() != Some("pending") {
        return Ok(current);
    }
    let capacity: i64 = tx.query_row(
        "SELECT max_players FROM matches WHERE id=?1",
        [match_id],
        |r| r.get(0),
    )?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM match_players WHERE match_id=?1 AND status='booked'",
        [match_id],
        |r| r.get(0),
    )?;
    let status = if count < capacity {
        "booked"
    } else {
        "waitlist"
    };
    tx.execute("UPDATE match_players SET status=?1,decided_by=?2,decided_at=?3 WHERE match_id=?4 AND user_id=?5", params![status,actor,now,match_id,user_id])?;
    tx.commit()?;
    Ok(Some(status.to_owned()))
}

pub fn reject(
    db: &Connection,
    match_id: i64,
    user_id: i64,
    actor: i64,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(db.execute("UPDATE match_players SET status='rejected',decided_by=?1,decided_at=?2 WHERE match_id=?3 AND user_id=?4 AND status='pending'", params![actor,now,match_id,user_id])? != 0)
}

/// Remove a player and promote the oldest waiting player atomically.
pub fn leave(
    db: &mut Connection,
    match_id: i64,
    user_id: i64,
) -> rusqlite::Result<(bool, Option<i64>)> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM match_players WHERE match_id=?1 AND user_id=?2",
            params![match_id, user_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(status) = status else {
        return Ok((false, None));
    };
    tx.execute(
        "DELETE FROM match_players WHERE match_id=?1 AND user_id=?2",
        params![match_id, user_id],
    )?;
    let promoted = if status == "booked" {
        tx.query_row("SELECT user_id FROM match_players WHERE match_id=?1 AND status='waitlist' ORDER BY created_at,id LIMIT 1", [match_id], |r| r.get::<_,i64>(0)).optional()?
    } else {
        None
    };
    if let Some(id) = promoted {
        tx.execute(
            "UPDATE match_players SET status='booked' WHERE match_id=?1 AND user_id=?2",
            params![match_id, id],
        )?;
    }
    tx.commit()?;
    Ok((true, promoted))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_last_place_is_not_overbooked() {
        let path = std::env::temp_dir().join(format!(
            "sqassit-match-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut db = Connection::open(&path).unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO matches(id,start_at,max_players,created_by,created_at) VALUES(1,100,1,1,0)", []).unwrap();
        drop(db);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (1..=2)
            .map(|user| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut db = Connection::open(path).unwrap();
                    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                    barrier.wait();
                    join(&mut db, 1, user, false, 1).unwrap()
                })
            })
            .collect();
        let mut statuses: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        statuses.sort();
        assert_eq!(statuses, ["booked", "full"]);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn approval_queue_is_fifo_and_idempotent() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO matches(id,start_at,max_players,created_by,created_at) VALUES(1,100,1,1,0)", []).unwrap();
        assert_eq!(join(&mut db, 1, 1, false, 1).unwrap(), "booked");
        assert_eq!(join(&mut db, 1, 2, false, 2).unwrap(), "full");
        assert_eq!(join(&mut db, 1, 2, true, 2).unwrap(), "pending");
        assert_eq!(join(&mut db, 1, 3, true, 2).unwrap(), "pending");
        assert_eq!(
            approve(&mut db, 1, 2, 1, 3).unwrap().as_deref(),
            Some("waitlist")
        );
        assert_eq!(
            approve(&mut db, 1, 3, 1, 3).unwrap().as_deref(),
            Some("waitlist")
        );
        assert_eq!(join(&mut db, 1, 2, false, 4).unwrap(), "waitlist");
        assert_eq!(leave(&mut db, 1, 1).unwrap(), (true, Some(2)));
        assert_eq!(leave(&mut db, 1, 2).unwrap(), (true, Some(3)));
        assert_eq!(leave(&mut db, 1, 2).unwrap(), (false, None));
        assert_eq!(join(&mut db, 1, 4, true, 5).unwrap(), "pending");
        assert!(reject(&db, 1, 4, 1, 6).unwrap());
        assert!(!reject(&db, 1, 4, 1, 6).unwrap());
        assert_eq!(join(&mut db, 1, 4, false, 7).unwrap(), "rejected");
    }
}

//! Atomic training bookings compatible with the legacy status machine.
use rusqlite::{params, Connection, OptionalExtension, Result, Transaction, TransactionBehavior};

fn next_position(tx: &Transaction<'_>, training_id: i64) -> Result<i64> {
    tx.query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM bookings WHERE training_id = ?1 AND status = 'waitlist'", [training_id], |r| r.get(0))
}
fn booked_count(tx: &Transaction<'_>, training_id: i64) -> Result<i64> {
    tx.query_row(
        "SELECT COUNT(*) FROM bookings WHERE training_id = ?1 AND status = 'booked'",
        [training_id],
        |r| r.get(0),
    )
}

/// Create or reinstate a booking. Validating access, capacity and event state is the caller's responsibility.
pub fn book(
    connection: &mut Connection,
    training_id: i64,
    user_id: i64,
    capacity: i64,
    requires_approval: bool,
    respect_waitlist: bool,
    now: i64,
) -> Result<String> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT status FROM bookings WHERE training_id = ?1 AND user_id = ?2",
            params![training_id, user_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(status) =
        existing.filter(|s| matches!(s.as_str(), "pending" | "booked" | "waitlist"))
    {
        tx.commit()?;
        return Ok(status);
    }
    let (status, position) = if requires_approval {
        ("pending", 0)
    } else {
        let has_waitlist = respect_waitlist && tx.query_row("SELECT EXISTS(SELECT 1 FROM bookings WHERE training_id = ?1 AND status = 'waitlist')", [training_id], |r| r.get::<_, bool>(0))?;
        if booked_count(&tx, training_id)? < capacity && !has_waitlist {
            ("booked", 0)
        } else {
            ("waitlist", next_position(&tx, training_id)?)
        }
    };
    tx.execute("INSERT INTO bookings (training_id, user_id, status, position, created_at) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(training_id,user_id) DO UPDATE SET status=excluded.status, position=excluded.position, created_at=excluded.created_at", params![training_id, user_id, status, position, now])?;
    tx.commit()?;
    Ok(status.to_owned())
}

pub fn approve(connection: &mut Connection, training_id: i64, user_id: i64) -> Result<String> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT status FROM bookings WHERE training_id=?1 AND user_id=?2",
            params![training_id, user_id],
            |r| r.get(0),
        )
        .optional()?;
    if existing.as_deref() != Some("pending") {
        tx.commit()?;
        return Ok(existing.unwrap_or_default());
    }
    let capacity: i64 = tx
        .query_row(
            "SELECT capacity FROM trainings WHERE id=?1",
            [training_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let (status, position) = if booked_count(&tx, training_id)? < capacity {
        ("booked", 0)
    } else {
        ("waitlist", next_position(&tx, training_id)?)
    };
    tx.execute(
        "UPDATE bookings SET status=?1, position=?2 WHERE training_id=?3 AND user_id=?4",
        params![status, position, training_id, user_id],
    )?;
    tx.commit()?;
    Ok(status.to_owned())
}

pub fn reject(connection: &mut Connection, training_id: i64, user_id: i64) -> Result<bool> {
    Ok(connection.execute("UPDATE bookings SET status='rejected', position=0 WHERE training_id=?1 AND user_id=?2 AND status='pending'", params![training_id, user_id])? > 0)
}

/// Cancel and promote the oldest waiting user if a seat is free.
pub fn cancel(connection: &mut Connection, training_id: i64, user_id: i64) -> Result<Option<i64>> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute("UPDATE bookings SET status='cancelled', position=0 WHERE training_id=?1 AND user_id=?2 AND status IN ('pending','booked','waitlist')", params![training_id,user_id])?;
    let mut promoted = None;
    if changed > 0 {
        let capacity: i64 = tx
            .query_row(
                "SELECT capacity FROM trainings WHERE id=?1",
                [training_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if booked_count(&tx, training_id)? < capacity {
            promoted = tx.query_row("SELECT user_id FROM bookings WHERE training_id=?1 AND status='waitlist' ORDER BY position ASC LIMIT 1", [training_id], |r| r.get(0)).optional()?;
            if let Some(id) = promoted {
                tx.execute("UPDATE bookings SET status='booked', position=0 WHERE training_id=?1 AND user_id=?2", params![training_id,id])?;
            }
        }
    }
    tx.commit()?;
    Ok(promoted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;
    #[test]
    fn booking_approval_and_fifo_are_idempotent() {
        let mut db = Connection::open_in_memory().unwrap();
        storage::initialize(&mut db).unwrap();
        db.execute("INSERT INTO trainings (id,title,start_at,capacity,created_at) VALUES (1,'Test',1000,1,0)", []).unwrap();
        assert_eq!(book(&mut db, 1, 1, 1, false, false, 10).unwrap(), "booked");
        assert_eq!(book(&mut db, 1, 2, 1, true, false, 11).unwrap(), "pending");
        assert_eq!(approve(&mut db, 1, 2).unwrap(), "waitlist");
        assert_eq!(
            book(&mut db, 1, 3, 1, false, false, 12).unwrap(),
            "waitlist"
        );
        assert_eq!(book(&mut db, 1, 1, 1, false, false, 13).unwrap(), "booked");
        assert_eq!(cancel(&mut db, 1, 1).unwrap(), Some(2));
        assert_eq!(cancel(&mut db, 1, 1).unwrap(), None);
        assert_eq!(cancel(&mut db, 1, 2).unwrap(), Some(3));
        assert_eq!(book(&mut db, 1, 4, 1, true, false, 14).unwrap(), "pending");
        assert!(reject(&mut db, 1, 4).unwrap());
        assert!(!reject(&mut db, 1, 4).unwrap());
    }
    #[test]
    fn simultaneous_requests_cannot_overbook() {
        use std::sync::{Arc, Barrier};
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("sqassit-booking-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("test.sqlite");
        let db = storage::open(&path).unwrap();
        db.execute("INSERT INTO trainings (id,title,start_at,capacity,created_at) VALUES (1,'Test',1000,1,0)", []).unwrap();
        drop(db);
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|id| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut db = storage::open(path).unwrap();
                    barrier.wait();
                    book(&mut db, 1, id, 1, false, false, 1).unwrap()
                })
            })
            .collect();
        let statuses: Vec<_> = workers.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(
            statuses.iter().filter(|s| s.as_str() == "booked").count(),
            1
        );
        assert_eq!(
            statuses.iter().filter(|s| s.as_str() == "waitlist").count(),
            7
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

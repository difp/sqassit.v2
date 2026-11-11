//! Opt-in large-directory timing for the coach player search.
//!
//! Run with `cargo test --release --locked --test search_benchmark -- --ignored --nocapture`.
use sqassit::profile::Profiles;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const INIT:&str="auth_date=1000000&query_id=AA&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22%5Cu0418%5Cu0432%5Cu0430%5Cu043d%22%7D&signature=telegram-signature&hash=aad77d89362a2d2634adf06dc7e3e3ac1d2043c386ebe89bc420769fcd8ef362";
const PLAYERS: i64 = 50_000;

#[test]
#[ignore = "manual large-directory timing; run with --ignored --nocapture"]
fn search_fifty_thousand_players() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    sqassit::storage::initialize(&mut db).unwrap();
    db.execute(
        "INSERT INTO coaches(telegram_id,created_at) VALUES(42,0)",
        [],
    )
    .unwrap();
    let seed_started = Instant::now();
    {
        let tx = db.transaction().unwrap();
        for index in 0..PLAYERS {
            tx.execute(
                "INSERT INTO users(telegram_id,first_name,username,created_at) VALUES(?1,?2,?3,0)",
                rusqlite::params![
                    1000 + index,
                    format!("Player {index}"),
                    format!("player{index}")
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let seed = seed_started.elapsed();
    let profiles = Profiles {
        db: Arc::new(Mutex::new(db)),
        bot_token: "123456:TEST-TOKEN".into(),
        owner: 42,
        timezone: chrono_tz::UTC,
        require_event_approval: false,
        phone_required_from: None,
        booking_limiter: Arc::new(Mutex::new(sqassit::rate_limit::RateLimiter::new(
            60,
            Duration::from_secs(60),
        ))),
    };
    let now = 1_000_000;
    let empty_started = Instant::now();
    let all = sqassit::players::list(&profiles, INIT, now, "").unwrap();
    let empty = empty_started.elapsed();
    assert_eq!(all["matched"], PLAYERS);
    let one_started = Instant::now();
    let one = sqassit::players::list(&profiles, INIT, now, "player 43210").unwrap();
    let one_elapsed = one_started.elapsed();
    assert_eq!(one["matched"], 1);
    let miss_started = Instant::now();
    let miss = sqassit::players::list(&profiles, INIT, now, "no such player").unwrap();
    let miss_elapsed = miss_started.elapsed();
    assert_eq!(miss["matched"], 0);
    println!(
        "seed {PLAYERS}: {seed:?}; full list: {empty:?}; one match: {one_elapsed:?}; no match: {miss_elapsed:?}"
    );
}

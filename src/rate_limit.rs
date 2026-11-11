//! In-memory sliding window limiter for a single application process.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct RateLimiter {
    limit: usize,
    window: Duration,
    events: HashMap<String, VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit: limit.max(1),
            window: window.max(Duration::from_secs(1)),
            events: HashMap::new(),
        }
    }

    /// Record an event or return the legacy Retry-After value in seconds.
    pub fn check_at(&mut self, key: &str, now: Instant) -> Result<(), u64> {
        let events = self.events.entry(key.to_owned()).or_default();
        while events
            .front()
            .is_some_and(|event| now.saturating_duration_since(*event) >= self.window)
        {
            events.pop_front();
        }
        if events.len() >= self.limit {
            let elapsed =
                now.saturating_duration_since(*events.front().expect("limit is at least one"));
            return Err(self.window.saturating_sub(elapsed).as_secs().max(1));
        }
        events.push_back(now);
        if self.events.len() > 10_000 {
            self.events.retain(|_, values| {
                values
                    .back()
                    .is_some_and(|last| now.saturating_duration_since(*last) < self.window)
            });
        }
        Ok(())
    }

    pub fn check(&mut self, key: &str) -> Result<(), u64> {
        self.check_at(key, Instant::now())
    }
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn respects_window_per_key_and_boundary() {
        let start = Instant::now();
        let mut limiter = RateLimiter::new(2, Duration::from_secs(10));
        assert_eq!(limiter.check_at("a", start), Ok(()));
        assert_eq!(
            limiter.check_at("a", start + Duration::from_secs(1)),
            Ok(())
        );
        assert_eq!(
            limiter.check_at("a", start + Duration::from_millis(1500)),
            Err(8)
        );
        assert_eq!(
            limiter.check_at("b", start + Duration::from_secs(2)),
            Ok(())
        );
        assert_eq!(
            limiter.check_at("a", start + Duration::from_secs(10)),
            Ok(())
        );
        limiter.clear();
        assert_eq!(
            limiter.check_at("a", start + Duration::from_secs(10)),
            Ok(())
        );
    }
}

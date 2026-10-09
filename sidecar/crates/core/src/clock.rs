//! Clock abstraction so the debounce engine and state machine are testable
//! with a mock clock.

use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[derive(Default)]
pub struct MockClock {
    pub ms: u64,
}

impl MockClock {
    pub fn advance(&mut self, ms: u64) {
        self.ms += ms;
    }
}

impl Clock for MockClock {
    fn now_ms(&self) -> u64 {
        self.ms
    }
}

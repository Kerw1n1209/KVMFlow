//! Retry policy - single copy. Applied to every DDC write and read attempt
//! uniformly on both platforms.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts for one logical operation (>= 1).
    pub attempts: u32,
    /// Delay between attempts in milliseconds.
    pub delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            delay_ms: 500,
        }
    }
}

impl RetryPolicy {
    /// Returns the delay to wait before attempt `next_attempt` (1-based),
    /// or None when `next_attempt` exceeds the budget.
    pub fn delay_before(&self, next_attempt: u32) -> Option<u64> {
        if next_attempt >= 1 && next_attempt <= self.attempts {
            Some(self.delay_ms)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_three_attempts() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_before(1), Some(500));
        assert_eq!(p.delay_before(3), Some(500));
        assert_eq!(p.delay_before(4), None);
        assert_eq!(p.delay_before(0), None);
    }
}

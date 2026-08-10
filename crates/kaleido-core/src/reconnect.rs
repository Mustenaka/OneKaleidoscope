//! Bounded reconnect backoff shared by the mobile worker and path probes.

#![allow(dead_code)]

use std::time::Duration;

const INITIAL_DELAY: Duration = Duration::from_millis(250);
const MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectBackoff {
    next: Duration,
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self {
            next: INITIAL_DELAY,
        }
    }
}

impl ReconnectBackoff {
    pub fn reset(&mut self) {
        self.next = INITIAL_DELAY;
    }

    /// Returns the current delay and advances the bounded sequence.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.checked_mul(2).unwrap_or(MAX_DELAY).min(MAX_DELAY);
        delay
    }

    pub fn current_delay(&self) -> Duration {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::ReconnectBackoff;

    #[test]
    fn backoff_is_bounded_and_resets() {
        let mut backoff = ReconnectBackoff::default();
        assert_eq!(backoff.next_delay(), Duration::from_millis(250));
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
        for _ in 0..16 {
            assert!(backoff.next_delay() <= Duration::from_secs(30));
        }
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_millis(250));
    }
}

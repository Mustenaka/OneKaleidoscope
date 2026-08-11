//! Closed path-selection state machine for the mobile transport.
//!
//! The state machine intentionally knows nothing about iroh or sockets.  A
//! platform transport reports an attempt as established only after it has
//! completed the real authentication handshake.  This prevents a listener,
//! dial attempt, or relay registration from being exposed as an online host.

#![allow(dead_code)]

use std::time::Duration;

use kaleido_proto::host::HostReachability;

/// Ordered connection attempts.  The order is part of the remote-control
/// contract: LAN first, then a direct peer path, and finally the owned relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAttempt {
    Lan,
    PeerToPeer,
    Relay,
}

impl PathAttempt {
    const ALL: [Self; 3] = [Self::Lan, Self::PeerToPeer, Self::Relay];

    fn reachability(self) -> HostReachability {
        match self {
            Self::Lan => HostReachability::LanDirect,
            Self::PeerToPeer => HostReachability::PeerToPeer,
            Self::Relay => HostReachability::Relayed,
        }
    }
}

/// Public status emitted by the state machine.  `Online` is only reachable by
/// [`PathMachine::established`] and therefore always carries a real path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStatus {
    Offline {
        at_ms: i64,
    },
    Connecting {
        attempt: PathAttempt,
        at_ms: i64,
    },
    Online {
        reachability: HostReachability,
        since_ms: i64,
    },
}

impl PathStatus {
    pub fn reachability(&self) -> HostReachability {
        match self {
            Self::Offline { .. } | Self::Connecting { .. } => HostReachability::Offline,
            Self::Online { reachability, .. } => reachability.clone(),
        }
    }

    pub fn at_ms(&self) -> i64 {
        match self {
            Self::Offline { at_ms } | Self::Connecting { at_ms, .. } => *at_ms,
            Self::Online { since_ms, .. } => *since_ms,
        }
    }

    pub fn is_online(&self) -> bool {
        matches!(self, Self::Online { .. })
    }
}

/// A small, deterministic path state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMachine {
    epoch: u64,
    next_attempt: usize,
    status: PathStatus,
}

impl PathMachine {
    pub fn new(now_ms: i64) -> Self {
        Self {
            epoch: 0,
            next_attempt: 0,
            status: PathStatus::Offline { at_ms: now_ms },
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn status(&self) -> PathStatus {
        self.status.clone()
    }

    /// Starts a new ordered attempt for a network generation.
    pub fn start(&mut self, epoch: u64, now_ms: i64) -> PathAttempt {
        self.epoch = epoch;
        self.next_attempt = 0;
        let attempt = PathAttempt::ALL[0];
        self.status = PathStatus::Connecting {
            attempt,
            at_ms: now_ms,
        };
        attempt
    }

    /// Returns the currently selected attempt, if the machine is connecting.
    pub fn current_attempt(&self) -> Option<PathAttempt> {
        match &self.status {
            PathStatus::Connecting { attempt, .. } => Some(*attempt),
            _ => None,
        }
    }

    /// Records a failed attempt and advances to the next tier.  Once all
    /// tiers have failed the state becomes offline; callers may invoke
    /// [`Self::start`] after a backoff or a network-generation change.
    pub fn failed(&mut self, epoch: u64, now_ms: i64) -> Option<PathAttempt> {
        if epoch != self.epoch {
            return None;
        }
        let next = self.next_attempt.saturating_add(1);
        if next >= PathAttempt::ALL.len() {
            self.next_attempt = PathAttempt::ALL.len();
            self.status = PathStatus::Offline { at_ms: now_ms };
            return None;
        }
        self.next_attempt = next;
        let Some(attempt) = PathAttempt::ALL.get(next).copied() else {
            self.next_attempt = PathAttempt::ALL.len();
            self.status = PathStatus::Offline { at_ms: now_ms };
            return None;
        };
        self.status = PathStatus::Connecting {
            attempt,
            at_ms: now_ms,
        };
        Some(attempt)
    }

    /// Marks a path online only when it is the path currently being tried and
    /// the result belongs to the active network generation.
    pub fn established(&mut self, attempt: PathAttempt, epoch: u64, now_ms: i64) -> bool {
        if epoch != self.epoch || self.current_attempt() != Some(attempt) {
            return false;
        }
        self.status = PathStatus::Online {
            reachability: attempt.reachability(),
            since_ms: now_ms,
        };
        true
    }

    /// Invalidates every in-flight result after a network change.  No stale
    /// path can publish online status for the new generation.
    pub fn network_changed(&mut self, epoch: u64, now_ms: i64) -> PathAttempt {
        self.start(epoch, now_ms)
    }

    /// Marks the currently online path as offline.  The next call to
    /// [`Self::start`] begins again at LAN, preserving the tier ordering.
    pub fn offline(&mut self, now_ms: i64) {
        self.status = PathStatus::Offline { at_ms: now_ms };
    }
}

/// Conservative cap used by reconnect callers when converting a path failure
/// to a poll timeout.  Keeping this here makes the bound visible to tests and
/// prevents a platform from sleeping forever while waiting for connectivity.
pub const MAX_PATH_WAIT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::{PathAttempt, PathMachine, PathStatus};
    use kaleido_proto::host::HostReachability;

    #[test]
    fn connecting_is_never_reported_as_online() {
        let mut machine = PathMachine::new(10);
        assert_eq!(machine.start(1, 20), PathAttempt::Lan);
        assert!(!machine.status().is_online());
        assert_eq!(machine.status().reachability(), HostReachability::Offline);
    }

    #[test]
    fn direct_failure_falls_back_to_relay_in_order() {
        let mut machine = PathMachine::new(0);
        machine.start(7, 1);
        assert_eq!(machine.failed(7, 2), Some(PathAttempt::PeerToPeer));
        assert_eq!(machine.failed(7, 3), Some(PathAttempt::Relay));
        assert!(machine.established(PathAttempt::Relay, 7, 4));
        assert_eq!(
            machine.status(),
            PathStatus::Online {
                reachability: HostReachability::Relayed,
                since_ms: 4
            }
        );
    }

    #[test]
    fn stale_network_result_cannot_publish_online() {
        let mut machine = PathMachine::new(0);
        machine.start(1, 1);
        machine.network_changed(2, 2);
        assert!(!machine.established(PathAttempt::Lan, 1, 3));
        assert!(!machine.status().is_online());
        assert_eq!(machine.epoch(), 2);
    }

    #[test]
    fn all_failed_paths_report_local_offline_time() {
        let mut machine = PathMachine::new(0);
        machine.start(1, 1);
        assert_eq!(machine.failed(1, 2), Some(PathAttempt::PeerToPeer));
        assert_eq!(machine.failed(1, 3), Some(PathAttempt::Relay));
        assert_eq!(machine.failed(1, 4), None);
        assert_eq!(machine.status(), PathStatus::Offline { at_ms: 4 });
    }
}

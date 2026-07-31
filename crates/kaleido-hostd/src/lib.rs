//! The composition root: the one place that knows both a provider adapter and
//! the canonical store.
//!
//! Everything provider-specific stays behind `kaleido-adapter-codex`, and
//! everything canonical stays behind `kaleido-state`. This crate wires the two
//! together and exposes a diagnostic client so a slice can be observed without
//! a phone.

pub mod content;
pub mod error;
pub mod slice;

pub use content::StoreContentAccess;
pub use error::HostdError;
pub use slice::{
    replay, run, show, ApprovalDecision, ReplayOutcome, ReplayRequest, RunOutcome, RunRequest,
    REPLAY_BASE_AT_MS,
};

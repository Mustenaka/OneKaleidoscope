//! Canonical state, its durable log, and the read models built from it.
//!
//! This crate owns the middle of the data path fixed by `docs/PROTOCOL.md`
//! section 0:
//!
//! ```text
//! provider decoder → reducer → StateEffect → durable log → projection
//! ```
//!
//! It knows nothing about any provider. A [`effect::StateEffect`] arrives, the
//! contract validates it, canonical state transitions, a cursor is assigned and
//! the record is appended. Because [`state::CanonicalState::apply`] reads no
//! clock and no external input, replaying the same log reproduces the same
//! state field for field, which is the convergence criterion section 5.4 sets
//! (as opposed to byte-for-byte equality, which it explicitly rejects).
//!
//! [`effect::StateEffect`]: kaleido_proto::effect::StateEffect

mod command_outbox;
pub mod content;
pub mod error;
pub mod log;
mod platform;
pub mod projection;
pub mod projection_journal;
pub mod state;
pub mod store;

pub use command_outbox::{DeviceCommandAdmission, DispatchClaim, DispatchTicket, PendingDispatch};
pub use content::ContentStore;
pub use error::StateError;
pub use log::StreamLog;
pub use projection::{DiagnosticProjectionEnvelope, ProjectionName};
pub use projection_journal::{ProjectionJournal, ProjectionReplay};
pub use state::CanonicalState;
pub use store::{CanonicalStore, ClockSource, StateCommit};

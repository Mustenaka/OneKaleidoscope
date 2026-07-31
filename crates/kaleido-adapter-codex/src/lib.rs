//! Codex app-server decoding and reduction.
//!
//! ADR-0012 settled how this crate reads upstream traffic: no generated client
//! and no hand-written upstream types, but an explicit table of pinned JSON
//! Pointers guarded against the committed schema snapshot. `surface.rs` is that
//! table; `decode.rs` is the only way to read through it; `reduce.rs` turns the
//! results into canonical state transitions.
//!
//! Untyped JSON does not leave this crate. A caller supplies bytes or a
//! recorded transcript and receives [`kaleido_proto::effect::StateEffect`]
//! values, so no downstream component can start depending on an upstream shape.

pub mod decode;
pub mod error;
pub mod reduce;
pub mod runtime;
pub mod surface;
pub mod transcript;

mod bindings;
mod platform;
mod process;

pub use error::CodexAdapterError;
pub use reduce::{CodexReducer, ReducerConfig};
pub use runtime::{CodexRuntimeConfig, CodexRuntimeSession, CodexSandboxMode};
pub use surface::{PinnedPath, SurfacePurpose, PINNED_PATHS};
pub use transcript::{parse_transcript, Direction, Transcript, TranscriptFrame};

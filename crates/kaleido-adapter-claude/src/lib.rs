//! Claude Agent SDK broker adapter.
//!
//! The official TypeScript SDK owns Claude's upstream message types.  This
//! crate owns only the closed sidecar envelope, transport lifecycle and
//! canonical reducer.

mod process;

pub mod error;
pub mod reduce;
pub mod runtime;
pub mod transcript;

pub use error::ClaudeAdapterError;
pub use reduce::{ClaudeReducer, DiscoveredSession, ReducerConfig};
pub use runtime::{ClaudeRuntimeConfig, ClaudeRuntimeSession};
pub use transcript::{parse_transcript, Direction, Transcript, TranscriptFrame};

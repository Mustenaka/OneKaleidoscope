//! OpenCode server adapter.
//!
//! OpenCode is a shared HTTP server, so this adapter uses its REST session
//! endpoints for discovery/reconstruction and its structured `/event` SSE
//! endpoint for live observation.  It never starts a TUI or parses terminal
//! output.  Generated upstream types are private to [`wire`]; callers receive
//! canonical effects or explicit protocol errors.

mod bindings;
mod client;
mod error;
pub mod normalization;
mod reduce;
mod runtime;
mod wire;

pub use client::{OpenCodeClient, OpenCodeClientConfig, PromptAdmission, PromptDelivery, SseEvent};
pub use error::{OpenCodeAdapterError, OpenCodeDecodeError};
pub use reduce::{CanonicalEvent, OpenCodeReducer, ReducerConfig};
pub use runtime::{OpenCodeRuntimeConfig, OpenCodeRuntimeSession, ReconnectOutcome};

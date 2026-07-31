//! Provider-neutral runtime traits shared by every concrete adapter.
//!
//! Nothing in this crate may name or assume a specific provider. A concrete
//! adapter (`kaleido-adapter-*`) decodes its own upstream wire format and hands
//! back [`kaleido_proto::effect::StateEffect`] values; the composition root
//! joins those to a store. The split exists so the canonical model never learns
//! an upstream shape (`docs/ARCHITECTURE.md` section 9, ADR-0012 D-1).

pub mod capability;
pub mod content;
pub mod identity;
pub mod session;

pub use capability::CapabilityProbe;
pub use content::{ContentAccess, ContentAccessError};
pub use identity::IdentityMint;
pub use session::{ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest};

//! The content-addressed store as an adapter sees it.
//!
//! Rule R-P5 keeps message bodies, tool arguments, diffs and filesystem paths
//! out of canonical state; only a [`ContentRef`] travels. An adapter therefore
//! never returns a body, it stores one and forwards the reference. The store
//! itself lives in `kaleido-state`, which does not know this trait exists; the
//! composition root bridges the two.

use kaleido_proto::content::{ContentKind, ContentRef, Sensitivity};
use kaleido_proto::ids::ContentId;
use kaleido_proto::ContractViolation;
use thiserror::Error;

/// Read and write access to bodies referenced by canonical state.
pub trait ContentAccess {
    /// Stores a body and returns the reference canonical state will carry.
    fn store(
        &mut self,
        kind: ContentKind,
        sensitivity: Sensitivity,
        bytes: &[u8],
    ) -> Result<ContentRef, ContentAccessError>;

    /// Reads a body back for delivery to a runtime or a reader.
    fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, ContentAccessError>;
}

#[derive(Debug, Error)]
pub enum ContentAccessError {
    #[error("content body storage failed: {detail}")]
    Storage { detail: String },

    #[error("content {content_id} has no stored body")]
    Missing { content_id: ContentId },

    #[error("content {content_id} does not match its recorded digest")]
    DigestMismatch { content_id: ContentId },

    #[error("content reference is not contract-valid: {0}")]
    Contract(#[from] ContractViolation),
}

/// Stores UTF-8 text as a sensitive body.
///
/// Every provider-derived string this project handles is on the section 10
/// redaction list, so the helper deliberately offers no business-sensitivity
/// shortcut.
pub fn store_sensitive_text(
    access: &mut dyn ContentAccess,
    kind: ContentKind,
    text: &str,
) -> Result<ContentRef, ContentAccessError> {
    access.store(kind, Sensitivity::Sensitive, text.as_bytes())
}

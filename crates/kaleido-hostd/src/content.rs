//! Bridges the adapter's content trait to the store's content-addressed
//! directory.
//!
//! The two sides deliberately do not know each other: `kaleido-state` depends
//! only on the contract, and `kaleido-adapter` only declares what an adapter
//! needs. Joining them is this crate's job.

use kaleido_adapter::content::{ContentAccess, ContentAccessError};
use kaleido_proto::content::{ContentKind, ContentRef, Sensitivity};
use kaleido_state::{ContentStore, StateError};

/// Adapter-facing view of the store's body directory.
#[derive(Debug, Clone)]
pub struct StoreContentAccess {
    store: ContentStore,
}

impl StoreContentAccess {
    pub fn new(store: ContentStore) -> Self {
        Self { store }
    }
}

impl ContentAccess for StoreContentAccess {
    fn store(
        &mut self,
        kind: ContentKind,
        sensitivity: Sensitivity,
        bytes: &[u8],
    ) -> Result<ContentRef, ContentAccessError> {
        self.store
            .store(kind, sensitivity, bytes)
            .map_err(translate)
    }

    fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, ContentAccessError> {
        self.store.load(reference).map_err(translate)
    }
}

fn translate(error: StateError) -> ContentAccessError {
    match error {
        StateError::ContentMissing { content_id } => ContentAccessError::Missing { content_id },
        StateError::ContentDigestMismatch { content_id } => {
            ContentAccessError::DigestMismatch { content_id }
        }
        StateError::Contract(violation) => ContentAccessError::Contract(violation),
        other => ContentAccessError::Storage {
            detail: other.to_string(),
        },
    }
}

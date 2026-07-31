//! Deterministic minting of broker-assigned canonical identifiers.
//!
//! Two properties matter here. Identifiers must be **deterministic**, so that
//! replaying the same upstream traffic twice converges to the same canonical
//! state (`docs/PROTOCOL.md` section 5.4). And they must not embed the upstream
//! identifier they were derived from, because section 10 keeps raw provider
//! identifiers out of the durable log, projections and pushes. A digest gives
//! both: stable for one input, and not reversible into the input.

use sha2::{Digest, Sha256};

use kaleido_proto::ids::{
    AttentionId, CommandId, ContentId, ItemId, ProjectBindingId, ProjectId, ProviderBindingHandle,
    ProviderBindingId, ProviderBindingKind, ProviderRuntimeId, QueueEntryId, SessionId, TurnId,
};

/// Number of hexadecimal characters kept from each digest.
///
/// Sixty-four bits of a SHA-256 digest is far more than a single host's
/// identifier space needs, and it satisfies the eight-character minimum that
/// [`ProviderBindingHandle::validate`] enforces.
const TOKEN_LENGTH: usize = 16;

/// Mints canonical identifiers from provider-private seeds.
#[derive(Debug, Clone)]
pub struct IdentityMint {
    salt: String,
}

impl IdentityMint {
    /// Creates a mint whose identifiers are stable for one host installation.
    pub fn new(salt: impl Into<String>) -> Self {
        Self { salt: salt.into() }
    }

    fn token(&self, namespace: &str, seed: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.salt.as_bytes());
        hasher.update([0]);
        hasher.update(namespace.as_bytes());
        hasher.update([0]);
        hasher.update(seed.as_bytes());
        let digest = hasher.finalize();
        let mut token = String::with_capacity(TOKEN_LENGTH);
        for byte in digest.iter().take(TOKEN_LENGTH / 2) {
            token.push_str(&format!("{byte:02x}"));
        }
        token
    }

    fn identifier(&self, prefix: &str, namespace: &str, seed: &str) -> String {
        format!("{prefix}_{}", self.token(namespace, seed))
    }

    pub fn host_id(&self, seed: &str) -> kaleido_proto::ids::HostId {
        kaleido_proto::ids::HostId::new(self.identifier("hst", "host", seed))
    }

    pub fn runtime_id(&self, seed: &str) -> ProviderRuntimeId {
        ProviderRuntimeId::new(self.identifier("rtm", "runtime", seed))
    }

    pub fn project_id(&self, seed: &str) -> ProjectId {
        ProjectId::new(self.identifier("prj", "project", seed))
    }

    pub fn project_binding_id(&self, seed: &str) -> ProjectBindingId {
        ProjectBindingId::new(self.identifier("pbd", "project-binding", seed))
    }

    pub fn session_id(&self, seed: &str) -> SessionId {
        SessionId::new(self.identifier("ses", "session", seed))
    }

    pub fn turn_id(&self, seed: &str) -> TurnId {
        TurnId::new(self.identifier("trn", "turn", seed))
    }

    pub fn item_id(&self, seed: &str) -> ItemId {
        ItemId::new(self.identifier("itm", "item", seed))
    }

    pub fn attention_id(&self, seed: &str) -> AttentionId {
        AttentionId::new(self.identifier("atn", "attention", seed))
    }

    pub fn queue_entry_id(&self, seed: &str) -> QueueEntryId {
        QueueEntryId::new(self.identifier("que", "queue-entry", seed))
    }

    pub fn command_id(&self, seed: &str) -> CommandId {
        CommandId::new(self.identifier("cmd", "command", seed))
    }

    pub fn content_id(&self, seed: &str) -> ContentId {
        ContentId::new(self.identifier("cnt", "content", seed))
    }

    /// A stable, human-opaque key for correlating an interactive request across
    /// reconnects (`docs/PROTOCOL.md` section 4.7).
    pub fn request_key(&self, seed: &str) -> String {
        self.identifier("req", "request-key", seed)
    }

    /// Mints the broker handle that stands in for a provider-private
    /// identifier. The raw identifier stays in the adapter's own binding store.
    pub fn binding_handle(
        &self,
        runtime_id: &ProviderRuntimeId,
        kind: ProviderBindingKind,
        seed: &str,
    ) -> ProviderBindingHandle {
        let namespace = match kind {
            ProviderBindingKind::Session => "binding-session",
            ProviderBindingKind::Turn => "binding-turn",
            ProviderBindingKind::Item => "binding-item",
            ProviderBindingKind::InteractionRequest => "binding-interaction",
            ProviderBindingKind::RuntimeAcknowledgement => "binding-acknowledgement",
        };
        ProviderBindingHandle {
            id: ProviderBindingId::new(self.identifier("bnd", namespace, seed)),
            runtime_id: runtime_id.clone(),
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_deterministic_and_namespaced() {
        let mint = IdentityMint::new("host-salt");
        assert_eq!(mint.session_id("thread-a"), mint.session_id("thread-a"));
        assert_ne!(mint.session_id("thread-a"), mint.session_id("thread-b"));
        // Same seed, different namespace: the tokens must not collide, or a
        // turn identifier could be mistaken for its session identifier.
        assert_ne!(
            mint.session_id("shared").value,
            mint.turn_id("shared").value.replace("trn_", "ses_")
        );
    }

    #[test]
    fn a_minted_identifier_never_contains_its_seed() {
        let mint = IdentityMint::new("host-salt");
        let seed = "019fb0d8-5af0-7d22-a53f-daf0d7c4c510";
        assert!(!mint.session_id(seed).value.contains(seed));
        assert!(!mint.item_id(seed).value.contains(seed));
    }

    #[test]
    fn minted_binding_handles_satisfy_the_contract() {
        let mint = IdentityMint::new("host-salt");
        let runtime_id = mint.runtime_id("codex-app-server");
        let handle = mint.binding_handle(&runtime_id, ProviderBindingKind::Session, "thread-a");
        assert!(handle.validate_for(ProviderBindingKind::Session).is_ok());
        assert!(handle.validate_for(ProviderBindingKind::Turn).is_err());
    }
}

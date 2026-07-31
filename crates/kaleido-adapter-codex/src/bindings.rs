//! The adapter-private binding store.
//!
//! Section 3 keeps raw provider identifiers out of canonical state, the durable
//! log, projections and pushes. They live only here, mapped to the broker
//! handles that stand in for them. Nothing in this module is re-exported.

use std::collections::BTreeMap;

use kaleido_adapter::IdentityMint;
use kaleido_proto::ids::{
    AttentionId, ItemId, ProviderBindingHandle, ProviderBindingKind, ProviderRuntimeId, SessionId,
    TurnId,
};

/// What a raw upstream item identifier resolves to.
#[derive(Debug, Clone)]
pub(crate) struct ItemBinding {
    pub item_id: ItemId,
    pub handle: ProviderBindingHandle,
    pub session_id: SessionId,
    pub turn_id: TurnId,
}

#[derive(Debug, Default)]
pub(crate) struct BindingStore {
    sessions: BTreeMap<String, (SessionId, ProviderBindingHandle)>,
    turns: BTreeMap<String, (TurnId, ProviderBindingHandle, SessionId)>,
    items: BTreeMap<String, ItemBinding>,
    interactions: BTreeMap<String, (AttentionId, ProviderBindingHandle)>,
}

impl BindingStore {
    pub fn bind_session(
        &mut self,
        mint: &IdentityMint,
        runtime_id: &ProviderRuntimeId,
        raw: &str,
    ) -> (SessionId, ProviderBindingHandle) {
        self.sessions
            .entry(raw.to_owned())
            .or_insert_with(|| {
                (
                    mint.session_id(raw),
                    mint.binding_handle(runtime_id, ProviderBindingKind::Session, raw),
                )
            })
            .clone()
    }

    pub fn session(&self, raw: &str) -> Option<&(SessionId, ProviderBindingHandle)> {
        self.sessions.get(raw)
    }

    pub fn bind_turn(
        &mut self,
        mint: &IdentityMint,
        runtime_id: &ProviderRuntimeId,
        raw: &str,
        session_id: &SessionId,
    ) -> (TurnId, ProviderBindingHandle, SessionId) {
        self.turns
            .entry(raw.to_owned())
            .or_insert_with(|| {
                (
                    mint.turn_id(raw),
                    mint.binding_handle(runtime_id, ProviderBindingKind::Turn, raw),
                    session_id.clone(),
                )
            })
            .clone()
    }

    pub fn bind_item(
        &mut self,
        mint: &IdentityMint,
        runtime_id: &ProviderRuntimeId,
        raw: &str,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> ItemBinding {
        self.items
            .entry(raw.to_owned())
            .or_insert_with(|| ItemBinding {
                item_id: mint.item_id(raw),
                handle: mint.binding_handle(runtime_id, ProviderBindingKind::Item, raw),
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            })
            .clone()
    }

    pub fn item(&self, raw: &str) -> Option<&ItemBinding> {
        self.items.get(raw)
    }

    pub fn bind_interaction(
        &mut self,
        mint: &IdentityMint,
        runtime_id: &ProviderRuntimeId,
        raw: &str,
    ) -> (AttentionId, ProviderBindingHandle) {
        self.interactions
            .entry(raw.to_owned())
            .or_insert_with(|| {
                (
                    mint.attention_id(raw),
                    mint.binding_handle(runtime_id, ProviderBindingKind::InteractionRequest, raw),
                )
            })
            .clone()
    }
}

//! Provider-private OpenCode identifier bindings.

use std::collections::BTreeMap;

use kaleido_adapter::IdentityMint;
use kaleido_proto::ids::{
    AttentionId, ItemId, ProviderBindingHandle, ProviderBindingKind, ProviderRuntimeId, SessionId,
    TurnId,
};

#[derive(Debug, Clone)]
pub(crate) struct ItemBinding {
    pub item_id: ItemId,
    pub handle: ProviderBindingHandle,
}

#[derive(Debug, Default)]
pub(crate) struct BindingStore {
    sessions: BTreeMap<String, (SessionId, ProviderBindingHandle)>,
    turns: BTreeMap<String, (TurnId, ProviderBindingHandle, SessionId)>,
    items: BTreeMap<String, ItemBinding>,
    interactions: BTreeMap<String, (AttentionId, ProviderBindingHandle)>,
}

impl BindingStore {
    pub fn session(
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

    pub fn turn(
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

    pub fn item(
        &mut self,
        mint: &IdentityMint,
        runtime_id: &ProviderRuntimeId,
        raw: &str,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> ItemBinding {
        self.items
            .entry(raw.to_owned())
            .or_insert_with(|| ItemBinding {
                item_id: mint.item_id(raw),
                handle: mint.binding_handle(runtime_id, ProviderBindingKind::Item, raw),
            })
            .clone()
    }

    pub fn interaction(
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

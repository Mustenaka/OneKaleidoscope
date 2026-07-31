//! Reading values through the pinned surface.
//!
//! Every read goes through [`Reader`], which resolves a
//! [`SurfacePurpose`] to its declared pointer and nothing else. There is no
//! function here that takes a caller-supplied pointer, which is what makes
//! `surface::PINNED_PATHS` the complete decode surface rather than a list of
//! examples.
//!
//! A read also records that the purpose was exercised. The drift test uses
//! that record to prove the table has no entry the decoder never consults
//! (ADR-0012 D-2, third assertion).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::error::CodexAdapterError;
use crate::surface::{self, Requirement, SurfacePurpose};

/// Resolves pinned paths against one payload.
#[derive(Debug)]
pub(crate) struct Reader<'usage> {
    exercised: &'usage mut BTreeSet<SurfacePurpose>,
}

impl<'usage> Reader<'usage> {
    pub fn new(exercised: &'usage mut BTreeSet<SurfacePurpose>) -> Self {
        Self { exercised }
    }

    fn resolve<'value>(
        &mut self,
        value: &'value Value,
        purpose: SurfacePurpose,
    ) -> Result<Option<&'value Value>, CodexAdapterError> {
        self.exercised.insert(purpose);
        let Some(path) = surface::path(purpose) else {
            // Unreachable through the public surface: every purpose is a table
            // entry, and the drift test proves it.
            return Err(CodexAdapterError::UnknownBinding {
                scope: "pinned path",
            });
        };
        let found = value.pointer(path.pointer).filter(|found| !found.is_null());
        match (found, path.requirement) {
            (Some(found), _) => Ok(Some(found)),
            (None, Requirement::Optional) => Ok(None),
            (None, Requirement::Required) => Err(CodexAdapterError::PointerUnresolved {
                purpose,
                pointer: path.pointer,
            }),
        }
    }

    fn mismatch(purpose: SurfacePurpose) -> CodexAdapterError {
        let pointer = surface::path(purpose).map_or("<unregistered>", |path| path.pointer);
        CodexAdapterError::PointerTypeMismatch { purpose, pointer }
    }

    pub fn string(
        &mut self,
        value: &Value,
        purpose: SurfacePurpose,
    ) -> Result<String, CodexAdapterError> {
        self.optional_string(value, purpose)?
            .ok_or(CodexAdapterError::PointerUnresolved {
                purpose,
                pointer: surface::path(purpose).map_or("<unregistered>", |path| path.pointer),
            })
    }

    pub fn optional_string(
        &mut self,
        value: &Value,
        purpose: SurfacePurpose,
    ) -> Result<Option<String>, CodexAdapterError> {
        match self.resolve(value, purpose)? {
            None => Ok(None),
            Some(found) => found
                .as_str()
                .map(|text| Some(text.to_owned()))
                .ok_or_else(|| Self::mismatch(purpose)),
        }
    }

    pub fn integer(
        &mut self,
        value: &Value,
        purpose: SurfacePurpose,
    ) -> Result<i64, CodexAdapterError> {
        self.optional_integer(value, purpose)?
            .ok_or(CodexAdapterError::PointerUnresolved {
                purpose,
                pointer: surface::path(purpose).map_or("<unregistered>", |path| path.pointer),
            })
    }

    pub fn optional_integer(
        &mut self,
        value: &Value,
        purpose: SurfacePurpose,
    ) -> Result<Option<i64>, CodexAdapterError> {
        match self.resolve(value, purpose)? {
            None => Ok(None),
            Some(found) => found
                .as_i64()
                .map(Some)
                .ok_or_else(|| Self::mismatch(purpose)),
        }
    }

    pub fn array(
        &mut self,
        value: &Value,
        purpose: SurfacePurpose,
    ) -> Result<Vec<Value>, CodexAdapterError> {
        match self.resolve(value, purpose)? {
            None => Ok(Vec::new()),
            Some(found) => found
                .as_array()
                .map(|items| items.to_vec())
                .ok_or_else(|| Self::mismatch(purpose)),
        }
    }

    /// Joins the string elements of a pinned array field.
    pub fn joined_strings(
        &mut self,
        value: &Value,
        purpose: SurfacePurpose,
        separator: &str,
    ) -> Result<String, CodexAdapterError> {
        let items = self.array(value, purpose)?;
        let mut parts = Vec::with_capacity(items.len());
        for item in &items {
            let text = item.as_str().ok_or_else(|| Self::mismatch(purpose))?;
            parts.push(text.to_owned());
        }
        Ok(parts.join(separator))
    }

    /// Whether the pinned field is present and non-null.
    pub fn is_present(
        &mut self,
        value: &Value,
        purpose: SurfacePurpose,
    ) -> Result<bool, CodexAdapterError> {
        match surface::path(purpose) {
            // Presence probing must not turn a missing required field into an
            // error, because the caller is asking exactly that question.
            Some(path) => {
                self.exercised.insert(purpose);
                Ok(value
                    .pointer(path.pointer)
                    .is_some_and(|found| !found.is_null()))
            }
            None => Ok(false),
        }
    }
}

/// The decision vocabulary the committed approval schema declares.
///
/// Section 4.7 forbids a client inventing an allow/deny pair, and an approval
/// request carries no options of its own, so the vocabulary is pinned here and
/// the drift test asserts it still matches the schema snapshot exactly.
pub const APPROVAL_DECISIONS: &[&str] = &["accept", "acceptForSession", "decline", "cancel"];

/// The upstream item kinds this slice decodes.
///
/// Anything else becomes an `UnknownUpstreamLabel` diagnostic rather than a
/// guess at a neighbouring shape (section 4.5).
pub const DECODED_ITEM_KINDS: &[&str] = &["userMessage", "agentMessage", "reasoning", "fileChange"];

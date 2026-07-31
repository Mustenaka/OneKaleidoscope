//! Content references. See `docs/PROTOCOL.md` section 4.9 and section 10.

use serde::{Deserialize, Serialize};

use crate::ids::ContentId;
use crate::ContractViolation;

/// Largest body that may travel inline with its metadata.
pub const MAX_INLINE_BYTES: u64 = 4096;

/// Longest preview a projection may carry.
pub const MAX_PREVIEW_BYTES: usize = 256;

/// Largest content body chunk a reader may request at once.
pub const MAX_CONTENT_READ_BYTES: u32 = 65_536;

/// A reference to a payload body, never the body itself.
///
/// Message text, reasoning, tool arguments, tool output, diffs and filesystem
/// paths all enter the canonical model through this type. Rule R-P5 forbids
/// inlining those bodies into canonical state, the durable log or push wakeups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContentRef {
    pub content_id: ContentId,
    pub kind: ContentKind,
    pub byte_len: u64,
    /// Integrity digest, formatted as `sha256:<lowercase hex>`.
    pub digest: String,
    pub preview: Option<String>,
    pub sensitivity: Sensitivity,
    pub availability: ContentAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    PlainText,
    Markdown,
    ToolArguments,
    ToolOutput,
    UnifiedDiff,
    FilePath,
    StructuredSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Redactable business content: may carry a bounded preview.
    Business,
    /// Never previewed, never logged, never pushed.
    Sensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ContentAvailability {
    Inline,
    Stored,
    /// Retention policy removed the body. Readers must show this state, not an
    /// empty string.
    Evicted,
    NeverStored,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContentReadRequest {
    pub content_id: ContentId,
    pub offset: u64,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContentReadChunk {
    pub content_id: ContentId,
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub next_offset: Option<u64>,
    pub eof: bool,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentReadResponse {
    Chunk {
        chunk: ContentReadChunk,
    },
    Unavailable {
        content_id: ContentId,
        reason: ContentUnavailableReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ContentUnavailableReason {
    Evicted,
    NeverStored,
    NotFound,
    Unauthorized,
    DigestMismatch,
}

impl ContentRef {
    /// Whether a reader may render the body right now.
    pub fn body_is_retrievable(&self) -> bool {
        matches!(
            self.availability,
            ContentAvailability::Inline | ContentAvailability::Stored
        )
    }

    /// Whether the body may appear in ordinary logs, pushes or relay metadata.
    pub fn loggable(&self) -> bool {
        matches!(self.sensitivity, Sensitivity::Business)
    }

    /// Enforces the section 4.9 rules that a reducer must not be able to bypass.
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.content_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "content_id",
            });
        }
        validate_digest(&self.digest)?;
        if self.sensitivity == Sensitivity::Sensitive && self.preview.is_some() {
            return Err(ContractViolation::SensitivePreview);
        }
        if let Some(preview) = &self.preview {
            if preview.len() > MAX_PREVIEW_BYTES {
                return Err(ContractViolation::PreviewTooLong {
                    byte_len: preview.len(),
                });
            }
            let lowercase = preview.to_ascii_lowercase();
            if preview
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\'))
                || lowercase.contains("bearer")
                || lowercase.contains("sk-")
                || lowercase.contains("ghp_")
            {
                return Err(ContractViolation::UnsafePreview);
            }
        }
        if self.availability == ContentAvailability::Inline {
            if self.byte_len > MAX_INLINE_BYTES {
                return Err(ContractViolation::InlineTooLarge {
                    byte_len: self.byte_len,
                });
            }
            if self.sensitivity == Sensitivity::Sensitive {
                return Err(ContractViolation::SensitiveInline);
            }
        }
        Ok(())
    }

    /// Validates this reference and requires its body to remain non-previewable.
    pub fn ensure_sensitive(&self, field: &'static str) -> Result<(), ContractViolation> {
        if self.sensitivity != Sensitivity::Sensitive {
            return Err(ContractViolation::SensitiveContentRequired { field });
        }
        self.validate()
    }
}

impl ContentReadRequest {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.content_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "content_id",
            });
        }
        if !(1..=MAX_CONTENT_READ_BYTES).contains(&self.max_bytes) {
            return Err(ContractViolation::InvalidContentReadSize {
                max_bytes: self.max_bytes,
            });
        }
        Ok(())
    }
}

impl ContentReadChunk {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.content_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "content_id",
            });
        }
        if self.bytes.len() > MAX_CONTENT_READ_BYTES as usize {
            return Err(ContractViolation::ContentReadChunkTooLarge {
                byte_len: self.bytes.len(),
            });
        }
        let byte_len = u64::try_from(self.bytes.len())
            .map_err(|_| ContractViolation::ContentReadOffsetOverflow)?;
        let expected = self
            .offset
            .checked_add(byte_len)
            .ok_or(ContractViolation::ContentReadOffsetOverflow)?;
        if self.eof {
            if self.next_offset.is_some() {
                return Err(ContractViolation::ContentReadEofHasNext);
            }
        } else if self.next_offset != Some(expected) {
            return Err(ContractViolation::ContentReadOffsetMismatch {
                expected,
                found: self.next_offset,
            });
        }
        validate_digest(&self.digest)
    }
}

impl ContentReadResponse {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        match self {
            ContentReadResponse::Chunk { chunk } => chunk.validate(),
            ContentReadResponse::Unavailable { content_id, .. } => {
                if content_id.is_empty() {
                    Err(ContractViolation::EmptyIdentifier {
                        field: "content_id",
                    })
                } else {
                    Ok(())
                }
            }
        }
    }
}

fn validate_digest(digest: &str) -> Result<(), ContractViolation> {
    let valid = digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(ContractViolation::MalformedDigest {
            digest: digest.to_owned(),
        })
    }
}

//! OpenAPI 3.1 validation before protocol-derived schema extraction.
//!
//! The committed document is the only upstream source.  This module does not
//! infer provider semantics. The pinned snapshot currently needs no rewrite:
//! numeric `exclusiveMinimum` has the same strict-bound spelling in JSON
//! Schema 2020-12 and draft-07. The report remains available so a future
//! mechanically necessary rule cannot become invisible.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleHit {
    pub name: &'static str,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NormalizationReport {
    pub rules: Vec<RuleHit>,
}

impl NormalizationReport {
    #[allow(dead_code)]
    pub fn count(&self, name: &str) -> usize {
        self.rules
            .iter()
            .find(|rule| rule.name == name)
            .map_or(0, |rule| rule.count)
    }
}

#[derive(Debug, Error)]
pub enum NormalizationError {
    #[error("OpenAPI document is not an object")]
    NotObject,
    #[error("OpenAPI document has unsupported version {0:?}")]
    UnsupportedVersion(String),
}

/// Normalize one OpenAPI 3.1 document without deleting or weakening fields.
pub fn normalize_openapi_document(
    document: Value,
) -> Result<(Value, NormalizationReport), NormalizationError> {
    let object = document.as_object().ok_or(NormalizationError::NotObject)?;
    let version = object
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or_else(|| NormalizationError::UnsupportedVersion(String::new()))?;
    if !version.starts_with("3.1.") {
        return Err(NormalizationError::UnsupportedVersion(version.to_owned()));
    }

    Ok((document, NormalizationReport::default()))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numeric_exclusive_minimum_is_preserved_for_draft_07() {
        let (after, report) = normalize_openapi_document(json!({
            "openapi": "3.1.0",
            "components": {"schemas": {"Value": {
                "type": "number", "exclusiveMinimum": 0
            }}}
        }))
        .expect("valid document");
        let schema = after
            .pointer("/components/schemas/Value")
            .expect("normalized schema exists");
        assert_eq!(schema.get("minimum"), None);
        assert_eq!(schema.get("exclusiveMinimum"), Some(&json!(0)));
        assert!(report.rules.is_empty());
    }

    #[test]
    fn pinned_snapshot_exercises_every_registered_rule() {
        let source = serde_json::from_str(include_str!("../../../schemas/opencode/openapi.json"))
            .expect("pinned snapshot is JSON");
        let (_, report) = normalize_openapi_document(source).expect("snapshot normalizes");
        assert!(report.rules.is_empty());
    }

    #[test]
    fn unsupported_document_version_is_rejected() {
        let error = normalize_openapi_document(json!({"openapi": "3.0.3"}));
        assert!(matches!(
            error,
            Err(NormalizationError::UnsupportedVersion(version)) if version == "3.0.3"
        ));
    }
}

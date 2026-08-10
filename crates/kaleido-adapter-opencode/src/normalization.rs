//! Mechanical OpenAPI 3.1 -> JSON Schema draft-07 normalization.
//!
//! The committed document is the only upstream source.  This module does not
//! infer provider semantics: it only rewrites constructs present in the
//! pinned snapshot whose meaning is expressed differently by JSON Schema
//! 2020-12 and the draft understood by the pinned generator.  The report is
//! printed by `build.rs` so a schema refresh records exactly which rules were
//! exercised; zero-hit rules are deliberately absent.

use serde_json::{Map, Value};
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
    fn hit(&mut self, name: &'static str) {
        if let Some(rule) = self.rules.iter_mut().find(|rule| rule.name == name) {
            rule.count = rule.count.saturating_add(1);
        } else {
            self.rules.push(RuleHit { name, count: 1 });
        }
    }

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
    mut document: Value,
) -> Result<(Value, NormalizationReport), NormalizationError> {
    let object = document
        .as_object_mut()
        .ok_or(NormalizationError::NotObject)?;
    let version = object
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or_else(|| NormalizationError::UnsupportedVersion(String::new()))?;
    if !version.starts_with("3.1.") {
        return Err(NormalizationError::UnsupportedVersion(version.to_owned()));
    }

    let mut report = NormalizationReport::default();
    normalize_node(&mut document, &mut report);
    Ok((document, report))
}

fn normalize_node(value: &mut Value, report: &mut NormalizationReport) {
    match value {
        Value::Array(values) => values
            .iter_mut()
            .for_each(|child| normalize_node(child, report)),
        Value::Object(object) => normalize_object(object, report),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn normalize_object(object: &mut Map<String, Value>, report: &mut NormalizationReport) {
    // JSON Schema 2020-12 changed exclusiveMinimum from a boolean to a numeric
    // annotation. Draft-07 spells the same constraint as a bound plus a
    // boolean marker. This is a direct mechanical representation.
    if let Some(exclusive) = object.get("exclusiveMinimum").cloned() {
        if exclusive.is_number() {
            object.insert("minimum".to_owned(), exclusive);
            object.insert("exclusiveMinimum".to_owned(), Value::Bool(true));
            report.hit("numeric_exclusive_minimum_to_bound");
        }
    }
    // Recurse after replacing nodes so nested schemas are covered.
    object
        .values_mut()
        .for_each(|child| normalize_node(child, report));
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numeric_exclusive_minimum_is_represented_without_widening() {
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
        assert_eq!(schema.get("minimum"), Some(&json!(0)));
        assert_eq!(schema.get("exclusiveMinimum"), Some(&json!(true)));
        assert_eq!(report.count("numeric_exclusive_minimum_to_bound"), 1);
    }

    #[test]
    fn pinned_snapshot_exercises_every_registered_rule() {
        let source = serde_json::from_str(include_str!("../../../schemas/opencode/openapi.json"))
            .expect("pinned snapshot is JSON");
        let (_, report) = normalize_openapi_document(source).expect("snapshot normalizes");
        assert_eq!(
            report.rules,
            vec![RuleHit {
                name: "numeric_exclusive_minimum_to_bound",
                count: 25,
            }]
        );
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

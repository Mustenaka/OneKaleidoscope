#[path = "src/normalization.rs"]
mod normalization;

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
};

use openapiv3::OpenAPI;
use proc_macro2::TokenStream;
use quote::quote;
use schemars::schema::RootSchema;
use serde_json::{json, Map, Value};
use typify::{TypeSpace, TypeSpaceSettings};

const ROOT_SCHEMAS: &[&str] = &[
    "Session",
    "Message",
    "Part",
    "EventPermissionAsked",
    "EventPermissionReplied",
    "EventPermissionV2Asked",
    "EventPermissionV2Replied",
    "EventQuestionAsked",
    "EventQuestionRejected",
    "EventQuestionReplied",
    "EventQuestionV2Asked",
    "EventQuestionV2Rejected",
    "EventQuestionV2Replied",
    "EventSessionCreated",
    "EventSessionUpdated",
    "EventSessionDeleted",
    "EventSessionStatus",
    "EventSessionIdle",
    "EventSessionDiff",
    "EventMessageUpdated",
    "EventMessagePartUpdated",
    "EventMessagePartDelta",
    "EventPluginAdded",
    "EventSessionNextPromptAdmitted",
    "EventServerConnected",
    "PermissionAction",
    "PermissionActionConfig",
    "PermissionAsked",
    "PermissionConfig",
    "PermissionNotFoundError",
    "PermissionObjectConfig",
    "PermissionReplied",
    "PermissionRequest",
    "PermissionRule",
    "PermissionRuleConfig",
    "PermissionRuleset",
    "PermissionSavedInfo",
    "PermissionV2Asked",
    "PermissionV2Effect",
    "PermissionV2Replied",
    "PermissionV2Reply",
    "PermissionV2Request",
    "PermissionV2Rule",
    "PermissionV2Ruleset",
    "PermissionV2Source",
    "PromptInput",
    "SessionInputAdmitted",
    "SessionStatus",
    "Project",
    "QuestionInfo",
    "QuestionAnswer",
    "QuestionV2Info",
    "QuestionV2Answer",
    "QuestionV2Reply",
];

const SYNTHETIC_OPERATIONS: &[(&str, &str, &str, &str)] = &[
    ("CreateSessionRequest", "/session", "post", "requestBody"),
    (
        "PromptRequest",
        "/session/{sessionID}/message",
        "post",
        "requestBody",
    ),
    (
        "PermissionReplyRequest",
        "/permission/{requestID}/reply",
        "post",
        "requestBody",
    ),
    (
        "SessionPermissionReplyRequest",
        "/session/{sessionID}/permissions/{permissionID}",
        "post",
        "requestBody",
    ),
    (
        "PermissionV2ReplyRequest",
        "/api/session/{sessionID}/permission/{requestID}/reply",
        "post",
        "requestBody",
    ),
    (
        "SessionPromptV2Request",
        "/api/session/{sessionID}/prompt",
        "post",
        "requestBody",
    ),
];

const SYNTHETIC_RESPONSES: &[(&str, &str, &str, &str)] = &[(
    "SessionPromptV2Response",
    "/api/session/{sessionID}/prompt",
    "post",
    "200",
)];

fn main() {
    println!("cargo:rerun-if-changed=../../schemas/opencode/openapi.json");
    println!("cargo:rerun-if-changed=src/normalization.rs");

    if let Err(error) = generate() {
        eprintln!("OpenCode schema generation failed: {error}");
        std::process::exit(1);
    }
}

fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let schema_path = manifest_dir.join("../../schemas/opencode/openapi.json");
    let source: Value = serde_json::from_slice(&fs::read(&schema_path)?)?;
    let (normalized, report) = normalization::normalize_openapi_document(source)?;

    // Parse the normalized document with an OpenAPI 3 parser as a structural
    // guard.  We still retain the source JSON for extraction because the
    // OpenAPI crate models 3.0 semantics and intentionally drops extensions.
    let _: OpenAPI = serde_json::from_value(normalized.clone())?;
    let subset = extract_subset(&normalized)?;

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let subset_path = out_dir.join("opencode-normalized.json");
    fs::write(&subset_path, serde_json::to_vec_pretty(&subset)?)?;

    let root_schema: RootSchema = serde_json::from_value(subset.clone())?;
    let mut settings = TypeSpaceSettings::default();
    settings.with_struct_builder(true);
    settings.with_map_type("std::collections::BTreeMap");
    let mut type_space = TypeSpace::new(&settings);
    type_space.add_root_schema(root_schema)?;

    let generated: TokenStream = quote! { #type_space };
    let generated = rustfmt_wrapper::rustfmt(generated)?;
    fs::write(out_dir.join("opencode_generated.rs"), generated)?;

    for rule in report.rules {
        println!(
            "cargo:warning=opencode normalization {} hit {}",
            rule.name, rule.count
        );
    }
    println!(
        "cargo:warning=opencode generated subset: {} schemas",
        subset
            .get("definitions")
            .and_then(Value::as_object)
            .map_or(0, Map::len)
    );
    Ok(())
}

fn extract_subset(document: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let components = document
        .get("components")
        .and_then(|value| value.get("schemas"))
        .and_then(Value::as_object)
        .ok_or("OpenAPI components.schemas is missing")?;
    let mut selected = BTreeSet::new();
    let mut queue = ROOT_SCHEMAS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let mut definitions = BTreeMap::new();
    let mut synthetic = BTreeMap::new();

    for (name, path, method, kind) in SYNTHETIC_OPERATIONS {
        let schema = document
            .get("paths")
            .and_then(|paths| paths.get(*path))
            .and_then(|item| item.get(*method))
            .and_then(|operation| operation.get(*kind))
            .and_then(|body| body.get("content"))
            .and_then(|content| content.get("application/json"))
            .and_then(|media| media.get("schema"))
            .cloned()
            .ok_or_else(|| format!("missing synthetic operation schema {name}"))?;
        synthetic.insert((*name).to_owned(), schema);
        queue.push((*name).to_owned());
    }

    for (name, path, method, status) in SYNTHETIC_RESPONSES {
        let schema = document
            .get("paths")
            .and_then(|paths| paths.get(*path))
            .and_then(|item| item.get(*method))
            .and_then(|operation| operation.get("responses"))
            .and_then(|responses| responses.get(*status))
            .and_then(|response| response.get("content"))
            .and_then(|content| content.get("application/json"))
            .and_then(|media| media.get("schema"))
            .cloned()
            .ok_or_else(|| format!("missing synthetic response schema {name}"))?;
        synthetic.insert((*name).to_owned(), schema);
        queue.push((*name).to_owned());
    }

    while let Some(name) = queue.pop() {
        if !selected.insert(name.clone()) {
            continue;
        }
        let schema = synthetic
            .get(&name)
            .or_else(|| components.get(&name))
            .ok_or_else(|| format!("required OpenCode schema {name} is absent"))?;
        let mut schema = schema.clone();
        if synthetic.contains_key(&name) {
            if let Value::Object(object) = &mut schema {
                object
                    .entry("title")
                    .or_insert_with(|| Value::String(name.clone()));
            }
        }
        collect_refs(&schema, &mut queue);
        definitions.insert(name, schema);
    }

    // RootSchema uses `definitions` rather than OpenAPI's components map.  No
    // fields from selected schemas are discarded; references retain their
    // original `#/components/schemas/...` spelling until this mechanical pass.
    let mut rewritten = Map::new();
    for (name, mut schema) in definitions {
        rewrite_refs(&mut schema);
        rewritten.insert(name, schema);
    }
    Ok(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "OpenCodeRequiredSurface",
        "type": "object",
        "definitions": rewritten,
    }))
}

fn collect_refs(value: &Value, queue: &mut Vec<String>) {
    match value {
        Value::Array(values) => values.iter().for_each(|value| collect_refs(value, queue)),
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                if let Some(name) = reference.strip_prefix("#/components/schemas/") {
                    queue.push(name.to_owned());
                }
            }
            object.values().for_each(|value| collect_refs(value, queue));
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn rewrite_refs(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(rewrite_refs),
        Value::Object(object) => {
            if let Some(reference) = object.get_mut("$ref") {
                let replacement = reference
                    .as_str()
                    .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
                    .map(|name| Value::String(format!("#/definitions/{name}")));
                if let Some(replacement) = replacement {
                    *reference = replacement;
                }
            }
            object.values_mut().for_each(rewrite_refs);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

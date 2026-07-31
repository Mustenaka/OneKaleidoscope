#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! The drift guard ADR-0012 D-2 requires.
//!
//! A pinned-path table is only worth having if something checks it, so these
//! tests resolve every declared pointer against the committed schema snapshot,
//! every declared surface identifier against `schemas/required-surface.toml`,
//! and every declared purpose against what the decoder actually consults while
//! replaying the committed recordings.

mod support;

use std::collections::BTreeSet;

use kaleido_adapter_codex::decode::APPROVAL_DECISIONS;
use kaleido_adapter_codex::surface::{Scope, SurfacePurpose};
use kaleido_adapter_codex::PINNED_PATHS;
use serde_json::Value;

use support::{load_transcript, reducer, repository_root, MemoryContent, FIXTURES};

fn schema_document(name: &str) -> Value {
    let path = repository_root().join("schemas").join("codex").join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("committed schema {name} must be readable: {error}"));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("{name} must be valid JSON: {error}"))
}

#[test]
fn every_pinned_pointer_resolves_in_the_committed_schema_snapshot() {
    for path in PINNED_PATHS {
        assert!(
            !path.anchors.is_empty(),
            "{:?} declares no schema anchor",
            path.purpose
        );
        for anchor in path.anchors {
            let document = schema_document(anchor.document);
            let resolved = document.pointer(anchor.pointer);
            assert!(
                resolved.is_some(),
                "{:?}: `{}` does not resolve in {}",
                path.purpose,
                anchor.pointer,
                anchor.document
            );
            if let Some(expected_title) = anchor.title {
                // A positional `oneOf` branch only means what we think it means
                // while it keeps its title, so an upstream reordering has to
                // fail here rather than silently address another shape.
                let owner = anchor
                    .pointer
                    .strip_suffix(
                        anchor
                            .pointer
                            .rsplit("/properties/")
                            .next()
                            .map(|leaf| format!("/properties/{leaf}"))
                            .unwrap_or_default()
                            .as_str(),
                    )
                    .unwrap_or(anchor.pointer);
                let title = document
                    .pointer(owner)
                    .and_then(|branch| branch.get("title"))
                    .and_then(Value::as_str);
                assert_eq!(
                    title,
                    Some(expected_title),
                    "{:?}: `{owner}` in {} no longer carries the expected title",
                    path.purpose,
                    anchor.document
                );
            }
        }
    }
}

#[test]
fn every_declared_surface_identifier_exists_in_the_required_surface() {
    let raw = std::fs::read_to_string(
        repository_root()
            .join("schemas")
            .join("required-surface.toml"),
    )
    .expect("required-surface.toml must be readable");
    let declared = raw
        .lines()
        .filter_map(|line| line.trim().strip_prefix("id = \""))
        .filter_map(|rest| rest.strip_suffix('"'))
        .collect::<BTreeSet<_>>();
    for path in PINNED_PATHS {
        assert!(
            !path.surface_entry_ids.is_empty(),
            "{:?} claims no required-surface entry",
            path.purpose
        );
        for identifier in path.surface_entry_ids {
            assert!(
                declared.contains(identifier),
                "{:?} claims `{identifier}`, which required-surface.toml does not declare",
                path.purpose
            );
        }
    }
}

#[test]
fn the_table_has_no_duplicate_and_no_unused_entry() {
    let mut seen = BTreeSet::new();
    for path in PINNED_PATHS {
        assert!(
            seen.insert(path.purpose),
            "{:?} is declared more than once",
            path.purpose
        );
    }

    // "Unused" means the decoder never consults it, so the check is what the
    // decoder actually did while reducing every committed recording.
    let mut exercised = BTreeSet::<SurfacePurpose>::new();
    for fixture in FIXTURES {
        let transcript = load_transcript(fixture);
        let mut reducer = reducer();
        let mut content = MemoryContent::default();
        reducer
            .ingest(&transcript, &mut content)
            .unwrap_or_else(|error| panic!("{fixture} must reduce cleanly: {error}"));
        exercised.extend(reducer.exercised_purposes().iter().copied());
    }
    let dead = PINNED_PATHS
        .iter()
        .filter(|path| !exercised.contains(&path.purpose))
        .map(|path| path.purpose)
        .collect::<Vec<_>>();
    assert!(
        dead.is_empty(),
        "the pinned table declares paths the decoder never reads: {dead:?}"
    );
}

#[test]
fn no_pinned_path_reads_the_turn_completion_item_summary() {
    // The recorded completion payload is a summary view whose item array holds
    // only the last message of a six-item turn. Making it unreachable is
    // stronger than remembering not to use it.
    for path in PINNED_PATHS {
        assert_ne!(
            path.pointer, "/params/turn/items",
            "{:?} would read the completion summary array",
            path.purpose
        );
        assert!(
            !path.pointer.starts_with("/params/turn/items/"),
            "{:?} would read inside the completion summary array",
            path.purpose
        );
    }
}

#[test]
fn the_pinned_decision_vocabulary_matches_the_committed_schema() {
    // Section 4.7 forbids a client inventing an allow/deny pair. The request
    // carries no options, so the vocabulary is pinned, and pinned means checked.
    let document = schema_document("FileChangeRequestApprovalResponse.json");
    let declared = document
        .pointer("/definitions/FileChangeApprovalDecision/oneOf")
        .and_then(Value::as_array)
        .expect("the approval decision enumeration must be present")
        .iter()
        .filter_map(|branch| branch.get("enum"))
        .filter_map(Value::as_array)
        .filter_map(|values| values.first())
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        declared, APPROVAL_DECISIONS,
        "the pinned decision vocabulary drifted from the committed schema"
    );
}

#[test]
fn element_scoped_pointers_are_relative_and_payload_pointers_are_absolute() {
    for path in PINNED_PATHS {
        match path.scope {
            Scope::Payload => assert!(
                path.pointer.starts_with("/params/") || path.pointer.starts_with("/result/"),
                "{:?}: a payload pointer must address the frame envelope",
                path.purpose
            ),
            Scope::Element => assert!(
                !path.pointer.starts_with("/params/") && !path.pointer.starts_with("/result/"),
                "{:?}: an element pointer must be relative to its element",
                path.purpose
            ),
        }
    }
}

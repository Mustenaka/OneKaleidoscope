#![allow(clippy::expect_used)]

use std::collections::BTreeSet;

use serde_json::{json, Value};
use xtask::deps::{
    check_with_inputs, parse_rules, DependencyCheckError, ManifestInput, Violation, ViolationRule,
};

const REPOSITORY_RULES: &str = include_str!("../../docs/dependency-rules.toml");

#[derive(Clone, Debug)]
struct TestPackage<'a> {
    name: &'a str,
    manifest: &'a str,
    dependencies: Vec<Value>,
}

impl<'a> TestPackage<'a> {
    fn new(name: &'a str, manifest: &'a str) -> Self {
        Self {
            name,
            manifest,
            dependencies: Vec::new(),
        }
    }

    fn with_dependencies(mut self, dependencies: Vec<Value>) -> Self {
        self.dependencies = dependencies;
        self
    }
}

fn dependency(name: &str) -> Value {
    json!({ "name": name })
}

fn metadata(packages: &[TestPackage<'_>]) -> String {
    let workspace_members = packages
        .iter()
        .map(|package| format!("test:{}", package.name))
        .collect::<Vec<_>>();
    let packages = packages
        .iter()
        .map(|package| {
            json!({
                "id": format!("test:{}", package.name),
                "name": package.name,
                "manifest_path": format!("C:/repo/{}", package.manifest),
                "dependencies": package.dependencies,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "workspace_root": "C:/repo",
        "workspace_members": workspace_members,
        "packages": packages,
    })
    .to_string()
}

fn violations(error: &DependencyCheckError) -> &[Violation] {
    error
        .violations()
        .expect("the check must fail with rule violations")
}

#[test]
fn repository_rules_declare_the_complete_eleven_crate_matrix() {
    let rules = parse_rules(REPOSITORY_RULES).expect("repository rules must parse");
    let expected = BTreeSet::from([
        "kaleido-proto",
        "kaleido-state",
        "kaleido-adapter",
        "kaleido-adapter-codex",
        "kaleido-adapter-acp",
        "kaleido-adapter-opencode",
        "kaleido-transport",
        "kaleido-core",
        "kaleido-hostd",
        "kaleido-cli",
        "kaleido-relay",
    ]);

    assert_eq!(
        rules
            .crates
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(
        rules
            .crates
            .get("kaleido-relay")
            .expect("relay rule must exist")
            .may_depend_on,
        Vec::<String>::new()
    );
    assert_eq!(
        rules
            .crates
            .get("kaleido-transport")
            .expect("transport rule must exist")
            .may_depend_on,
        ["kaleido-proto"]
    );
    assert_eq!(
        rules
            .crates
            .get("kaleido-core")
            .expect("core rule must exist")
            .may_depend_on,
        [
            "kaleido-proto",
            "kaleido-state",
            "kaleido-adapter",
            "kaleido-transport",
        ]
    );
    for (crate_name, rule) in &rules.crates {
        if rule
            .may_depend_on
            .iter()
            .any(|dependency| dependency.contains('*'))
        {
            assert_eq!(crate_name, "kaleido-hostd");
        }
    }
    assert_eq!(rules.exclusive_targets.len(), 1);
    let exclusive = rules
        .exclusive_targets
        .first()
        .expect("concrete adapter target must be exclusive");
    assert_eq!(exclusive.to, "kaleido-adapter-*");
    assert_eq!(exclusive.allowed_from, ["kaleido-hostd"]);
}

#[test]
fn declared_but_absent_crates_and_coverage_exempt_members_are_accepted() {
    let source = metadata(&[
        TestPackage::new("xtask", "xtask/Cargo.toml"),
        TestPackage::new("kaleido-recorder", "spikes/kaleido-recorder/Cargo.toml"),
    ]);

    let report =
        check_with_inputs(REPOSITORY_RULES, &source, &[]).expect("legal metadata must pass");

    assert_eq!(report.workspace_members, 2);
    assert_eq!(report.internal_edges, 0);
}

#[test]
fn unauthorized_internal_edge_reports_both_endpoints() {
    let source = metadata(&[
        TestPackage::new("kaleido-core", "crates/kaleido-core/Cargo.toml")
            .with_dependencies(vec![dependency("kaleido-cli")]),
        TestPackage::new("kaleido-cli", "crates/kaleido-cli/Cargo.toml"),
    ]);

    let error =
        check_with_inputs(REPOSITORY_RULES, &source, &[]).expect_err("illegal edge must fail");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "kaleido-core"
            && violation.to == "kaleido-cli"
            && violation.rule == ViolationRule::Matrix
    }));
    assert!(error.to_string().contains("kaleido-core -> kaleido-cli"));
}

#[test]
fn existing_undeclared_workspace_member_fails_coverage() {
    let source = metadata(&[TestPackage::new(
        "kaleido-unknown",
        "tools/kaleido-unknown/Cargo.toml",
    )]);

    let error =
        check_with_inputs(REPOSITORY_RULES, &source, &[]).expect_err("unknown member must fail");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "kaleido-unknown" && violation.rule == ViolationRule::Coverage
    }));
}

#[test]
fn coverage_exemption_does_not_exempt_internal_edges() {
    let source = metadata(&[
        TestPackage::new("xtask", "xtask/Cargo.toml")
            .with_dependencies(vec![dependency("kaleido-recorder")]),
        TestPackage::new("kaleido-recorder", "spikes/kaleido-recorder/Cargo.toml"),
    ]);

    let error = check_with_inputs(REPOSITORY_RULES, &source, &[])
        .expect_err("an exempt source must still have an empty internal allow-list");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "xtask"
            && violation.to == "kaleido-recorder"
            && violation.rule == ViolationRule::Matrix
    }));
    assert!(!violations(&error)
        .iter()
        .any(|violation| violation.rule == ViolationRule::Coverage));
}

#[test]
fn proto_external_deny_uses_the_original_package_name_after_rename() {
    let mut renamed_tokio = dependency("tokio");
    if let Some(object) = renamed_tokio.as_object_mut() {
        object.insert("rename".to_owned(), json!("runtime"));
    }
    let source = metadata(&[
        TestPackage::new("kaleido-proto", "crates/kaleido-proto/Cargo.toml")
            .with_dependencies(vec![renamed_tokio]),
    ]);

    let error =
        check_with_inputs(REPOSITORY_RULES, &source, &[]).expect_err("tokio must be denied");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "kaleido-proto"
            && violation.to == "tokio"
            && violation.rule == ViolationRule::ForbiddenDependency
    }));
}

#[test]
fn dependencies_of_every_cargo_kind_reach_the_proto_deny_check() {
    let source = metadata(&[
        TestPackage::new("kaleido-proto", "crates/kaleido-proto/Cargo.toml").with_dependencies(
            vec![
                json!({ "name": "tokio", "kind": null, "optional": false }),
                json!({ "name": "reqwest", "kind": "dev", "optional": false }),
                json!({ "name": "iroh", "kind": "build", "optional": false }),
                json!({
                    "name": "notify",
                    "kind": null,
                    "optional": true,
                    "target": "cfg(windows)"
                }),
            ],
        ),
    ]);

    let error =
        check_with_inputs(REPOSITORY_RULES, &source, &[]).expect_err("all kinds must be denied");
    let denied = violations(&error)
        .iter()
        .filter(|violation| violation.rule == ViolationRule::ForbiddenDependency)
        .map(|violation| violation.to.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        denied,
        BTreeSet::from(["iroh", "notify", "reqwest", "tokio"])
    );
}

#[test]
fn hostd_wildcard_allows_all_three_concrete_adapters() {
    let source = metadata(&[
        TestPackage::new("kaleido-hostd", "crates/kaleido-hostd/Cargo.toml").with_dependencies(
            vec![
                dependency("kaleido-adapter-codex"),
                dependency("kaleido-adapter-acp"),
                dependency("kaleido-adapter-opencode"),
            ],
        ),
        TestPackage::new(
            "kaleido-adapter-codex",
            "crates/kaleido-adapter-codex/Cargo.toml",
        ),
        TestPackage::new(
            "kaleido-adapter-acp",
            "crates/kaleido-adapter-acp/Cargo.toml",
        ),
        TestPackage::new(
            "kaleido-adapter-opencode",
            "crates/kaleido-adapter-opencode/Cargo.toml",
        ),
    ]);

    let report =
        check_with_inputs(REPOSITORY_RULES, &source, &[]).expect("hostd composition must pass");

    assert_eq!(report.internal_edges, 3);
}

#[test]
fn adapter_wildcard_does_not_match_the_shared_adapter_crate() {
    let rules = REPOSITORY_RULES.replace(
        "\"kaleido-adapter\",\n    \"kaleido-adapter-*\",",
        "\"kaleido-adapter-*\",",
    );
    let source = metadata(&[
        TestPackage::new("kaleido-hostd", "crates/kaleido-hostd/Cargo.toml")
            .with_dependencies(vec![dependency("kaleido-adapter")]),
        TestPackage::new("kaleido-adapter", "crates/kaleido-adapter/Cargo.toml"),
    ]);

    let error = check_with_inputs(&rules, &source, &[])
        .expect_err("the concrete adapter wildcard must not match the shared crate");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "kaleido-hostd"
            && violation.to == "kaleido-adapter"
            && violation.rule == ViolationRule::Matrix
    }));
}

#[test]
fn adapter_to_adapter_edge_reports_a_10() {
    let source = metadata(&[
        TestPackage::new(
            "kaleido-adapter-codex",
            "crates/kaleido-adapter-codex/Cargo.toml",
        )
        .with_dependencies(vec![dependency("kaleido-adapter-acp")]),
        TestPackage::new(
            "kaleido-adapter-acp",
            "crates/kaleido-adapter-acp/Cargo.toml",
        ),
    ]);

    let error =
        check_with_inputs(REPOSITORY_RULES, &source, &[]).expect_err("adapter edge must fail");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "kaleido-adapter-codex"
            && violation.to == "kaleido-adapter-acp"
            && violation.rule == ViolationRule::AdapterIsolation
    }));
}

#[test]
fn ui_to_adapter_edge_reports_ui_isolation() {
    let source = metadata(&[
        TestPackage::new("kaleido-cli", "crates/kaleido-cli/Cargo.toml")
            .with_dependencies(vec![dependency("kaleido-adapter-codex")]),
        TestPackage::new(
            "kaleido-adapter-codex",
            "crates/kaleido-adapter-codex/Cargo.toml",
        ),
    ]);

    let error = check_with_inputs(REPOSITORY_RULES, &source, &[]).expect_err("UI edge must fail");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "kaleido-cli"
            && violation.to == "kaleido-adapter-codex"
            && violation.rule == ViolationRule::UiIsolation
    }));
}

#[test]
fn arbitrarily_named_future_ui_cannot_bypass_the_exclusive_adapter_target() {
    let source = metadata(&[
        TestPackage::new(
            "future-mobile-surface",
            "mobile/future-mobile-surface/Cargo.toml",
        )
        .with_dependencies(vec![dependency("kaleido-adapter-opencode")]),
        TestPackage::new(
            "kaleido-adapter-opencode",
            "crates/kaleido-adapter-opencode/Cargo.toml",
        ),
    ]);

    let error = check_with_inputs(REPOSITORY_RULES, &source, &[])
        .expect_err("an arbitrary future UI name must not bypass adapter isolation");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "future-mobile-surface"
            && violation.to == "kaleido-adapter-opencode"
            && violation.rule == ViolationRule::UiIsolation
    }));
}

#[test]
fn crates_manifest_absent_from_workspace_metadata_fails() {
    let source = metadata(&[TestPackage::new("xtask", "xtask/Cargo.toml")]);
    let manifests = [ManifestInput::new(
        "crates/kaleido-core/Cargo.toml",
        "[package]\nname = \"kaleido-core\"\nversion = \"0.1.0\"\n",
    )];

    let error = check_with_inputs(REPOSITORY_RULES, &source, &manifests)
        .expect_err("an untracked crates manifest must fail");

    assert!(violations(&error).iter().any(|violation| {
        violation.from == "kaleido-core" && violation.rule == ViolationRule::WorkspaceMembership
    }));
}

#[test]
fn tracked_crates_manifest_is_accepted() {
    let source = metadata(&[TestPackage::new(
        "kaleido-core",
        "crates/kaleido-core/Cargo.toml",
    )]);
    let manifests = [ManifestInput::new(
        "crates/kaleido-core/Cargo.toml",
        "[package]\nname = \"kaleido-core\"\nversion = \"0.1.0\"\n",
    )];

    let report =
        check_with_inputs(REPOSITORY_RULES, &source, &manifests).expect("tracked crate must pass");

    assert_eq!(report.crate_manifests, 1);
}

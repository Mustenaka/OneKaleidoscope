# Development

## Rust toolchain

Run Rust commands from the repository root. `rust-toolchain.toml` selects Rust
1.94.0 with `rustfmt`, `clippy`, and the `aarch64-linux-android` target, so a
compatible `rustup` installation installs or selects the pinned toolchain
automatically.

Workspace members inherit the common package metadata, shared foundational
dependencies, and lint policy from the root `Cargo.toml`. Every new member must
opt in with workspace-inherited package fields and:

```toml
[lints]
workspace = true
```

## Local checks

The single local entry point for all repository gates is:

```text
cargo xtask ci
```

It stops at the first failure and runs, in this order:

1. `cargo fmt --all -- --check`
2. the architecture dependency guard
3. the repository antipattern scanner
4. `cargo clippy --all-targets -- -D warnings`
5. `cargo test --workspace`
6. fixture schema and leak verification

Each gate can also be run independently:

```text
cargo xtask fmt
cargo xtask check-deps
cargo xtask lint-forbidden
cargo xtask clippy
cargo xtask test
cargo xtask fixtures verify
```

`cargo xtask fmt` is a formatting check; it does not rewrite files. To format
code before running the gates, use `cargo fmt --all`.

The forbidden-pattern scanner checks Rust source files under `crates/`,
`spikes/`, and `xtask/`. It excludes root `schemas/`, `target/`, and every
`tests/fixtures/` subtree. Dependency and crate-role rules come only from
`docs/dependency-rules.toml`.

## Adding a crate

Every production crate belongs under `crates/` and must be represented in all
of these places in the same change:

1. the root workspace `members` list (the first M3 crate should add the
   `crates/*` member glob);
2. a `[crates."<package-name>"]` entry in
   `docs/dependency-rules.toml`;
3. that entry's complete `may_depend_on` allow-list and, where required,
   its direct dependency deny-list.

`cargo xtask check-deps` rejects a `crates/*/Cargo.toml` that Cargo does not
consider a workspace member, a workspace member without a rules entry, and
every workspace-internal edge outside the declared matrix. Rules for crates
that have not been created yet are valid and intentionally do not fail.
`spikes/*` and `xtask` are exempt only from having their own declaration; their
workspace-internal edges still default to an empty allow-list.
The `exclusive_targets` rule reserves concrete `kaleido-adapter-*` dependencies
for the `kaleido-hostd` composition root, so a newly named UI crate cannot
bypass the adapter boundary. If the new crate contains Rust UI code, also add
its exact package name to `antipatterns.a2.ui_crates`.

## A-2 agent-name branch exemptions

UI code must branch on `capabilities()`, not an adapter name. If an A-2 match is
a reviewed false positive, place this exact comment immediately before the
flagged comparison or `match` expression:

```rust
// #[allow(kaleido::agent_name_branch)] reason: compatibility diagnostics only
match adapter_name {
    "codex" => render_diagnostic(),
    _ => render_generic(),
}
```

The text after `reason:` must be non-empty. The scanner prints the number of
effective A-2 exemptions on every run so growth is visible in CI. This is the
only antipattern exemption: A-1 ANSI/terminal parsing, A-4 upstream
discriminators in UACP, and A-6 handwritten upstream types cannot be exempted.

## Test-only lint allowances

Production code must satisfy every workspace lint. Test code may allow only
`clippy::unwrap_used` and `clippy::expect_used`, and only at the top of a file
under `tests/` or inside a `#[cfg(test)]` module:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]
```

Do not place this allowance at a crate root, and do not allow panic or indexing
lints. If a production exception appears necessary, stop and report it instead
of weakening the workspace policy.

## Continuous integration

`.github/workflows/ci.yml` runs on Windows, macOS, and Ubuntu, with Windows
listed first as the primary platform. Each job checks out the repository,
installs the toolchain described by `rust-toolchain.toml`, restores the Rust
build cache, and runs `cargo xtask ci`. No matrix entry permits failure.

The repository currently has no configured remote, so the workflow has not run
on GitHub Actions. Windows is verified locally; macOS and Linux execution is
deferred until the repository is hosted.

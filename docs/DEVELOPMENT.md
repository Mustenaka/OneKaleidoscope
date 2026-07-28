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
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --workspace`
4. the repository forbidden-pattern scanner

Each gate can also be run independently:

```text
cargo xtask fmt
cargo xtask clippy
cargo xtask test
cargo xtask lint-forbidden
```

`cargo xtask fmt` is a formatting check; it does not rewrite files. To format
code before running the gates, use `cargo fmt --all`.

The forbidden-pattern scanner checks Rust source files under `crates/`,
`spikes/`, and `xtask/`. It excludes `tests/fixtures/`, `schemas/`, and
`target/`. A match is always an error; the scanner has no exemption mechanism.

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

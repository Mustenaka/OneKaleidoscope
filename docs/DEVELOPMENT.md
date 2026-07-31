# Development

> Current phase: R2 is complete (`docs/gates/T-100-result.md`). The active task is
> `docs/tasks/T-102.md`, which must land before R3. See `docs/STATUS.md` and
> `docs/tasks/README.md` before using the instructions below.

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

## R1 UniFFI binding probe

`kaleido-core` is a binding-only façade. Its exported probes use the actual
`kaleido-proto` command, error, state-effect and projection types; there is no
second DTO layer. UniFFI is pinned to `0.32.0`.

T-102 extends the original record round-trip with the mobile call shapes:

- `ProjectionProbeCallback` is implemented by Kotlin or Swift and called by
  Rust with `ProjectionEnvelope` and `CanonicalError`;
- `ProjectionSubscriptionProbe` owns that callback between `subscribe` and
  `unsubscribe`;
- `fallible_binding_probe` throws `BindingProbeError`, whose payload is the
  canonical `CanonicalError`;
- `async_binding_probe` asynchronously returns a canonical `CommandAck`.

These exports contain no session, projection, or storage implementation. They
only force Rust, UniFFI, Kotlin, and Swift to agree on the intended API shape.

Generate both language bindings after building the dynamic library:

```text
cargo build -p kaleido-core --lib
cargo run -p kaleido-core --bin uniffi-bindgen -- generate --language kotlin --out-dir target/uniffi/kotlin target/debug/kaleido_core.dll
cargo run -p kaleido-core --bin uniffi-bindgen -- generate --language swift --out-dir target/uniffi/swift target/debug/kaleido_core.dll
```

On non-Windows platforms, replace the final library path with that platform's
`cdylib` filename. Generated sources stay under ignored `target/`.

The Kotlin consumer probe is in
`crates/kaleido-core/bindings/kotlin-probe`; compile it with Gradle 8.14:

```text
gradle --project-dir crates/kaleido-core/bindings/kotlin-probe --no-daemon --console=plain compileKotlin
```

It uses Kotlin JVM plugin `2.2.20`, kotlinx.coroutines `1.11.0` for UniFFI's
generated async bridge, JNA `5.19.1`, and JDK 22.

The Swift probe is
`crates/kaleido-core/bindings/swift-probe/Probe.swift`. On macOS, compile and
link it together with both generated Swift sources and both generated FFI
modules:

```bash
swiftc --version
cargo build --locked -p kaleido-core --lib
cargo run --locked -p kaleido-core --bin uniffi-bindgen -- \
  generate --language swift \
  --out-dir target/uniffi/swift \
  target/debug/libkaleido_core.dylib

swift_out="$PWD/target/uniffi/swift"
swiftc -swift-version 5 -parse-as-library \
  -emit-library -emit-module \
  -module-name KaleidoCoreProbe \
  -emit-module-path "$swift_out/KaleidoCoreProbe.swiftmodule" \
  -I "$swift_out" \
  -L "$PWD/target/debug" \
  -lkaleido_core \
  -Xcc "-fmodule-map-file=$swift_out/kaleido_coreFFI.modulemap" \
  -Xcc "-fmodule-map-file=$swift_out/kaleido_protoFFI.modulemap" \
  "$swift_out/kaleido_proto.swift" \
  "$swift_out/kaleido_core.swift" \
  "$PWD/crates/kaleido-core/bindings/swift-probe/Probe.swift" \
  -o "$swift_out/libKaleidoCoreProbe.dylib"
test -s "$swift_out/libKaleidoCoreProbe.dylib"
```

The CI workflow runs the Kotlin compile gate only on `ubuntu-latest` and the
Swift compile-and-link gate only on `macos-latest`; both are hard failures.
Binding generation alone is not compilation evidence.

`cargo xtask fmt` is a formatting check; it does not rewrite files. To format
code before running the gates, use `cargo fmt --all`.

The forbidden-pattern scanner checks Rust source files under `crates/`,
`spikes/`, and `xtask/`. It excludes root `schemas/`, `target/`, and every
`tests/fixtures/` subtree. Dependency and crate-role rules come only from
`docs/dependency-rules.toml`.

## Adding a crate

After R1 defines `PROTOCOL.md`, every production crate created by a T-100+
task belongs under `crates/` and must be represented in all of these places in
the same change:

1. the root workspace `members` list;
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

## Redaction scans need their own test binary

`tracing` caches call-site interest process-wide: a call site first reached while
no subscriber is installed stays disabled for the rest of the process. A section
10 redaction scan that shares a binary with tests which run without a subscriber
would therefore capture an empty buffer and pass for the wrong reason.

`crates/kaleido-hostd/tests/tracing_redaction.rs` is a separate test binary for
this reason, and it asserts that the capture is non-empty before asserting what
is absent from it. Do not merge it into another test file.

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

GitHub Actions status is evidence only when linked to the exact commit under
review. Local Windows success must not be reported as macOS/Linux success.

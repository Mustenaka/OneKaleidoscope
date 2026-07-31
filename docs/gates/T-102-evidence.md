# T-102 UniFFI mobile-call-surface evidence

> Evidence date: 2026-07-31
> Restored implementation commit:
> `fde369c5bec241d0d623d57e7eb2f2d30173aa1a`
> Restored CI run:
> <https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614424449>

> Status: Swift/Kotlin compilation and both mutation proofs are complete.
> T-102 as a whole is still blocked by a pre-existing Windows-only CRLF test
> setup defect; the three-platform workflow is therefore not reported as green.

## Conclusion

**R3 的投影推送能不能走 UniFFI 回调？能。理由是 Kotlin 与 Swift 两端都实现并编译了
`ProjectionProbeCallback`，Rust 通过 `ProjectionSubscriptionProbe.subscribe` 调用它，
且同一门禁还编译了 object、携带 `CanonicalError` 的失败载荷与返回 `CommandAck` 的
async 调用面。**

This conclusion is about whether UniFFI 0.32 can express and compile the
required mobile call shape. It does not add a production subscription,
projection, session, or storage implementation.

## Evidence commit chain

| Purpose | Commit | Run | Outcomes |
|---|---|---|---|
| ADR-0016 implementation baseline | `1ba23c7cc7d78dd228b594e3b63c35862e12d2d4` | [30613601014](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30613601014) | macOS Swift success; Ubuntu Kotlin success; Windows CRLF blocker |
| deliberate Swift-name collision | `d971b23838d058bf767f8b9e67b4939f2f3917b2` | [30614223755](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614223755) | macOS repository checks success then Swift compilation fails exactly at the collision; Ubuntu remains success; Windows has the same independent CRLF blocker |
| mutation removed | `fde369c5bec241d0d623d57e7eb2f2d30173aa1a` | [30614424449](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614424449) | restored macOS Swift success; Ubuntu Kotlin success; Windows CRLF blocker |

None of these runs is described as overall green while the Windows matrix job
is red.

## Implemented probe surface

The probe uses canonical contract types directly; no shadow DTO was added.

| Shape | Rust export | Kotlin consumer | Swift consumer |
|---|---|---|---|
| foreign callback | `ProjectionProbeCallback` receives `ProjectionEnvelope` and `CanonicalError` | `ProjectionProbeSink` implements both methods | `ProjectionProbeSink` implements both methods |
| stateful object | `ProjectionSubscriptionProbe::new/subscribe/unsubscribe` | constructs, subscribes, then unsubscribes | constructs, subscribes, then unsubscribes |
| fallible call | `fallible_binding_probe -> Result<(), BindingProbeError>` with canonical error payload | catches `BindingProbeException.Canonical` and reads `error` | catches `BindingProbeError.Canonical` and reads `error` |
| async call | `async_binding_probe -> CommandAck` | suspend function calls it | async function awaits it |

Rust also has four executable tests covering callback retention and delivery,
unsubscribe, both fallible paths, canonical error preservation, and the async
acknowledgement:

```text
running 4 tests
test tests::async_probe_returns_the_canonical_ack ... ok
test tests::fallible_probe_has_a_success_path ... ok
test tests::fallible_probe_preserves_the_canonical_error_payload ... ok
test tests::subscription_calls_and_retains_the_foreign_callback_until_unsubscribe ... ok

test result: ok. 4 passed; 0 failed; 0 ignored
```

## Toolchain

| Tool | Exact evidence |
|---|---|
| Rust | `rustc 1.94.0 (4a4ef493e 2026-03-02)` |
| Cargo | `cargo 1.94.0 (85eff7c80 2026-01-15)` |
| UniFFI | `0.32.0` |
| successful macOS runner Swift | `Apple Swift version 6.3.2 (swiftlang-6.3.2.1.108 clang-2100.1.1.101)` |
| successful macOS target | `arm64-apple-macosx26.0`; swift-driver `1.148.6` |
| Ubuntu JDK | Temurin OpenJDK `22.0.2+9` |
| Gradle | `8.14` |
| Kotlin JVM plugin | `2.2.20` (the Gradle runtime reports embedded Kotlin `2.0.21`) |
| kotlinx.coroutines | `1.11.0` |
| JNA | `5.19.1` |

`thiserror` was already a workspace dependency and is used only for the
UniFFI-compatible error wrapper whose payload remains `CanonicalError`.
`kotlinx-coroutines-core` is required by the generated Kotlin async bridge;
JNA is required by the generated JVM FFI layer. No product runtime dependency
was introduced.

## Successful Swift and Kotlin compile evidence

The restored commit is
`fde369c5bec241d0d623d57e7eb2f2d30173aa1a`, run
<https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614424449>.

### macOS Swift

Job `91104224954`, step `Compile Swift UniFFI consumer probe`: **success**.
The step builds the cdylib, generates both Swift sources, compiles them together
with `Probe.swift`, links a dylib, and checks that the dylib is non-empty.

Raw log excerpt:

```text
Apple Swift version 6.3.2 (swiftlang-6.3.2.1.108 clang-2100.1.1.101)
Target: arm64-apple-macosx26.0
swift-driver version: 1.148.6
Running `target/debug/uniffi-bindgen generate --language swift --out-dir target/uniffi/swift target/debug/libkaleido_core.dylib`
```

The exact compile and output check printed by the same step were:

```bash
swiftc \
  -swift-version 5 \
  -parse-as-library \
  -emit-library \
  -emit-module \
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

`swiftc` is silent on success; the GitHub step and macOS job both concluded
`success`.

### Ubuntu Kotlin

Job `91104224953`, step `Compile Kotlin UniFFI consumer probe`: **success**.

Raw log excerpt:

```text
openjdk 22.0.2 2024-07-16
OpenJDK Runtime Environment Temurin-22.0.2+9
Welcome to Gradle 8.14!
Gradle 8.14
Kotlin:        2.0.21
Running `target/debug/uniffi-bindgen generate --language kotlin --out-dir target/uniffi/kotlin target/debug/libkaleido_core.so`
> Task :compileKotlin
BUILD SUCCESSFUL in 1m
1 actionable task: 1 executed
```

Kotlin runs on Ubuntu because this JVM probe has no Apple-platform dependency.
That keeps the macOS runner focused on the Apple-only Swift evidence while
still exercising generated bindings on a non-Windows system.

## Final hard-gate workflow YAML

There is no `continue-on-error`, `|| true`, `if: false`, or equivalent
softening. Matrix `fail-fast: false` lets all three platforms report; it does
not permit any job to fail.

```yaml
name: CI

'on':
  push:
  pull_request:
  workflow_dispatch:

permissions:
  contents: read

jobs:
  ci:
    name: ci (${{ matrix.os }})
    strategy:
      fail-fast: false
      matrix:
        os:
          - windows-latest
          - macos-latest
          - ubuntu-latest
    runs-on: ${{ matrix.os }}
    steps:
      - name: Checkout
        uses: actions/checkout@v7

      - name: Install Rust toolchain
        uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          cache: false
          rustflags: ""

      - name: Cache Rust build
        uses: Swatinem/rust-cache@v2

      - name: Set up JDK 22 for Kotlin probe
        if: matrix.os == 'ubuntu-latest'
        uses: actions/setup-java@v5
        with:
          distribution: temurin
          java-version: '22'

      - name: Set up Gradle 8.14 for Kotlin probe
        if: matrix.os == 'ubuntu-latest'
        uses: gradle/actions/setup-gradle@v6
        with:
          gradle-version: '8.14'

      - name: Run repository checks
        run: cargo xtask ci

      - name: Compile Kotlin UniFFI consumer probe
        if: matrix.os == 'ubuntu-latest'
        shell: bash
        run: |
          set -euo pipefail
          java --version
          gradle --version
          cargo build --locked -p kaleido-core --lib
          cargo run --locked -p kaleido-core --bin uniffi-bindgen -- \
            generate --language kotlin \
            --out-dir target/uniffi/kotlin \
            target/debug/libkaleido_core.so
          gradle \
            --project-dir crates/kaleido-core/bindings/kotlin-probe \
            --no-daemon \
            --console=plain \
            compileKotlin

      - name: Compile Swift UniFFI consumer probe
        if: matrix.os == 'macos-latest'
        shell: bash
        run: |
          set -euo pipefail
          swiftc --version
          cargo build --locked -p kaleido-core --lib
          cargo run --locked -p kaleido-core --bin uniffi-bindgen -- \
            generate --language swift \
            --out-dir target/uniffi/swift \
            target/debug/libkaleido_core.dylib

          swift_out="$PWD/target/uniffi/swift"
          swiftc \
            -swift-version 5 \
            -parse-as-library \
            -emit-library \
            -emit-module \
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

## Gate effectiveness: deliberate Swift break turned CI red

This was a temporary, isolated commit and is absent from the restored tree.

Mutation commit:
`d971b23838d058bf767f8b9e67b4939f2f3917b2`
Run:
<https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614223755>
macOS job: `91103597620`

The only mutation was:

```rust
#[uniffi::export]
pub fn probe_protocol_version() -> String {
    String::new()
}
```

UniFFI maps that export to `probeProtocolVersion()`, deliberately colliding
with the existing consumer probe function. Repository checks stayed green,
then the hard Swift step turned red:

```text
Apple Swift version 6.3.3 (swiftlang-6.3.3.1.3 clang-2100.1.1.101)
Target: arm64-apple-macosx26.0
Running `target/debug/uniffi-bindgen generate --language swift --out-dir target/uniffi/swift target/debug/libkaleido_core.dylib`
error: emit-module command failed with exit code 1 (use -v to see invocation)
target/uniffi/swift/kaleido_core.swift:1082:13: error: invalid redeclaration of 'probeProtocolVersion()'
1082 | public func probeProtocolVersion() -> String  {
     |             `- error: invalid redeclaration of 'probeProtocolVersion()'
##[error]Process completed with exit code 1.
```

Restore commit:
`fde369c5bec241d0d623d57e7eb2f2d30173aa1a`. The restored macOS Swift job is
green, and `git diff 1ba23c7..fde369c -- crates/kaleido-core/src/lib.rs` is
empty.

## ADR-0016 D-1: recorder test scope

`cargo xtask test` and the test phase inside `cargo xtask ci` now use on every
platform:

```text
cargo test --workspace --exclude kaleido-recorder
test: kaleido-recorder excluded on all platforms (ADR-0016)
```

`xtask/src/main.rs` has an executable test which asserts the exact arguments
and exact notice. `docs/DEVELOPMENT.md` separately retains the Windows-only
local regression command:

```text
cargo test -p kaleido-recorder
```

That local command still passed the frozen spike unchanged:

```text
running 164 tests
test result: ok. 164 passed; 0 failed; 0 ignored
```

The exclusion affects only behavior tests. Three-platform fmt, dependency
rules, forbidden-pattern lint, workspace clippy including `spikes/**`, fixture
verification, and all `crates/**` plus `xtask` tests remain hard gates.

## ADR-0016 D-2: single-leading-backslash leak candidates

D-2 is kept separate from D-1. The scanner now recognizes a single-leading
backslash root while preserving existing UNC recognition, sandbox comparison,
and placeholder suppression. It also avoids treating a later separator inside
an already recognized path or a JSON string escape as a new root.

Executable evidence:

- outside `open(\foo\secret.txt)` is reported at line 2,
  `/payload/body/version`, category
  `leak: absolute path outside fixture sandbox`;
- `\kaleido-t102-sandbox\safe-\inside.txt` is accepted as inside;
- the complete xtask fixture-test binary passes `35 passed`;
- committed evidence passes as `5 file(s), 220 record(s)`.

The dedicated implementation mutation changed the recognizer so it again
required two leading backslashes. The new outside test turned red:

```text
running 1 test
Error: Custom { kind: Other, error: "the rooted backslash path must be rejected" }
test single_leading_backslash_root_outside_sandbox_is_reported_as_a_leak ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 34 filtered out
error: test failed, to rerun pass `-p xtask --test fixtures`
```

After restoration:

```text
running 35 tests
test single_leading_backslash_root_outside_sandbox_is_reported_as_a_leak ... ok
test single_leading_backslash_root_inside_sandbox_is_not_reported ... ok
test result: ok. 35 passed; 0 failed; 0 ignored

<== fixtures-verify: ok; 5 file(s), 220 record(s) (codex: 3, acp-claude: 1, opencode: 1)
```

The restored macOS and Ubuntu jobs both independently print the same `5/220`
success. The Windows clean-checkout run currently stops at the unrelated CRLF
blocker below before reaching fixture verification; local Windows `5/220`
passed. Therefore the requested third CI-platform fixture proof remains
blocked rather than being inferred or fabricated.

This change does **not** decide whether `\` is a separator on Unix; D-B6 remains
open.

## Authorized cross-platform compile/lint fixes

These are the complete authorized exception-scope changes, with no assertion,
fixture, schema, or production redaction change:

1. `spikes/kaleido-recorder/src/agents/mod.rs:406-407`
   - Before: `validate_exact_permission_cwd` was in an unconditional grouped
     import.
   - After: it is a separate `#[cfg(windows)]` import.
   - Its only use is line 607 in
     `exact_permission_cwd_accepts_a_trailing_separator`, itself
     `#[cfg(windows)]` at line 598. Import and sole use now have identical cfg.
2. `xtask/src/schema.rs:1255-1270`
   - Before: `let mut command` existed on every platform although mutation by
     `creation_flags(CREATE_NO_WINDOW)` exists only on Windows.
   - After: Windows constructs a mutable command and applies the flag; the
     non-Windows cfg branch returns `Command::new(program)` directly.
   - Runtime command behavior is unchanged on each platform; the non-Windows
     unused-mut lint is removed without an allow.
3. `crates/kaleido-adapter-codex/src/platform/mod.rs:11-37`
   - Before: only Linux/macOS/Windows branches existed, so a fourth
     `target_os` returned `()` and left parameters unused.
   - After: `configure` explicitly consumes its parameter on unsupported
     targets; `terminate_tree` returns `io::ErrorKind::Unsupported`.
   - Android no longer fails to compile, and process-tree termination never
     lies by returning `Ok(())`.

## Frozen/protected scope hashes

The deterministic Git-tree manifest covered proto, `PROTOCOL.md`, ADRs,
requirements, schemas, committed fixtures, spikes, xtask, and the authorized
adapter platform module.

The first authorized cross-platform checkpoint was:

```text
before: 402 files
SHA-256 D8C60A71B34FE79DCE87D0344E27165FF908CCF254B158081E735CF1DFD6C642

after the three §5.4 fixes: 402 files
SHA-256 77B1162469F08A62CD41CAC1D09D447B8A95D8CCE3CBBEE3C68EEB30486CF01E

git diff --stat bdc1066604e527e1ac25ef7492d46d8b1037ceed..ead999cb43c3dad442fa89b69461f40ee9acc5a6
3 files changed, 19 insertions(+), 3 deletions(-)
```

After ADR-0016 D-1/D-2, a fresh aggregate was calculated rather than reusing
that earlier checkpoint:

```text
b11b32c baseline: 373 files
SHA-256 DA6A46D5C9D103AC19C6CF485994AC02525D65CAA1192780A0231A7D852FF5AF

1ba23c7 ADR-0016 implementation: 373 files
SHA-256 BEA85BB367B29A06BEB077CEA6FD10BF98B820A28AFD44EF1B11971238774904
```

The manifest difference is exactly these six authorized implementation paths,
and no others:

```text
crates/kaleido-adapter-codex/src/platform/mod.rs
spikes/kaleido-recorder/src/agents/mod.rs
xtask/src/schema.rs
xtask/src/main.rs
xtask/src/fixtures.rs
xtask/tests/fixtures.rs
```

Hard-contract diff:

```text
git diff --stat b11b32c..fde369c -- \
  crates/kaleido-proto docs/PROTOCOL.md docs/adr schemas tests/fixtures

# no output
```

Generated `target/`, Kotlin `build/`, and `.gradle/` outputs remain ignored and
`git ls-files` reports none of them.

## Gates and current blocker

The original Windows working tree completed the full gate:

```text
==> fmt-check
<== fmt-check: ok
==> check-deps
<== check-deps: ok
==> lint-forbidden
<== lint-forbidden: ok
==> clippy
<== clippy: ok
test: kaleido-recorder excluded on all platforms (ADR-0016)
==> test
<== test: ok
==> fixtures-verify
<== fixtures-verify: ok; 5 file(s), 220 record(s) (codex: 3, acp-claude: 1, opencode: 1)
```

Exit code: `0`.

However, the final clean Windows checkout exposes a pre-existing,
line-ending-sensitive xtask test. Restored run
<https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614424449>,
Windows job `91104224840`, and a new local worktree with Git for Windows'
`core.autocrlf=true` both reproduce it:

```text
test adapter_wildcard_does_not_match_the_shared_adapter_crate ... FAILED
thread 'adapter_wildcard_does_not_match_the_shared_adapter_crate' panicked at xtask\tests\deps.rs:305:10:
the concrete adapter wildcard must not match the shared crate:
CheckReport { workspace_members: 2, internal_edges: 1, crate_manifests: 0 }
test result: FAILED. 13 passed; 1 failed
xtask: step `test` failed with status exit code: 101
##[error]Process completed with exit code 1.
```

Root cause: the test embeds `docs/dependency-rules.toml` with `include_str!`
and removes an allow-list entry using a hard-coded LF substring. A Windows
checkout contains CRLF, so `replace` is a no-op and the test's synthetic rule
mutation never occurs. The assertion and dependency checker are not wrong;
the test setup is line-ending-sensitive. This test predates T-102.

The minimal suggested correction is test-local CRLF-to-LF normalization before
the existing exact replacement. It changes neither assertion nor production
dependency semantics. Changing `.gitattributes` or the dependency parser would
be broader alternatives.

This correction is outside ADR-0016 D-1/D-2 and T-102 §5.4's cfg-only
exception, so it has not been made without supervisor approval. Consequences:

- macOS Swift and Ubuntu Kotlin compile evidence is complete;
- the deliberate Swift-red evidence is complete and restored;
- local Windows functional gates and `5/220` fixture verification passed;
- final three-platform all-green CI and Windows CI `5/220` evidence remain
  blocked at this one pre-existing test.

## Unresolved observations

These were not fixed and must not be described as resolved:

- D-B6: Unix meaning of `\` in future security path validation; R9 prerequisite.
- D-B7: macOS `/var` symlink alias versus ancestor-link policy; R9 prerequisite.
- D-B8: `<HOME>` matching before `<SANDBOX>` in the frozen recorder; revisit
  during R4 redaction finalization.

## DoD status

- [x] Callback, object, fallible error, and async shapes compile and are
      actually consumed by Kotlin and Swift.
- [x] macOS Swift success is tied to a run URL, commit, job, version, and exact
      compile command.
- [x] Kotlin compilation is a hard Ubuntu CI step.
- [x] Deliberate conflicting export made the hard Swift step red; it was
      removed and the restored Swift step is green.
- [x] Workflow has no softening switch.
- [x] Proto, protocol, ADR, schema, and committed-fixture contract scopes are
      unchanged.
- [x] Generated artifacts are not tracked.
- [x] ADR-0016 D-1 is explicit and tested; D-2 has positive, negative, and
      mutation evidence.
- [ ] Final Windows clean-checkout `cargo xtask ci` and Windows CI fixture
      verification: blocked by the pre-existing CRLF-sensitive
      `xtask/tests/deps.rs` setup described above.

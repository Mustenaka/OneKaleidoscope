# T-102 UniFFI mobile-call-surface evidence

> Evidence date: 2026-08-01
> Validated implementation commit:
> `4d40c76cc1f3ba76f0144eeac552e2c4de476fbe`
> Three-platform green CI run:
> <https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30707060011>

> Status: Swift/Kotlin compilation, the Swift collision mutation, the fixture
> scanner mutation, and the ADR-0017 fail-loud mutation are complete. The same
> implementation SHA is green on macOS, Ubuntu, and Windows. One historical
> §5.4 before/after passed-count transcript remains unavailable and is marked
> explicitly below.

## Conclusion

**R3 的投影推送能不能走 UniFFI 回调？能。理由是 Kotlin 与 Swift 两端都实现并编译了
`ProjectionProbeCallback`，Rust 通过 `ProjectionSubscriptionProbe.subscribe` 调用它，
且同一门禁还编译了 object、携带 `CanonicalError` 的失败载荷与返回 `CommandAck` 的
async 调用面。**

This conclusion is about whether UniFFI 0.32 can express and compile the
required mobile call shape. It does not add a production subscription,
projection, session, or storage implementation, and it does not prove callback
thread scheduling, backpressure, process-death recovery, or a production
subscription lifecycle.

## Evidence commit chain

| Purpose | Commit | Run | Outcomes |
|---|---|---|---|
| ADR-0016 implementation baseline | `1ba23c7cc7d78dd228b594e3b63c35862e12d2d4` | [30613601014](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30613601014) | macOS Swift success; Ubuntu Kotlin success; Windows CRLF blocker |
| deliberate Swift-name collision | `d971b23838d058bf767f8b9e67b4939f2f3917b2` | [30614223755](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614223755) | macOS repository checks success then Swift compilation fails exactly at the collision; Ubuntu remains success; Windows has the same independent CRLF blocker |
| mutation removed | `fde369c5bec241d0d623d57e7eb2f2d30173aa1a` | [30614424449](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614424449) | restored macOS Swift success; Ubuntu Kotlin success; Windows CRLF blocker |
| ADR-0017 implementation | `4d40c76cc1f3ba76f0144eeac552e2c4de476fbe` | [30707060011](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30707060011) | macOS, Ubuntu, and Windows all success; Swift and Kotlin hard compile gates success; all three report fixture verification `5/220` |

The first three rows preserve the historical red Windows outcomes rather than
rewriting them. ADR-0017 removes the line-ending-dependent setup and the final
row is the first same-SHA, three-platform green run.

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

That command remains a local-only regression aid, but the complete package does
not currently pass. A clean Windows run executes the first 164-test binary
successfully, then later reaches the already registered D-B8 failure:

```text
running 164 tests
test result: ok. 164 passed; 0 failed; 0 ignored

test repository_fixture_sandbox_is_replaced_before_absolute_path_scanning ... FAILED
actual:   {"directory":"<OUTSIDE_PATH>"}
expected: {"directory":"<SANDBOX>"}

executed before Cargo stopped: 262 passed; 1 failed; 0 ignored
exit code: 101
```

No recorder source or assertion was changed. D-B8 remains unresolved exactly
as ADR-0016 requires; the failure is not part of the product/xtask CI test
scope, while the frozen spike remains under three-platform clippy.

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

The final same-SHA run
[30707060011](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30707060011)
prints the same `5/220` success independently on macOS, Ubuntu, and Windows.
The Windows job's raw line is:

```text
<== fixtures-verify: ok; 5 file(s), 220 record(s) (codex: 3, acp-claude: 1, opencode: 1)
```

This change does **not** decide whether `\` is a separator on Unix; D-B6 remains
open.

## ADR-0017 D-1: deterministic line endings and raw evidence bytes

The repository root now declares:

```gitattributes
* text=auto eol=lf

# 逐字节证据：永不做行尾翻译
schemas/**          -text
tests/fixtures/**   -text
```

Normal text is therefore deterministic LF. The committed schema snapshots and
fixtures are stronger: `-text` disables all line-ending translation. A fresh
Windows worktree at
`4d40c76cc1f3ba76f0144eeac552e2c4de476fbe`, with system
`core.autocrlf=true`, produced:

```text
evidence_files=295
byte_mismatches=0
dependency_rules_crlf_pairs=0
```

Each of the 295 working-tree files was hashed with `git hash-object
--no-filters` and compared with its `HEAD:<path>` blob. The protected Git trees
are unchanged across the ADR-0017 commit:

```text
schemas tree:        a03d3a47eec854d139a9f94fab16ff27884bdcfe
tests/fixtures tree: 060d9cca21d52482fa958cf50d508c2e1bc15064

git diff --stat 6de6eb0..4d40c76 -- schemas tests/fixtures
# no output
```

Thus the first checkout normalization changed ordinary text only; no schema or
fixture byte changed.

## ADR-0017 D-2: dependency-test setup is fail-loud

Only `xtask/tests/deps.rs` changed. The test setup first normalizes its embedded
rules from CRLF to LF, performs the existing exact replacement, then proves
that the replacement happened:

```rust
let normalized_rules = REPOSITORY_RULES.replace("\r\n", "\n");
let rules = normalized_rules.replace(
    "\"kaleido-adapter\",\n    \"kaleido-adapter-*\",",
    "\"kaleido-adapter-*\",",
);
assert_ne!(
    rules, normalized_rules,
    "test setup must remove the shared adapter allow-list entry"
);
```

`xtask/src/deps.rs`, the test's `expect_err`, and its business violation
assertion are unchanged. Restored green evidence is:

```text
adapter_wildcard_does_not_match_the_shared_adapter_crate ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out

running 14 tests
test result: ok. 14 passed; 0 failed; 0 ignored
```

The setup-specific mutation temporarily replaced the needle with the absent
`kaleido-adapter-never-present`. The new assertion failed before the business
assertion could run:

```text
assertion `left != right` failed: test setup must remove the shared adapter allow-list entry
test result: FAILED. 0 passed; 1 failed; 13 filtered out
```

The mutation was removed and the target test returned green. The dependency
test binary remains 14 tests before and after ADR-0017.

## ADR-0017 D-3: D-B10 clean-Windows schema-diff verification

D-B10 has now been exercised in a disposable clean Windows worktree with
`core.autocrlf=true`. Codex was the pinned `0.146.0`; native `codex.exe` and
`opencode.exe` were used with the committed ACP snapshot. A repository-external
worktree ran the literal command:

```text
cargo xtask schema diff
```

The raw result was:

```text
schema: observed codex 0.146.0 (snapshot 0.146.0), opencode 1.18.9 (snapshot 1.18.8), acp 1.18.0 (snapshot 1.18.0)
schema: NOTICE opencode version differs: observed 1.18.9, snapshot 1.18.8; comparison will continue
schema: WARNING unverified version for opencode: observed 1.18.9, supported range =1.18.8; comparison will continue
schema: fetching Codex 0.146.0, OpenCode 1.18.9, ACP crate 1.3.0 / schema 1.18.0
schema: used a configured ACP snapshot verified against commit 48b2abf1ac750fece26e03e92e773ccbd4754f5d
  in-surface    : 0 drift
  out-of-surface: 1 drift (0 added / 0 changed / 1 removed)
schema history: appended 3 new observation(s), deduplicated 0 existing observation(s)
schema diff: required surface is compatible (278 JSON files compared)
exit code: 0
```

The one informational out-of-surface removal was observed in the same run as
the explicitly shown OpenCode version mismatch. That correlation is recorded,
but no causal attribution is claimed. The machine conclusion is narrower: the
required surface has no drift, and there is no Windows line-ending-induced Git
blob false positive. The command appended three observations to
`schemas/surface-history.jsonl` only in the disposable worktree. That file was
restored to its committed empty state; afterwards `git status --short --
schemas tests/fixtures` had no output and the 295-file byte comparison again
reported zero mismatches.

One additional observation is not treated as a D-B10 failure: the first probe
through installed `.cmd` wrappers reached an existing fail-closed Windows
descendant-cleanup `Access denied` error (exit 3). The exact process was gone
and no orphan remained. Re-running with the native executables produced the
successful result above; no process-cleanup code was changed under T-102.

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

The earlier Windows delivery recorded `cargo xtask ci` exit zero around these
changes, and the diff adds or removes no test. However, the complete paired
per-test-binary passed-count transcript was not retained. A retrospective
checkout cannot recreate that historical exit-zero environment: it now reaches
the registered recorder D-B8 failure before the suite finishes. Therefore the
exact §5.4 “before and after counts” transcript is an explicit evidence gap,
not a newly manufactured success claim. The cfg/diff reasoning above and the
supervisor's scope verification remain valid, but they are not presented as a
substitute for the missing raw count log.

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

Through ADR-0016, the manifest difference is exactly these six authorized
implementation paths, and no others:

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
git diff --stat b11b32c..4d40c76 -- \
  crates/kaleido-proto docs/PROTOCOL.md docs/adr schemas tests/fixtures

# no output
```

This is a precise proof for commits *after* `b11b32c`, but not for history
before that snapshot. `b11b32c` was the first T-102 branch commit cut from a
dirty main worktree that already contained accepted R1/R2/T-100 contract files.
The old `ae9da23` main commit is therefore not a valid contract baseline for a
T-102-only diff. During final convergence, the owner must preserve the accepted
main-worktree contract content and the supervisor-authored documents rather
than treating this branch as a complete historical source.

ADR-0017 is a separate authorized increment:

```text
git diff --stat 6de6eb0..4d40c76
 .gitattributes      | 5 +++++
 xtask/tests/deps.rs | 7 ++++++-
 2 files changed, 11 insertions(+), 1 deletion(-)
```

Generated `target/`, Kotlin `build/`, and `.gradle/` outputs remain ignored and
`git ls-files` reports none of them.

## Final clean-checkout and three-platform gates

A repository-external fresh Windows worktree checked out
`4d40c76cc1f3ba76f0144eeac552e2c4de476fbe` under the system
`core.autocrlf=true`. It ran the literal required command `cargo xtask ci`:

```text
==> fmt-check
<== fmt-check: ok
==> check-deps
<== check-deps: ok; 9 workspace member(s), 9 internal edge(s), 6 crates/* manifest(s)
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

Exit code: `0`; elapsed: `353.5s`. The worktree remained clean and
`git diff --exit-code -- schemas tests/fixtures` also returned zero. The
pre-ADR original Windows working tree and the post-ADR clean checkout both
complete `cargo xtask ci` with exit zero. The dependency integration binary
remains `14 passed`, so ADR-0017 changed neither its count nor business
assertions.

The same implementation SHA is green in CI run
[30707060011](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30707060011):

| Job | ID | Conclusion | Platform-specific evidence |
|---|---:|---|---|
| `ci (macos-latest)` | `91387880690` | success | Swift binding generation and consumer compilation |
| `ci (ubuntu-latest)` | `91387880710` | success | generated Kotlin consumer `:compileKotlin` |
| `ci (windows-latest)` | `91387880717` | success | clean-checkout repository gates and fixture verification |

Final macOS raw excerpt:

```text
Apple Swift version 6.3.3 (swiftlang-6.3.3.1.3 clang-2100.1.1.101)
Target: arm64-apple-macosx26.0
swift-driver version: 1.148.6
Running `target/debug/uniffi-bindgen generate --language swift --out-dir target/uniffi/swift target/debug/libkaleido_core.dylib`
```

The step then ran the exact `swiftc` command recorded earlier in this file and
its `test -s` output check; the job concluded success. The earlier accepted run
`30614424449` remains the raw Swift 6.3.2 evidence requested by the card, while
this final same-SHA run used the runner's current Swift 6.3.3.

Final Ubuntu raw excerpt:

```text
test: kaleido-recorder excluded on all platforms (ADR-0016)
<== fixtures-verify: ok; 5 file(s), 220 record(s) (codex: 3, acp-claude: 1, opencode: 1)
> Task :compileKotlin
BUILD SUCCESSFUL in 1m 6s
```

Final Windows raw excerpt:

```text
4d40c76cc1f3ba76f0144eeac552e2c4de476fbe
<== fmt-check: ok
<== check-deps: ok; 9 workspace member(s), 9 internal edge(s), 6 crates/* manifest(s)
<== lint-forbidden: ok
<== clippy: ok
test: kaleido-recorder excluded on all platforms (ADR-0016)
<== test: ok
<== fixtures-verify: ok; 5 file(s), 220 record(s) (codex: 3, acp-claude: 1, opencode: 1)
```

## Branch/main convergence state

The implementation branch and the supervisor's main worktree are intentionally
not merged in this delivery:

- `codex/t-102-uniffi-probe` contains the implementation and evidence commits,
  but not ADR-0015/0016/0017, the four T-102 unblock rulings, or the task-card
  §5.4–§5.7 text subsequently authored in the main worktree;
- the main worktree remains at `ae9da23` with its pre-existing uncommitted
  R1/R2 and supervisor-authored documents intact;
- this evidence file is also placed at main-worktree
  `docs/gates/T-102-evidence.md`, without checkout, merge, cherry-pick, or
  replacement of those supervisor documents.

The owner must converge both sources while retaining the main-worktree ADR and
task text. Neither source by itself is a complete final documentation tree.

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
- [x] The active workspace-test gate is the §5.5/ADR-0016 replacement:
      `cargo test --workspace --exclude kaleido-recorder`, exercised through
      `cargo xtask test`/`ci`. The literal superseded `cargo test --workspace`
      is not claimed green: the documented Windows run fails at D-B8, while
      the non-Windows recorder failures D-B6/D-B7 also remain.
- [x] ADR-0017 D-1 preserves all 295 raw evidence files byte-for-byte; D-2 is
      fail-loud with green and red mutation evidence; D-B10 has a clean-Windows
      schema-diff result with zero required-surface drift.
- [x] Final Windows clean-checkout `cargo xtask ci`, same-SHA three-platform CI,
      and all three platforms' `5/220` fixture verification are green.
- [ ] The exact paired per-binary passed-count transcript requested by §5.4
      was not retained from the original Windows environment; retrospective
      checkout is stopped by the already registered D-B8 as documented above.

# Upstream schema compatibility

> **Rebaseline notice (2026-07-30):** this file and `schemas/` are frozen
> upstream evidence and drift-monitoring assets. They are not an active product
> gate. A product slice may use only the protocol surface derived from
> `PROTOCOL.md`; collecting or generating every upstream type must not block it.

The JSON files under `schemas/codex/`, `schemas/opencode/`, and `schemas/acp/`
are byte-preserving upstream snapshots. They are the baseline for protocol
design and drift review. Generated or normalized schema must never be written
back into those directories.

## Snapshot sources

| Upstream | Snapshot version | Snapshot |
|---|---|---|
| Codex app-server | `codex-cli 0.147.0` / `@openai/codex@0.147.0` | `schemas/codex/` |
| OpenCode | `opencode 1.18.16` / `opencode-ai@1.18.16` | `schemas/opencode/openapi.json` |
| Agent Client Protocol | crate `1.3.0`, wire v1, schema artifact `1.18.0` | `schemas/acp/` |

The ACP tags `v1.3.0` and `schema-v1.18.0` both resolve to commit
`48b2abf1ac750fece26e03e92e773ccbd4754f5d`. The snapshot uses
`schema/v1/schema.json` and `schema/v1/meta.json` from that immutable commit.
Exact installation and capture commands are recorded in
`schemas/VERSIONS.md`.

On 2026-08-10, both `npm view opencode-ai version` and
`npm view @opencode-ai/sdk version` returned `1.18.16`. This confirms the CLI,
published SDK and pinned `/doc` use the same public version label; it does not
erase the runtime contract drift recorded below.

Snapshot version, observed version, and supported range are deliberately
different concepts:

- **Snapshot version** records the immutable source of the committed full
  snapshot.
- **Observed version** is the version installed when `schema diff` runs. It is
  data to inspect, not an admission check.
- **Supported range** records versions backed by successful project evidence.
  It drives a warning, never a refusal to inspect a newer or older version.

## Provisional required surface

`schemas/required-surface.toml` is the single source of truth for the upstream
methods and types required by UACP. Every entry has a concrete reason tied to a
UACP event variant or method family. T-011 establishes a provisional surface;
the project supervisor will reconcile it entry by entry when `PROTOCOL.md` is
finalized.

Required-surface entries must be derived from protocol needs. A drifting entry
must never be removed merely to make `schema diff` pass. Full snapshots remain
the evidence for out-of-surface changes, while the required surface determines
whether drift is release-blocking.

Method entries own the method/operation envelope itself. They deliberately do
not make every transitively referenced payload type required: for example,
following `GET /event` through the aggregate `Event` union would pull unrelated
LSP, formatter, and server events into UACP's compatibility contract. Payload
dependencies must therefore appear as explicit `kind = "type"` entries; those
type entries do include their complete local `$ref` closure. The supervisor's
`PROTOCOL.md` reconciliation must check both halves for every method.

The OpenCode snapshot contains three concurrently available permission-reply
routes with different wire shapes:

- `POST /session/{sessionID}/permissions/{permissionID}` is the provisional
  route named by T-011 and uses a `response` field.
- `POST /permission/{requestID}/reply` is the legacy route selected by the
  recorder and uses a `reply` field.
- `POST /api/session/{sessionID}/permission/{requestID}/reply` is the v2 route
  selected by the recorder and uses `PermissionV2Reply`.

All three are declared. The recorder-observed routes supplement the task-card
route; they do not replace or reinterpret it.

## Supported ranges

Supported ranges are evidence statements and warning thresholds, not runtime
feature switches or schema-diff gates.

| Upstream | Snapshot version | Supported range | Evidence |
|---|---|---|---|
| Codex app-server | `0.147.0` | `>=0.146.0, <=0.147.0` | T-100 recorded real `0.146.0` simple-turn and approval evidence. T-105 reviewed all 58 required-surface changes, resolved all 41 pinned paths / 47 schema anchors, and completed real structured app-server simple turns on `0.146.1` and `0.147.0`; exact `0.146.1` schema diff against `0.146.0` had 0 in-surface drift. |
| OpenCode | `1.18.16` | `=1.18.16` warning candidate; live acceptance blocked | T-111 aligns the raw `/doc`, required surface, CLI and SDK version label, but the latest real `/event` violates that same schema. Earlier `1.18.8` / `1.18.11` fixtures remain historical regressions, not current support claims. |
| ACP v1 schema artifact | `1.18.0` | `=1.18.0` | T-003 pinned the immutable artifact and T-004/T-006 validated ACP v1 lifecycle, filesystem, and terminal messages against it. |

OpenCode's configured warning candidate is deliberately exact at `1.18.16`; it
is not currently an accepted realtime support claim because the live contract
gate below fails. The historical surface
ledger contains observations for `1.18.8`, `1.18.15` and `1.18.16`; the
committed `1.18.11` fixture supplies an additional real runtime regression.
Those artifacts remain useful drift/history evidence but do not justify a
continuous range with untested patch members. Versions outside `1.18.16`
remain inspectable and print the ADR-0008 `unverified version` warning. Adapter
behavior is still selected from structured runtime evidence, never from
`if version >= ...`.

### Codex 0.147.0 review

T-105 grouped the 58 in-surface changes into 13 required-surface entries. None
of them changed an adapter-read JSON Pointer or schema anchor. The changes are
transitive `CommandAction.path` references, optional initialization and item
fields, the `thread/list` pin-to-section model, and a new required
`request_user_input.isBlocking` field. The last two areas are not implemented
by the current R2 adapter and therefore remain future integration risks rather
than supported capabilities. The complete path-by-path accounting and real
runtime evidence are recorded in `docs/gates/T-105-evidence.md`.

Merge review also closed the otherwise implicit `0.146.1` interval member:
its exact schema has 0 in-surface drift from `0.146.0` and its native
app-server completed a real structured simple turn. The continuous supported
range therefore contains no version that lacks both schema and runtime evidence.

Adapter behavior must be selected from runtime capabilities and the observed
schema, never from a comparison such as `if version >= ...`.

## Local commands

Fetch the schemas from the actually installed Codex and OpenCode versions into
an ignored staging directory and compare them with the committed snapshots:

```text
cargo xtask schema diff
```

The command always reports snapshot and observed versions. A version mismatch
does not prevent acquisition or comparison. Its partitioned report has:

- **in-surface drift**: exact required-surface entry and JSON path; causes
  failure because an adapter dependency needs review;
- **out-of-surface drift**: counts and summary only; informational because UACP
  does not depend on that area.

The schema-diff exit-code contract is:

| Code | Meaning |
|---:|---|
| `0` | No in-surface drift, including different versions whose required surfaces are semantically equal |
| `1` | One or more required-surface entries drifted |
| `2` | A required tool does not exist or cannot be executed; version mismatch is not this condition |

Object key order and whitespace do not count as drift. Added, removed, or
changed values are reported as escaped JSON paths. When an observed version is
new, its required-surface digests are appended once to
`schemas/surface-history.jsonl`; full snapshots are not retained per version.

Inspect one required-surface entry across observed versions with:

```text
cargo xtask schema history <tool> <entry-id>
```

Refresh the full snapshots only after the partitioned drift report has been
reviewed:

```text
cargo xtask schema refresh
```

Refresh replaces the current full snapshot with the observed version and
updates its provenance. It must preserve `schemas/required-surface.toml` and
`schemas/surface-history.jsonl`; neither file is an upstream snapshot.

## Upgrade and compatibility process

1. Run `cargo xtask schema diff` with the version actually installed by the
   user or scheduled environment.
2. Review every in-surface path and the summarized out-of-surface changes.
3. Record successful adapter/fixture evidence before widening a supported
   range.
4. Refresh the full snapshot only after the observed drift is understood.
5. Review the raw snapshot, provenance, required-surface digests, and supported
   range as separate artifacts.
6. Run `cargo xtask ci` and the schema-diff tests.

No step requires downgrading merely to inspect drift. Conversely, seeing no
in-surface drift does not by itself widen the supported range; the range is
backed by runtime and fixture evidence.

## Scheduled check

`.github/workflows/schema-drift.yml` runs daily and on manual dispatch. The
workflow must exercise the same `schema diff` behavior: discover and report the
observed versions, continue when they differ from the snapshot, fail only for
in-surface drift or an unavailable tool, and retain the complete partitioned
report. It must not contain a separate exact-version equality gate.

The workflow is exercised on GitHub Actions. `schema diff` remains separate from `cargo xtask ci`
because it needs external CLIs, outbound acquisition, and writes an observation
to surface history; the normal deterministic CI gate does not.

Each scheduled run uploads `schema-diff.log` together with the resulting
`surface-history.jsonl` as a retained artifact, including on a drift failure.
The workflow has read-only repository permissions and therefore never commits
generated observations directly. A reviewed local run or reviewed artifact
promotion must merge new history records into the repository; this keeps the
longitudinal record durable without granting an unattended workflow write
access.

## Schema normalization

ADR-0005 requires generated and normalized artifacts to live outside
`schemas/`. The required surface is derived from `PROTOCOL.md`, never from
which types a particular generator happens to accept.

### OpenCode OpenAPI 3.1

R5 implements the chain selected by [ADR-0026](adr/0026-opencode-generated-rest-sse.md):

```text
schemas/opencode/openapi.json (read-only OpenAPI 3.1)
  -> deterministic normalization
  -> protocol-derived operation/type closure
  -> build-directory Rust generation
```

The `1.18.16` source document currently exercises exactly one normalization
rule:

| Rule | Before | After | Hits |
|---|---|---|---:|
| `numeric_exclusive_minimum_to_bound` | numeric `exclusiveMinimum: n` | `minimum: n` and `exclusiveMinimum: true` | 25 |

This is the draft-07 spelling of the same strict lower bound; it neither drops
fields nor widens the constraint. The unit test asserts the before/after shape,
and the real-snapshot test asserts the exact hit count. The generated closure
currently contains 117 schemas. `EventPluginAdded` and
`EventSessionNextPromptAdmitted` are generated specifically so real stream
hygiene remains checked by upstream types. No zero-hit rule is retained. Normalized and
generated files are build artifacts and are not committed.

The latest real `opencode serve --pure` `1.18.16` probe found that `/event`
sends `session.next.prompt.admitted.properties.timestamp` as a string while the
pinned `/doc` `EventSessionNextPromptAdmitted` requires a number. The same
stream sends `server.heartbeat`, which `/doc` does not declare at all. The
adapter rejects the generated-type mismatch instead of adding a handwritten or
untyped escape hatch. Consequently D-B11 and the realtime/recovery live gate
remain blocked even though the raw snapshot, CLI and SDK all say `1.18.16`.

### Claude Agent SDK bridge

Claude has no schema snapshot in `schemas/`. R5 instead pins
`@anthropic-ai/claude-agent-sdk@0.3.226` and its lockfile inside
`crates/kaleido-adapter-claude/bridge`. The TypeScript sidecar consumes official
SDK types (including `AskUserQuestionInput`) and emits a closed, versioned
OneKaleidoscope frame; Rust does not hand-write Claude SDK DTOs. Exact ownership,
provisional-session and evidence semantics are defined by
[ADR-0027](adr/0027-claude-sdk-sidecar-provisional-session.md).

`cargo xtask claude-sidecar` runs platform-correct `npm` / `npm.cmd`,
`npm ci --ignore-scripts`, then strict typecheck. It is part of
`cargo xtask ci`; the Windows/macOS/Linux workflow installs Node 22 before the
shared gate. Ignoring package scripts prevents install-time provider execution;
the actual SDK bridge still runs only in explicit runtime/fixture commands.

The committed Claude fixture is a real SDK run whose OAuth refresh failed. It
is valid failure-path evidence, not a successful turn or permission/question
acceptance claim. Its metadata fixes `expected_outcome = authentication_failure`
and `acceptance_eligible = false`; fixture verification rejects any attempt to
present it as success. See `docs/gates/T-112-evidence.md`.

## Temporary offline Rust vendoring

The T-004 recorder needed the following crates in a network-restricted build
environment where Cargo could not download them from crates.io:

| Crate | Exact version | Reason for vendoring |
|---|---:|---|
| `directories` | `6.0.0` | Required for platform-correct user directories and unavailable from crates.io in the offline environment |
| `dirs-sys` | `0.5.0` | Transitive platform implementation required by `directories 6.0.0` |
| `option-ext` | `0.2.0` | Transitive dependency required by `dirs-sys 0.5.0` |

The root `[patch.crates-io]` table redirects the entire workspace to the
unmodified sources under `vendor/`. This is an offline build accommodation,
not a permanent upstream selection.

**Removal condition:** after the build environment regains network access,
delete the root `[patch.crates-io]` table and the entire `vendor/` directory,
then confirm that `cargo build --locked` succeeds using the registry-resolved
dependencies. Remove both overrides in the same reviewed change so the
workspace cannot retain one half of the temporary configuration.

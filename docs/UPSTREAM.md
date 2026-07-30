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
| Codex app-server | `codex-cli 0.146.0` / `@openai/codex@0.146.0` | `schemas/codex/` |
| OpenCode | `opencode 1.18.8` / `opencode-ai@1.18.8` | `schemas/opencode/openapi.json` |
| Agent Client Protocol | crate `1.3.0`, wire v1, schema artifact `1.18.0` | `schemas/acp/` |

The ACP tags `v1.3.0` and `schema-v1.18.0` both resolve to commit
`48b2abf1ac750fece26e03e92e773ccbd4754f5d`. The snapshot uses
`schema/v1/schema.json` and `schema/v1/meta.json` from that immutable commit.
Exact installation and capture commands are recorded in
`schemas/VERSIONS.md`.

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
| Codex app-server | `0.146.0` | `=0.146.0` | T-006 refreshed and validated the `0.146.0` schema; the owner's T-010 path recorded a real simple turn with this CLI. |
| OpenCode | `1.18.8` | `=1.18.8` | T-003 captured the OpenAPI document and T-004/T-006 validated real session and event traffic against `1.18.8`. |
| ACP v1 schema artifact | `1.18.0` | `=1.18.0` | T-003 pinned the immutable artifact and T-004/T-006 validated ACP v1 lifecycle, filesystem, and terminal messages against it. |

OpenCode `1.18.9` has been observed on the owner's machine but is not yet in
the supported range. The previous exact-version gate prevented the comparison
from running, so no compatibility claim is made yet. Under ADR-0008,
`schema diff` must still run normally and print a prominent
`unverified version` warning.

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

The repository currently has no configured remote, so this workflow has not
run on GitHub Actions. `schema diff` remains separate from `cargo xtask ci`
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

Schema normalization and Rust type generation are intentionally outside this
workflow. ADR-0005 requires generated and normalized artifacts to live outside
`schemas/`. The required surface must be derived from `PROTOCOL.md`, never from
which types a particular generator happens to accept.

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

# Upstream schema snapshots

The files under `schemas/` are byte-preserving snapshots of upstream protocol
descriptions. They are the baseline for protocol design and drift review.
Generated or normalized schema must never be written back into this directory.

## Pinned sources

| Upstream | Tool or artifact version | Snapshot |
|---|---|---|
| Codex app-server | `codex-cli 0.146.0` / `@openai/codex@0.146.0` | `schemas/codex/` |
| OpenCode | `opencode 1.18.8` / `opencode-ai@1.18.8` | `schemas/opencode/openapi.json` |
| Agent Client Protocol | crate `1.3.0`, wire v1, schema artifact `1.18.0` | `schemas/acp/` |

The ACP tags `v1.3.0` and `schema-v1.18.0` both resolve to commit
`48b2abf1ac750fece26e03e92e773ccbd4754f5d`. The snapshot uses
`schema/v1/schema.json` and `schema/v1/meta.json` from that immutable commit.
Exact installation and capture commands are recorded in
`schemas/VERSIONS.md`.

## Local commands

Check that installed CLIs match the pinned versions, fetch all three sources
into a staging directory, validate every JSON file, and atomically replace the
snapshot only after all sources succeed:

```text
cargo xtask schema refresh
```

Fetch all sources into an ignored temporary directory and compare parsed JSON
values against the committed snapshot:

```text
cargo xtask schema diff
```

Object key order and whitespace do not count as drift. Added, removed, or
changed values are reported as repository-relative files followed by escaped
JSON Pointers. The command returns 0 for no drift, 1 for drift, 2 when a pinned
upstream CLI is missing or has the wrong version, and 3 for other acquisition
or validation failures.

## Upgrade process

1. Obtain architecture approval for the exact new upstream version.
2. Update the pinned constants, workflow installation versions, and this
   version table in one reviewed change.
3. Install the exact approved CLI versions.
4. Run `cargo xtask schema diff` and review every reported JSON Pointer.
5. Run `cargo xtask schema refresh` only after the drift is understood.
6. Review the raw snapshot changes and `schemas/VERSIONS.md`.
7. Run `cargo xtask ci` and the schema-diff tests.

Schema normalization and Rust type generation are intentionally outside this
workflow. ADR-0005 requires those artifacts to live outside `schemas/`; their
implementation and generator selection belong to T-005.

## Scheduled check

`.github/workflows/schema-drift.yml` runs daily and on manual dispatch. It
installs the exact Codex and OpenCode versions above, verifies their reported
versions, and invokes only `cargo xtask schema diff`. The repository currently
has no configured remote, so this workflow has not yet run on GitHub Actions.

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

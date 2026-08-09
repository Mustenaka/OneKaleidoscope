# T-105 evidence — Codex 0.147.0 required-surface review

> Date: 2026-08-09  
> Baseline: `main@75f3588`  
> Branch: `codex/t-105-codex-0-147-compat`

## Conclusion

Codex `0.147.0` is compatible with the currently implemented R2 slice. The 58
in-surface changes reported by Schema drift belong to 13 required-surface
entries and 20 unique schema paths. None changes the 41 adapter-read JSON
Pointers or their 47 schema anchors. A real structured app-server simple turn
also completed successfully on the exact CLI.

No adapter, reducer, UACP, or proto change is required. T-105 therefore updates
only the unmodified upstream snapshot, provenance/history, the evidence-backed
Codex support interval, and documentation.

This is not a compatibility claim for two future capabilities:

- `item/tool/requestUserInput` now requires `isBlocking`; the R2 adapter does
  not implement that method and safely reports it as unmodelled.
- `thread/list` and resume now use the section model in place of pinning; those
  paths are not implemented by the R2 adapter.

## Accounting for all 58 paths

| Required-surface entry | Count | Review result |
|---|---:|---|
| `CommandExecutionRequestApprovalParams` | 2 | `CommandAction.path` changes from the removed `AbsolutePathBuf` reference to `LegacyAppPathString`. Both serialize as strings. The current adapter does not read command-execution approval/action paths. |
| `InitializeParams` | 2 | Adds optional `capabilities.extensions`; the existing elicitation capability receives a legacy-description change. The current initialize request remains valid. |
| `ItemCompletedNotification` | 3 | Repeats the action-path closure change and adds optional `readOnlyHint` / `transparentBackground` on unsupported item variants. All adapter-read item fields are unchanged. |
| `ItemStartedNotification` | 3 | Same closure/optional-field changes as item completion; item identity, type, content and lifecycle anchors remain unchanged. |
| `ServerRequest` | 8 | Repeats the action-path closure change. `request_user_input` adds required `isBlocking`, deprecates `autoResolutionMs`, and shifts five required-array positions. This method is not an implemented R2 capability. |
| `ThreadListParams` | 3 | Removes optional `isPinned`, adds optional `sectionId`, and adds `section_position` to the sort enum. `thread/list` is not implemented by the current adapter. |
| `ThreadListResponse` | 7 | Repeats the action/item changes and replaces optional pin data with optional section data plus `ThreadSection`. No pinned runtime path targets these fields. |
| `ThreadResumeResponse` | 7 | Same thread-section closure change; resume is not implemented by the current adapter. |
| `ThreadStartResponse` | 7 | Same closure change; the actually read `/result/thread/id` and `/result/cwd` paths and anchors are unchanged. |
| `ThreadStartedNotification` | 7 | Same closure change; the actually read `/params/thread/id` path and anchor are unchanged. |
| `TurnCompletedNotification` | 3 | Action-path closure plus two optional unsupported-item fields. Turn ID/status/items view/timestamps/error anchors are unchanged. |
| `TurnStartResponse` | 3 | Same closure/optional-item changes; the read turn ID/status paths are unchanged. |
| `TurnStartedNotification` | 3 | Same closure/optional-item changes; the read thread/turn/status/start-time paths are unchanged. |

Cross-check by change family: 12 transitive `$ref` changes + 2 initialize
changes + 18 repeated optional ThreadItem fields + 7 request-user-input changes
+ 3 list filter/sort changes + 16 repeated section-model changes = **58**.

## Exact schema and pinned-surface evidence

The exact package was installed into a temporary npm prefix outside the
repository. It generated 285 JSON files. Before refresh, a semantic comparison
of all declared paths and anchors reported:

```text
PINNED_PATHS=41
SCHEMA_ANCHORS=47
MISSING=0
SEMANTIC_CHANGED=0
TITLE_MISMATCH=0
```

`cargo xtask schema refresh` was then run with all three sources pinned:

```text
Codex:   @openai/codex@0.147.0
OpenCode: opencode-ai@1.18.8
ACP:     committed schema/v1 snapshot at 48b2abf1ac750fece26e03e92e773ccbd4754f5d

schema history: appended 0 new observation(s), deduplicated 3 existing observation(s)
schema refresh: wrote 285 Codex, 1 OpenCode, and 2 ACP JSON file(s)
REFRESH_EXIT=0
```

The OpenCode and ACP snapshots remained byte-identical. The four pre/post
SHA-256 values were:

```text
schemas/opencode/openapi.json  12A1AF95508B7B32D0A36E664CF9B35B90349645F5E37F2B12ACA479A43C9211
schemas/acp/schema.json        92C1DFCDA10DD47E99127500A3763DA2B471F9AC61E12B9BF0430C32CF953796
schemas/acp/meta.json          E0BF36F8123B2544B499174197FDC371EC49A1B4572A35114513D56492741599
schemas/required-surface.toml  3525C541E7D5270BA452612A41359E8FD19A3372E65B2CD1317C3D02E0A9CAE3
```

The required-surface hash above is the pre-review value; its only intentional
subsequent change is the Codex `supported_range`.

After refresh, the committed-snapshot guard resolves every pointer/anchor:

```text
running 6 tests
test element_scoped_pointers_are_relative_and_payload_pointers_are_absolute ... ok
test no_pinned_path_reads_the_turn_completion_item_summary ... ok
test the_pinned_decision_vocabulary_matches_the_committed_schema ... ok
test every_declared_surface_identifier_exists_in_the_required_surface ... ok
test the_table_has_no_duplicate_and_no_unused_entry ... ok
test every_pinned_pointer_resolves_in_the_committed_schema_snapshot ... ok

test result: ok. 6 passed; 0 failed; 0 ignored
```

## Real Codex 0.147.0 app-server run

The executable was the native binary from the isolated package. `slice run`
communicated only through app-server JSON-RPC over stdio; no PTY/TUI path was
used. Project and durable-log directories were temporary directories outside
the repository.

```text
codex-cli 0.147.0

cargo run -p kaleido-hostd -- slice run \
  --executable <isolated-0.147.0-codex.exe> \
  --project-root <temporary-project> \
  --log-dir <temporary-log> \
  --prompt "Reply with exactly this plain text and do not use any tool: KALEIDO T105 CODEX 0.147.0" \
  --timeout-secs 180

termination: turn_terminal
turn.status: completed
live_activity_while_streaming: observed
agent content: KALEIDO T105 CODEX 0.147.0
exit: 0
```

Approval recording was not required for this review: all six pinned
file-change approval paths and its request/response schemas are unchanged.
The drifting command-execution and request-user-input paths are deliberately
not represented as supported R2 capabilities.

## Local schema gates

Exact reviewed versions:

```text
schema: observed codex 0.147.0 (snapshot 0.147.0), opencode 1.18.8 (snapshot 1.18.8), acp 1.18.0 (snapshot 1.18.0)
in-surface    : 0 drift
out-of-surface: 0 drift (0 added / 0 changed / 0 removed)
schema history: appended 0 new observation(s), deduplicated 3 existing observation(s)
schema diff: required surface is compatible (288 JSON files compared)
PINNED_SCHEMA_DIFF_EXIT=0
```

Current npm latest at validation time was Codex `0.147.0` and OpenCode
`1.18.15`:

```text
in-surface    : 0 drift
out-of-surface: 12 drift (4 added / 1 changed / 7 removed)
schema history: appended 1 new observation(s), deduplicated 2 existing observation(s)
schema diff: required surface is compatible (288 JSON files compared)
LATEST_SCHEMA_DIFF_EXIT=0
```

The OpenCode version warning and 12 out-of-surface changes do not widen its
supported range; D-B11 remains open for R5.

## Full local gate

The first full run exposed the stale `0.146.0` installation-hint assertion
described under findings. After the narrow test expectation update, the full
gate completed:

```text
==> fmt-check
<== fmt-check: ok
==> check-deps
<== check-deps: ok; 9 workspace member(s), 9 internal edge(s), 6 crates/* manifest(s)
==> lint-forbidden
<== lint-forbidden: ok
==> clippy
<== clippy: ok
==> test
<== test: ok
==> fixtures-verify
<== fixtures-verify: ok; 5 file(s), 220 record(s) (codex: 3, acp-claude: 1, opencode: 1)
cargo xtask ci exit: 0
```

## Mutation evidence

The real `SessionThreadId` path was temporarily changed from
`/result/thread/id` to `/result/thread/T105_MUTATION_missing_id`. The complete
surface-drift test failed while reducing the real simple-turn fixture:

```text
the_table_has_no_duplicate_and_no_unused_entry ... FAILED
01-simple-turn.jsonl must reduce cleanly: pinned pointer
`/result/thread/T105_MUTATION_missing_id` for SessionThreadId did not resolve
test result: FAILED. 5 passed; 1 failed
MUTATION_FULL_TEST_EXIT=101
```

The path was restored with `apply_patch`; the six-test green result above was
then reproduced. The mutation never entered a commit.

## Remote validation

Remote run URLs and the tested implementation commit are recorded here after
the local full gate is green and the branch is pushed.

## Deviations and findings

- No implementation or protocol deviation was necessary.
- No dependency was added.
- Existing real fixtures remain unchanged.
- The snapshot-version error-path assertion in `xtask/tests/schema_diff.rs`
  was mechanically updated from `0.146.0` to `0.147.0` after the first full CI
  run correctly failed on the stale installation hint expectation. Production
  xtask behavior and exit-code logic were not changed.
- `request_user_input.isBlocking` and the thread section model must be handled
  when those capabilities are implemented; this task does not claim them.
- OpenCode D-B11 remains open. Latest `1.18.15` is surface-compatible with the
  R2 required subset but has 12 out-of-surface changes relative to `1.18.8`.

# Claude Agent SDK managed-session recording

`real-sdk-simple-turn.jsonl` is captured from the pinned
`@anthropic-ai/claude-agent-sdk@0.3.226` bridge against the isolated
`toy-project` directory.  The bridge reached a real SDK-managed session and
emitted `init`, a non-error assistant text block, and a terminal successful
result.  Its metadata marks the capture as acceptance eligible only because
all of those real-provider events are present and the SDK version is pinned.

The capture command starts `bridge/index.ts` with Node's type stripping,
sends `start` and one deterministic prompt, waits for the SDK result, then closes the
sidecar.  Absolute paths and plugin paths were replaced with
`<redacted-path>` before committing.  Raw Claude session/message UUIDs remain
inside this adapter-private fixture only and are never copied into canonical
state.

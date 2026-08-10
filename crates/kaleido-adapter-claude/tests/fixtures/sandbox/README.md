# Claude Agent SDK managed-session recording

`real-sdk-simple-turn.jsonl` is captured from the pinned
`@anthropic-ai/claude-agent-sdk@0.3.226` bridge against the isolated
`toy-project` directory.  The bridge reached a real SDK-managed session and
emitted `init`, assistant, and result messages.  The provider machine had an
expired OAuth session, so the SDK returned an authenticated `authentication_failed`
assistant message and a terminal API-error result instead of model text.  The
fixture is retained as evidence of the real failure path; it is not a mock
success recording.

The capture command starts `bridge/index.ts` with Node's type stripping,
sends `start` and one prompt, waits for the SDK result, then closes the
sidecar.  Absolute paths and plugin paths were replaced with
`<redacted-path>` before committing.  Raw Claude session/message UUIDs remain
inside this adapter-private fixture only and are never copied into canonical
state.

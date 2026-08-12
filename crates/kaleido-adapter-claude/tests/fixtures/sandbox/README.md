# Claude Agent SDK managed-session recording

`real-sdk-simple-turn.jsonl` and `real-sdk-authentication-failure.jsonl` are
captured from the pinned
`@anthropic-ai/claude-agent-sdk@0.3.226` bridge against the isolated
`toy-project` directory. The successful capture contains `init`, a non-error
assistant text block, and a terminal successful result. The rejection capture
contains the SDK's typed `authentication_failed` assistant event and terminal
API error. Their metadata keeps acceptance and rejection evidence distinct.

The capture command starts `bridge/index.ts` with Node's type stripping,
sends `start` and one deterministic prompt, waits for the SDK result, then closes
the sidecar. Absolute paths and plugin paths were replaced with
`<redacted-path>` before committing.  Raw Claude session/message UUIDs remain
inside this adapter-private fixture only and are never copied into canonical
state.

# ADR-0025: QuestionSet attention contract

- Status: accepted
- Date: 2026-08-10
- Scope: UACP 0.5 QuestionSet and answer provenance
- Supersedes: the single-prompt question shape in UACP 0.3
- Related: [ADR-0018](0018-attention-answer-provenance.md), [T-114](../tasks/T-114.md)

## Context

The UACP 0.3 question shape could represent only one prompt, one option and one
free-form body. Claude's structured elicitation surface can send a non-empty set
of prompts, with stable keys, independent single- or multi-select rules and an
optional free-form body for each prompt. Compressing that set into one prompt
loses both questions and the association between a prompt and its answer.

The clean R5 baseline also contains the accepted answer-provenance change from
ADR-0018. A durable answer must distinguish a broker command from an answer
observed in provider traffic or a recorded fixture. Observation evidence does
not identify the external actor; the broker must not label it as a user, policy,
provider or agent without an authenticated local command association.

## Decision

### Version boundary

The wire shape changes in a pre-1.0 minor boundary. `PROTOCOL_VERSION` is
`0.5.0`; `0.5.x` peers are compatible, while `0.3.x`, `0.4.x` and every other
minor line must be refused before decoding business messages. Old durable
records are not silently migrated or guessed into the new shape; loading
remains fail-loud.

The QuestionSet shape also changes the cached `AttentionInbox` projection.
`PROJECTION_VERSION` is therefore `3`; a mobile client reads the version header
before decoding a cached payload and discards older derived projection files.
Pairing credentials and command state are not part of that cache and remain
intact. Treating a v2 single-question payload as v3 is forbidden.

The initial R5 worktree used `0.4.0` while QuestionSet was the only new wire
shape. That version was never merged, released or used as a durable production
format. The same R5 branch subsequently made `AcceptedByRuntime.session_id` and
`acceptance_kind` mandatory so non-Turn commands such as interrupt have
canonical Session scope without fabricating a Turn. `PromptTurn` requires the
same command/session's unique `RemoteCommand` Turn; `SessionControl` represents
interrupt-like structured receipts that do not create a Turn. The final single
compatibility boundary is therefore `0.5.0`, carrying both QuestionSet and
scoped runtime acknowledgement; there is no supported 0.4 line.

### QuestionSet

`QuestionRequest` contains a non-empty `questions: Vec<QuestionPrompt>` and no
single prompt/option fields. Each `QuestionPrompt` contains:

```text
question_key: String
prompt_ref: ContentRef
options: Vec<DecisionOption>
multi_select: bool
free_form_allowed: bool
```

`QuestionAnswer` contains:

```text
question_key: String
option_ids: Vec<String>
free_form_ref: Option<ContentRef>
```

`AttentionResponse` and `AttentionState::Answered` carry
`question_answers: Vec<QuestionAnswer>` in addition to the existing approval /
workflow top-level fields.

For an Approval or WorkflowGate, `question_answers` must be empty and
`option_id`/`free_form_ref` retain their existing semantics. For a Question,
both top-level fields must be empty, answers must contain exactly one entry for
each prompt key, and keys and option IDs must be unique. A single-select prompt
accepts at most one option. An empty answer, unknown key, unknown option,
duplicate key/option, disallowed free-form body or invalid content reference is
rejected before a command is accepted.

Every prompt body and every answer free-form body remains a `ContentRef`; its
text is never put in canonical state, durable logs, projection metadata or
tracing. The state content-reference traversal includes every prompt and every
answer body so retention and read authorization cannot orphan a question.

### Provenance

The accepted ADR-0018 shape is retained unchanged while adding
`question_answers`:

```text
AttentionAnswerSource =
  | LocalCommand { command_id: CommandId }
  | ObservedExternal { evidence: AttentionAnswerEvidence }

AttentionAnswerEvidence = {
  observer_host_id: HostId,
  observed_at_ms: i64,
  source: ObservedInTraffic | RecordedFixture,
}
```

Only a real `CommandEnvelope.command_id` may produce `LocalCommand`. A live or
fixture observation without a local send association uses
`ObservedExternal`; its observer host must match the enclosing attention item.
No provenance variant is allowed to infer the external actor's identity.

## Rejected alternatives

- Keeping one prompt and flattening additional answers would lose prompt keys
  and make a valid provider response ambiguous.
- Making `question_answers` optional or accepting partial sets would permit a
  mobile client to submit a command that silently leaves a prompt unanswered.
- Reusing capability evidence or `Actor` to describe an observed answer would
  overclaim identity and violate ADR-0018 D-2/D-3.
- Treating 0.3/0.4 records as 0.5 with serde defaults would turn a wire/schema
  change into silent data corruption. The compatibility gate therefore rejects
  both earlier minor lines before business decoding.

## Consequences

- Provider adapters map each provider prompt and answer to a canonical key and
  reference; they do not invent a provider-specific UI model.
- Android renders and edits each prompt, then submits one provider-neutral
  command. The Rust core uploads free-form bodies and constructs canonical
  references.
- Approval and workflow behavior, including human decline as a normal item
  terminal state, remains unchanged.
- UniFFI bindings gain `QuestionPrompt`, `QuestionAnswer` and the mobile draft
  input record; generated Kotlin/Swift code must be rebuilt from the canonical
  Rust crate.

# OneKaleidoscope

[English](README.md) | [简体中文](README_ZH.md)

Continue controlling AI coding agents on your PC after you leave your desk.

> [!WARNING]
> OneKaleidoscope is under active development and is **not a finished product**.
> The repository currently contains a verified local Rust vertical slice and protocol work—not installable Android/iOS apps or a production-ready remote service. APIs, storage formats, and behavior may change while the project iterates.

OneKaleidoscope is a control plane for AI coding agents, not a terminal mirror. A PC-side session broker connects to providers through their public structured protocols, reduces provider-specific messages into canonical state, and exposes mobile-oriented read models and commands. The target product supports Codex, Claude Code, and OpenCode, including durable sessions, human approvals, queued input, remote recovery, and cross-agent workflows.

Agent data must never be obtained by scraping a terminal, parsing ANSI/TUI output, taking screenshots, using OCR, or polling transcripts and presenting them as live state.

## Current status

R0 (documentation baseline), R1 (canonical contract), and R2 (single-provider local slice) are complete. The project is finishing the protocol work required before R3, the first Android-over-LAN slice. See [docs/STATUS.md](docs/STATUS.md) for the progress source of truth and [docs/MILESTONES.md](docs/MILESTONES.md) for the delivery sequence.

Implemented and backed by repository evidence:

- Canonical protocol v0.1 for sessions, turns, items, commands, attention, queues, capabilities, projections, errors, and workflows.
- A provider-neutral runtime-session abstraction.
- A Codex app-server adapter using pinned JSON Pointer decoding, a reducer, and stdio JSON-RPC process transport.
- A local end-to-end path from real or recorded Codex app-server messages through canonical state and an append-only durable log to six read models: session index, transcript, live activity, input queue, attention inbox, and runtime capabilities.
- Diagnostic commands for live runs, fixture replay, restart recovery, and projection inspection.
- Content-addressed storage for sensitive payloads, idempotent local commands, and logging/redaction checks.
- Codex app-server compatibility evidence for the continuous `0.146.0`–`0.147.0` range.
- UniFFI API-shape probes compiled from Kotlin and Swift for callbacks, objects, async calls, and throwing calls. These are binding probes, not mobile application implementations.
- Rust CI on Windows, macOS, and Linux, plus Kotlin and Swift consumer compile gates.

Not yet fully implemented:

- Android and iOS applications.
- LAN and internet transport, pairing, end-to-end encryption, P2P connectivity, and the ciphertext relay.
- Production adapters for Claude Code and OpenCode, and the ACP compatibility path.
- Cross-agent workflow scheduling and the planned Claude → Codex → Claude workflow.
- Proven live attachment across every native CLI and GUI surface.
- Mobile-delivered steer, complete live-control semantics, file/code browsing, Git operations, packaging, and release hardening.

Some upstream native GUI/CLI surfaces do not currently expose a stable public third-party attachment contract. Those gaps remain explicit; the project does not replace them with terminal scraping or claim historical access as live control.

## Architecture

```text
Codex / Claude Code / OpenCode public structured protocols
                              │
                              ▼
                      PC Session Broker
          decoder → reducer → canonical state → durable log
                              │
                   projections and commands
                              │
                  LAN / P2P / encrypted relay     (planned)
                              │
                              ▼
                       Android / iOS               (planned)
```

The core rules are:

- Historical access and live runtime control are separate capabilities.
- Provider messages are reduced into canonical state before reaching UI projections.
- State can be rebuilt from a snapshot plus the durable log after its cursor.
- Capabilities belong to a concrete runtime connection; clients must not branch on provider names.
- Servers coordinate and relay ciphertext only. They must not hold provider credentials, project files, or business plaintext.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full model and [docs/PROTOCOL.md](docs/PROTOCOL.md) for the canonical contract.

## Repository layout

| Path | Purpose |
|---|---|
| `crates/kaleido-proto` | Canonical types, commands, validation, and the UniFFI-exportable contract |
| `crates/kaleido-state` | Canonical state, durable log, content store, command handling, and six implemented projections |
| `crates/kaleido-adapter` | Provider-neutral runtime traits, identities, content access, and capability evidence |
| `crates/kaleido-adapter-codex` | Codex app-server decoder, reducer, runtime process transport, and drift guards |
| `crates/kaleido-hostd` | Composition root and the current `slice run/replay/show` diagnostic CLI |
| `crates/kaleido-core` | Minimal UniFFI façade and Kotlin/Swift consumer probes; not the mobile product runtime yet |
| `schemas` | Byte-preserving upstream schema snapshots, required-surface ownership, and drift history |
| `tests/fixtures` | Real recorded structured-protocol evidence used by contract and reducer tests |
| `xtask` | The repository's local CI, dependency, fixture, and schema tooling |
| `spikes` | Frozen research assets; not the active product architecture |

## Quick start

Install [Rustup](https://rustup.rs/) and run commands from the repository root. `rust-toolchain.toml` selects Rust 1.94.0 with the required components.

Build the workspace:

```text
cargo build --workspace --locked
```

Replay a committed, real Codex fixture into a fresh diagnostic log and inspect all implemented projections:

```text
cargo run -p kaleido-hostd -- slice replay --fixture tests/fixtures/codex/01-simple-turn.jsonl --log-dir target/kaleido-demo
cargo run -p kaleido-hostd -- slice show --log-dir target/kaleido-demo --projection all
```

The current live diagnostic path can launch a Codex app-server session when given the native executable and a project directory:

```text
cargo run -p kaleido-hostd -- slice run --executable <path-to-codex-executable> --project-root <project-directory> --log-dir target/kaleido-live --prompt "Inspect this project and summarize it"
```

This is a development diagnostic interface, not a stable end-user CLI. It can submit a prompt and answer the first supported file-change approval with optional flags. A requested steer is deliberately kept in the broker queue unless runtime delivery has been observed; queued input must not be presented as delivered control.

## Development

The single local gate is:

```text
cargo xtask ci
```

It checks formatting, dependency boundaries, forbidden patterns, Clippy with warnings denied, workspace tests, and fixture integrity. Schema drift is separate because it invokes installed upstream tools and accesses the network:

```text
cargo xtask schema diff
```

Before changing the project, read [CLAUDE.md](CLAUDE.md), [AGENTS.md](AGENTS.md), and the current next step in [docs/STATUS.md](docs/STATUS.md). Protocol changes must update the protocol text, canonical Rust types, dependent code, tests, and an ADR together. Tests must cover a rejection or error path and provide assertions that can actually fail.

Useful references:

- [Requirements](docs/REQUIREMENTS.md)
- [Current status](docs/STATUS.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Protocol](docs/PROTOCOL.md)
- [Milestones](docs/MILESTONES.md)
- [Development guide](docs/DEVELOPMENT.md)
- [Upstream compatibility](docs/UPSTREAM.md)

## License

This repository is currently marked `UNLICENSED` and does not include an open-source license.

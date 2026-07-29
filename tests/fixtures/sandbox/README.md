# Fixture recording sandbox

This directory is the only project that `kaleido-recorder` may expose to an
agent. It contains no production code, credentials, or user data.

Recording scenarios use these deterministic targets:

- read and summarize `notes.txt`;
- replace `ORIGINAL` with `CHANGED` in `editable.txt`;
- run `cargo run -- fail` for a deterministic non-zero command;
- run `cargo run -- wait` and cancel the turn while the process is waiting.

Reset `editable.txt` to its committed contents before every file-change or
permission scenario.

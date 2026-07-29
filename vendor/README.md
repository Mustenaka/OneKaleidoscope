# Offline Rust sources

This directory contains unmodified upstream source needed to build the T-004
recorder in the repository's network-restricted verification environment.

| Crate | Exact version | Upstream source |
|---|---:|---|
| `directories` | `6.0.0` | `dirs-dev/directories-rs` |
| `dirs-sys` | `0.5.0` | `dirs-dev/dirs-sys-rs` |
| `option-ext` | `0.2.0` | `soc/option-ext` |

The root manifest uses Cargo's `[patch.crates-io]` mechanism, so dependency
declarations retain their exact registry versions while resolution uses these
checked-in sources offline.

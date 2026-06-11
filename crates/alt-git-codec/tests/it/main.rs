//! Single integration-test binary for alt-git-codec (one link, one process).
//! Add new integration modules here instead of new files under `tests/`.

mod common;
mod corpus_sweep;
mod loose_git_interop;
mod object_roundtrip;

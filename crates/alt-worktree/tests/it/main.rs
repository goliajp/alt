//! Single integration-test binary for alt-worktree.

#[cfg(unix)]
mod delta;
mod ignore_scan;
mod status_interop;
#[cfg(unix)]
mod untracked;

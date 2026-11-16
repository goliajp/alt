//! Single integration-test binary for alt-worktree.

#[cfg(unix)]
mod delta;
mod ignore_scan;
mod status_interop;
#[cfg(unix)]
mod untracked;
#[cfg(target_os = "linux")]
mod watch;
#[cfg(target_os = "linux")]
mod watch_inputs;
#[cfg(target_os = "linux")]
mod watch_loss;
#[cfg(target_os = "linux")]
mod watch_mounts;

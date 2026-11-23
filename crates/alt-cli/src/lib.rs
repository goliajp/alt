//! The alt CLI's reusable core: the native `.alt` commands, structured-output
//! helpers, and the local daemon protocol. Both the `alt` binary (client) and
//! the `altd` binary (daemon) build on this, so command logic and the wire
//! protocol live here, not in either binary.

/// A point a debug build can be made to die at; nothing in a release build.
macro_rules! crash_point {
    ($name:expr) => {
        #[cfg(debug_assertions)]
        $crate::crash::point($name);
    };
}

pub mod added_commits;
pub mod auth;
pub mod blame;
pub mod charset;
pub mod ci;
pub mod ci_cmd;
pub mod cli;
pub mod client;
mod combined;
pub mod commit_sign;
pub mod complete;
pub mod config_cmd;
#[cfg(debug_assertions)]
mod crash;
pub mod daemon;
pub mod daemon_meta;
pub mod diagnostics;
mod diffstat;
pub mod fsck;
pub mod group_commit;
pub mod hardening;
pub mod held;
pub mod index_tx;
pub mod json;
mod log2;
pub mod log_cmd;
pub mod log_count;
mod log_format;
mod log_range;
pub mod machine;
pub mod meta;
pub mod native;
pub mod past_damage;
mod patch;
pub mod patterns;
pub mod policy;
pub mod portable;
pub mod precommit;
pub mod prompt;
pub mod quote;
pub mod receive;
pub mod request;
pub mod rev_parse;
pub mod runner;
pub mod sequence_args;
pub mod shell;
mod tree_changes;
pub mod workers;
pub mod worktree_args;

pub mod execution;
mod file_header;
pub mod funcname;
mod git_settings;
/// For a thread that runs one command after another, before each: what
/// the one before read of the configuration is not what this one goes by.
pub use git_settings::forget as begin_command;

pub mod durable;

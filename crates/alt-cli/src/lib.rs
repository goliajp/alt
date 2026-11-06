//! The alt CLI's reusable core: the native `.alt` commands, structured-output
//! helpers, and the local daemon protocol. Both the `alt` binary (client) and
//! the `altd` binary (daemon) build on this, so command logic and the wire
//! protocol live here, not in either binary.

pub mod auth;
pub mod blame;
pub mod ci;
pub mod cli;
pub mod client;
pub mod commit_sign;
pub mod config_cmd;
pub mod daemon;
pub mod diagnostics;
pub mod group_commit;
pub mod index_tx;
pub mod json;
pub mod log_cmd;
pub mod machine;
pub mod meta;
pub mod native;
pub mod policy;
pub mod portable;
pub mod precommit;
pub mod quote;
pub mod request;
pub mod runner;

pub mod execution;

pub mod durable;

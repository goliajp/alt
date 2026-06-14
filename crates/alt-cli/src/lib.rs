//! The alt CLI's reusable core: the native `.alt` commands, structured-output
//! helpers, and the local daemon protocol. Both the `alt` binary (client) and
//! the `altd` binary (daemon) build on this, so command logic and the wire
//! protocol live here, not in either binary.

pub mod daemon;
pub mod json;
pub mod log_cmd;
pub mod native;
pub mod quote;

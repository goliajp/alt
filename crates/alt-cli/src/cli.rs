//! The command surface, shared by the `alt` binary (direct) and the `altd`
//! daemon. The argument types and the per-command dispatch live here so both reuse
//! the exact same command logic — the daemon is a perf cache, not a fork of
//! behaviour. Native commands run against a [`NativeRepo`] bound to a [`Store`];
//! git-layer commands (`cat-file`/`rev-parse`/`log`/`import`/`export`) open
//! their own [`Repository`].

use alt_args::Parser;

mod classify;
mod command;
mod git;
mod native;
mod platform;
mod prepare;
mod task;
mod vcs;

pub use classify::{is_native, mutates_command};
pub use command::Command;
pub use git::{CatFileArgs, run_git, run_on_store};
pub use native::run_native;
pub use platform::{AuthOp, OperationOp, PortableOp, PortableOptions, RequestOp, RunnerOp};
pub use task::{CandidateOp, ChangeOp, EvidenceOp, TaskOp, TaskRemoteOp};
pub use vcs::{
    BisectOp, CiOp, FlowOp, FlowTopicOp, IdentityOp, LfsOp, MetaOp, RemoteOp, WorkspaceOp,
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(name = "alt", version, disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
    /// Operate in the named parallel workspace instead of the default one
    #[arg(long, global = true)]
    pub workspace: Option<String>,
    /// Stable request identity for a write in an existing native repository
    #[arg(long, global = true, value_name = "HEX")]
    pub request_id: Option<String>,
}

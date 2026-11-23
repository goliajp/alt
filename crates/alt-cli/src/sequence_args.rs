//! The arguments of the commands that bring commits of another line onto
//! the current branch. Each type has the name of its command and each
//! field the name a durable request records it under.

use crate::native::FastForward;

#[derive(alt_args::Args)]
pub struct CherryPick {
    /// Revision of the commit to apply
    #[arg(required_unless_present_any = ["continue_pick", "abort_pick"], hint = "rev")]
    pub rev: Option<String>,
    /// Commit the resolved cherry-pick
    #[arg(long = "continue", conflicts_with_all = ["rev", "abort_pick"])]
    pub continue_pick: bool,
    /// Put the index and the files back as they were before the cherry-pick
    #[arg(long = "abort", conflicts_with = "rev")]
    pub abort_pick: bool,
    /// Emit a structured JSON result instead of human lines.
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Merge {
    /// Branch to merge into the current branch
    #[arg(required_unless_present = "abort_merge", hint = "branch")]
    pub branch: Option<String>,
    /// Put the index and the files back as they were before the merge
    #[arg(long = "abort", conflicts_with = "branch")]
    pub abort_merge: bool,
    /// Record a merge commit even where the branch could be fast-forwarded
    #[arg(long = "no-ff", conflicts_with_all = ["ff_only", "abort_merge"])]
    pub no_ff: bool,
    /// Fast-forward or change nothing: refuse a merge that needs a commit
    #[arg(long = "ff-only", conflicts_with = "abort_merge")]
    pub ff_only: bool,
    /// Emit a structured JSON result instead of the human lines
    #[arg(long)]
    pub json: bool,
}

impl Merge {
    pub fn fast_forward(&self) -> FastForward {
        match (self.no_ff, self.ff_only) {
            (true, _) => FastForward::Never,
            (false, true) => FastForward::Only,
            (false, false) => FastForward::Allowed,
        }
    }
}

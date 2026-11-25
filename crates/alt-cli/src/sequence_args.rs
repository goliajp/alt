//! The arguments of the commands that bring commits of another line onto
//! the current branch. Each type has the name of its command and each
//! field the name a durable request records it under.

use crate::native::FastForward;

#[derive(alt_args::Args)]
pub struct CherryPick {
    /// Revision of the commit to apply
    #[arg(required_unless_present_any = ["continue_pick", "abort_pick", "skip_pick", "quit_pick"], hint = "rev")]
    pub rev: Option<String>,
    /// Commit the resolved cherry-pick
    #[arg(long = "continue", conflicts_with_all = ["rev", "abort_pick"])]
    pub continue_pick: bool,
    /// Put the index and the files back as they were before the cherry-pick
    #[arg(long = "abort", conflicts_with = "rev")]
    pub abort_pick: bool,
    /// Give the stopped pick up and keep what else is in the files
    #[arg(long = "skip", conflicts_with_all = ["rev", "continue_pick", "abort_pick"])]
    pub skip_pick: bool,
    /// Forget the stopped pick; the index and the files stay as they are
    #[arg(long = "quit", conflicts_with_all = ["rev", "continue_pick", "abort_pick", "skip_pick"])]
    pub quit_pick: bool,
    /// End the message with a line that names the commit picked
    #[arg(short = 'x', conflicts_with_all = ["continue_pick", "abort_pick"])]
    pub noted: bool,
    /// Emit a structured JSON result instead of human lines.
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Revert {
    /// Commits to revert, in order
    #[arg(required_unless_present_any = ["cont", "abort", "skip_revert", "quit_revert"], hint = "rev")]
    pub revs: Vec<String>,
    /// For a merge commit, the parent (1-based) whose side is kept
    #[arg(short = 'm', long = "mainline", hint = none)]
    pub mainline: Option<usize>,
    /// Commit the resolved revert and go on with the rest
    #[arg(long = "continue", id = "cont", conflicts_with_all = ["revs", "abort", "mainline"])]
    pub cont: bool,
    /// Put the branch back where it was before the reverts
    #[arg(long, conflicts_with_all = ["revs", "mainline"])]
    pub abort: bool,
    /// Leave the stopped revert out and go on with the rest
    #[arg(long = "skip", conflicts_with_all = ["revs", "mainline", "cont", "abort"])]
    pub skip_revert: bool,
    /// Forget the sequence; the branch, the index and the files stay
    #[arg(long = "quit", conflicts_with_all = ["revs", "mainline", "cont", "abort", "skip_revert"])]
    pub quit_revert: bool,
    /// Use the generated message (alt never opens an editor; accepted as in git)
    #[arg(long)]
    pub no_edit: bool,
    /// Emit a structured JSON result instead of human lines.
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Merge {
    /// Branch, or any other commit, to merge into the current branch
    #[arg(required_unless_present_any = ["abort_merge", "continue_merge", "quit_merge"], hint = "branch")]
    pub branch: Option<String>,
    /// Put the index and the files back as they were before the merge
    #[arg(long = "abort", conflicts_with = "branch")]
    pub abort_merge: bool,
    /// Commit the merge whose conflicts are resolved
    #[arg(long = "continue", conflicts_with_all = ["branch", "abort_merge"])]
    pub continue_merge: bool,
    /// Forget the stopped merge; the index and the files stay as they are
    #[arg(long = "quit", conflicts_with_all = ["branch", "abort_merge", "continue_merge"])]
    pub quit_merge: bool,
    /// Record a merge commit even where the branch could be fast-forwarded
    #[arg(long = "no-ff", conflicts_with_all = ["ff_only", "abort_merge"])]
    pub no_ff: bool,
    /// Fast-forward or change nothing: refuse a merge that needs a commit
    #[arg(long = "ff-only", conflicts_with = "abort_merge")]
    pub ff_only: bool,
    /// Fast-forward where the branch is behind, whatever `merge.ff` says
    #[arg(long = "ff", conflicts_with_all = ["no_ff", "ff_only", "abort_merge"])]
    pub ff: bool,
    /// Take the message of the merge commit as it is (alt never opens an
    /// editor, so this is already the default; accepted as in git)
    #[arg(long = "no-edit")]
    pub as_it_is: bool,
    /// Emit a structured JSON result instead of the human lines
    #[arg(long)]
    pub json: bool,
}

impl Merge {
    /// What the command line says of a fast-forward; `None` where it
    /// says nothing, and `merge.ff` decides.
    pub fn fast_forward(&self) -> Option<FastForward> {
        match (self.no_ff, self.ff_only, self.ff) {
            (true, ..) => Some(FastForward::Never),
            (_, true, _) => Some(FastForward::Only),
            (.., true) => Some(FastForward::Allowed),
            _ => None,
        }
    }
}

#[derive(alt_args::Args)]
pub struct Tag {
    /// Tag name to create. Omit (and omit `-d`) to list all tags.
    #[arg(hint = none)]
    pub name: Option<String>,
    /// Revision the new tag should point to (default HEAD). Only
    /// honoured when creating a tag.
    #[arg(hint = "rev")]
    pub rev: Option<String>,
    /// Delete a tag by name. Mutually exclusive with `name`.
    #[arg(short = 'd', long, hint = "tag")]
    pub delete: Option<String>,
    /// Create an *annotated* tag (writes a tag object carrying the
    /// message + tagger ident, instead of a bare ref).
    #[arg(short = 'a', long)]
    pub annotate: bool,
    /// Annotated-tag message. Required when `-a` is set. Given more
    /// than once, each is a paragraph of its own.
    #[arg(short = 'm', long, hint = none)]
    pub message: Vec<String>,
    /// JSON output for list mode (ignored otherwise).
    #[arg(long)]
    pub json: bool,
}

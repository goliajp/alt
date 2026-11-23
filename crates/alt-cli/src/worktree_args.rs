//! The arguments of the commands that look at the working tree. Each
//! type has the name of its command and each field the name it had in the
//! command itself: both are part of what a durable request records.

#[derive(alt_args::Args)]
pub struct Add {
    /// Paths to stage; `.` stages the directory the command runs in
    #[arg(hint = "path-add")]
    pub paths: Vec<String>,
    /// Store every tracked file among the paths again, as the
    /// attributes and line-ending settings now in force clean it
    #[arg(long)]
    pub renormalize: bool,
    /// Stage files that an ignore rule hides as well
    #[arg(short = 'f', long = "force")]
    pub forced: bool,
    /// Stage what changed in the files the index has, and no new file:
    /// in the whole tree when no path is given
    #[arg(short = 'u', long)]
    pub update: bool,
    /// Stage every change, new files as well: in the whole tree when no
    /// path is given
    #[arg(short = 'A', long)]
    pub all: bool,
    /// Emit a structured JSON result instead of the human line
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Status {
    /// Which untracked files to show: `no`, `normal` (a directory with
    /// nothing tracked in it as one path) or `all`, or a boolean for
    /// `normal` or `no`; `all` when the option is given without a mode,
    /// and without the option what `status.showUntrackedFiles` sets
    #[arg(
        short = 'u',
        long = "untracked-files",
        value_name = "MODE",
        default_missing_value = "all",
        hint = none,
    )]
    pub untracked_files: Option<String>,
    /// Emit a stable JSON object instead of the human-readable view
    #[arg(long)]
    pub json: bool,
}

impl Status {
    pub fn untracked(&self) -> Option<&str> {
        self.untracked_files.as_deref()
    }
}

#[derive(alt_args::Args)]
pub struct Restore {
    /// Paths to restore; `.` is the directory the command runs in
    #[arg(hint = "path-tracked")]
    pub paths: Vec<String>,
    /// Restore the index, from HEAD unless a source is given
    #[arg(short = 'S', long)]
    pub staged: bool,
    /// Restore the files; what is restored where --staged is not given
    #[arg(short = 'W', long)]
    pub worktree: bool,
    /// The commit to restore from: the index for the files alone, else
    /// HEAD, where none is given
    #[arg(short = 's', long, value_name = "REV", hint = "rev")]
    pub source: Option<String>,
    /// For a path in conflict, restore the file of the current branch
    #[arg(long, conflicts_with = "theirs")]
    pub ours: bool,
    /// For a path in conflict, restore the file of the branch merged in
    #[arg(long)]
    pub theirs: bool,
    /// Emit a structured JSON result instead of printing nothing
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Checkout {
    /// A branch to switch to, or paths to restore from the index; a
    /// revision before the paths restores them from it, index and files
    #[arg(hint = "path-tracked")]
    pub names: Vec<String>,
    /// Paths to restore, where a name before them could be taken for
    /// a branch or a revision
    #[arg(last = true, hint = "path-tracked")]
    pub paths: Vec<String>,
    /// Create the branch named before switching to it
    #[arg(short = 'b')]
    pub create: bool,
    /// For a path in conflict, check out the file of the current branch
    #[arg(long, conflicts_with = "theirs")]
    pub ours: bool,
    /// For a path in conflict, check out the file of the branch merged in
    #[arg(long)]
    pub theirs: bool,
    /// Emit a structured JSON result instead of the human line
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Reset {
    /// The commit to reset to, HEAD where none is given, and paths: with
    /// paths, their index entries are reset and nothing else
    #[arg(hint = "rev")]
    pub names: Vec<String>,
    /// Paths, where a name before them could be taken for a revision
    #[arg(last = true, hint = "path-tracked")]
    pub paths: Vec<String>,
    /// Move the branch and leave the index and the files as they are
    #[arg(long, conflicts_with_all = ["mixed", "hard"])]
    pub soft: bool,
    /// Move the branch and reset the index; the files stay (the default)
    #[arg(long, conflicts_with = "hard")]
    pub mixed: bool,
    /// Move the branch and reset the index and the files
    #[arg(long)]
    pub hard: bool,
    /// Say nothing of what is left unstaged or where HEAD is
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Emit a structured JSON result instead of the human lines
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Rm {
    /// Files to remove; a directory only with -r
    #[arg(hint = "path-tracked")]
    pub paths: Vec<String>,
    /// Remove the index entries and leave the files
    #[arg(long)]
    pub cached: bool,
    /// Remove what lies beneath a directory given
    #[arg(short = 'r')]
    pub recursive: bool,
    /// Remove a file whose changes are nowhere else as well
    #[arg(short = 'f', long = "force")]
    pub forced: bool,
    /// Pass over a path that names no file of the index
    #[arg(long = "ignore-unmatch")]
    pub ignore_unmatch: bool,
    /// Say what would be removed and remove nothing
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
    /// Do not say what is removed
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Emit a structured JSON result instead of the human lines
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Mv {
    /// The file or directory to rename and its new name, or what to move
    /// and the directory to move it into
    #[arg(hint = "path-tracked")]
    pub paths: Vec<String>,
    /// Write over a file at the new name
    #[arg(short = 'f', long = "force")]
    pub forced: bool,
    /// Pass over a source that cannot be moved
    #[arg(short = 'k')]
    pub skip_errors: bool,
    /// Say what would be renamed and rename nothing
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
    /// Say each rename
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Emit a structured JSON result instead of the human lines
    #[arg(long)]
    pub json: bool,
}

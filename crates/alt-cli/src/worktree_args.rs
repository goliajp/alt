//! The arguments of the commands that look at the working tree. Each
//! type has the name of its command and each field the name it had in the
//! command itself: both are part of what a durable request records.

#[derive(alt_args::Args)]
pub struct Add {
    /// Paths to stage; `.` stages the directory the command runs in
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
        default_missing_value = "all"
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

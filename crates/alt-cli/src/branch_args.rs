//! The arguments of `alt branch` and `alt switch`.

#[derive(alt_args::Args)]
pub struct Branch {
    /// Branch name to create (omit to list branches)
    #[arg(hint = none)]
    pub name: Option<String>,
    /// Where the new branch starts: a branch, a tag or a commit (HEAD
    /// where none is given)
    #[arg(hint = "rev")]
    pub start: Option<String>,
    /// Delete the named branch
    #[arg(short = 'd', hint = "branch-delete")]
    pub delete: Option<String>,
    /// With -d, the branch is a remote-tracking one (`<remote>/<branch>`)
    #[arg(short = 'r', long = "remotes", requires = "delete")]
    pub remotes: bool,
    /// Emit the branch list as a stable JSON object
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Switch {
    /// Branch to switch to
    #[arg(hint = "branch")]
    pub name: String,
    /// With -c, where the new branch starts: a branch, a tag or a
    /// commit (the current commit where none is given)
    #[arg(hint = "rev")]
    pub start: Option<String>,
    /// Create the branch before switching
    #[arg(short = 'c')]
    pub create: bool,
    /// Emit a structured JSON result instead of the human line
    #[arg(long)]
    pub json: bool,
}

//! The arguments of the commands that look at the working tree.

#[derive(alt_args::Args)]
pub struct AddArgs {
    /// Paths to stage; `.` stages the whole working tree
    pub paths: Vec<String>,
    /// Store every tracked file among the paths again, as the
    /// attributes and line-ending settings now in force clean it
    #[arg(long)]
    pub renormalize: bool,
    /// Emit a structured JSON result instead of the human line
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct StatusArgs {
    /// Which untracked files to show: `no`, `normal` (a directory with
    /// nothing tracked in it as one path) or `all`; `all` when the
    /// option is given without a mode, `normal` without the option
    #[arg(
        short = 'u',
        long = "untracked-files",
        value_name = "MODE",
        default_missing_value = "all",
        value_parser = ["no", "normal", "all"]
    )]
    pub untracked: Option<String>,
    /// Emit a stable JSON object instead of the human-readable view
    #[arg(long)]
    pub json: bool,
}

//! The arguments of `alt commit` and `alt rebase`. Each type has the name
//! of its command and each field the name a durable request records it
//! under.

pub fn parse_key_value(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| format!("expected key=value, got '{s}'"))
}

#[derive(alt_args::Args)]
pub struct Commit {
    /// Commit message (with --amend, defaults to the amended commit's)
    #[arg(short = 'm', required_unless_present_any = ["amend", "validate"], hint = none)]
    pub message: Option<String>,
    /// Replace the current branch tip with a new commit made from the
    /// index, keeping its parents and author
    #[arg(long)]
    pub amend: bool,
    /// Keep the amended commit's message (alt never opens an editor, so
    /// this is already the default without -m; accepted as in git)
    #[arg(long, requires = "amend")]
    pub no_edit: bool,
    /// Record the commit even when it changes nothing
    #[arg(long)]
    pub allow_empty: bool,
    /// Skip the pre-commit checks (secret scan, large-file warning)
    #[arg(short = 'n', long)]
    pub no_verify: bool,
    /// Only run the pre-commit checks on what is staged; commit nothing
    #[arg(long, conflicts_with_all = ["amend", "message", "no_verify"])]
    pub validate: bool,
    /// Record a one-line decision in the commit's metadata (up to 3)
    #[arg(long = "decision", value_name = "TEXT", hint = none)]
    pub decisions: Vec<String>,
    /// Record an extra metadata field (`key=value`, e.g. `model=…`)
    #[arg(long = "meta", value_name = "KEY=VALUE", value_parser = parse_key_value, hint = none)]
    pub extra: Vec<(String, String)>,
    /// Whom the commit is by: `Name <email>`, or a text that the author
    /// of an earlier commit holds
    #[arg(long = "author", value_name = "AUTHOR", hint = none)]
    pub author_given: Option<String>,
    /// When the commit was authored
    #[arg(long = "date", value_name = "DATE", hint = none)]
    pub author_date: Option<String>,
    /// With --amend, the commit is by whoever amends it, as of now
    #[arg(long = "reset-author")]
    pub reset_author: bool,
    /// Emit the new commit/tree oids (or, with --validate, the findings)
    /// as a JSON object
    #[arg(long)]
    pub json: bool,
}

#[derive(alt_args::Args)]
pub struct Rebase {
    /// The base to replay onto
    #[arg(required_unless_present_any = ["cont", "skip", "abort"], hint = "rev")]
    pub upstream: Option<String>,
    /// Edit the list of commits first (pick, reword, squash, fixup,
    /// drop) in $GIT_SEQUENCE_EDITOR, $GIT_EDITOR, $VISUAL or $EDITOR
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Commit the resolved step and go on
    #[arg(long = "continue", id = "cont", conflicts_with_all = ["upstream", "skip", "abort"])]
    pub cont: bool,
    /// Drop the stopped step and go on
    #[arg(long, conflicts_with_all = ["upstream", "abort"])]
    pub skip: bool,
    /// Put the branch back as it was before the rebase
    #[arg(long, conflicts_with = "upstream")]
    pub abort: bool,
    /// Run a shell command after each commit replayed; the rebase stops
    /// where one fails
    #[arg(short = 'x', long = "exec", value_name = "COMMAND", requires = "upstream", hint = none)]
    pub exec: Vec<String>,
    /// Replay the merge commits too, each made again of the commits
    /// replayed, instead of leaving them out
    #[arg(short = 'r', long = "rebase-merges", requires = "upstream")]
    pub rebase_merges: bool,
    /// Give each commit replayed the moment of the rebase as its
    /// author's date
    #[arg(long = "reset-author-date", requires = "upstream")]
    pub reset_author_date: bool,
    /// Emit a structured JSON result instead of human lines.
    #[arg(long)]
    pub json: bool,
}

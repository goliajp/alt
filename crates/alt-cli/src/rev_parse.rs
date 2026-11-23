//! `alt rev-parse`: what a revision names, and where the repository is.

use std::io::Write;

use alt_git_codec::ObjectId;
use alt_repo::Repository;

use crate::native::pathspec::Place;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(alt_args::Args)]
pub struct RevParse {
    /// Revisions to resolve: an id, a ref, `HEAD~2`, `HEAD^2`,
    /// `v1^{commit}`, `@{u}`, `<rev>:<path>`, or `:<path>` and
    /// `:<n>:<path>` for what the index has, at stage n
    #[arg(hint = "rev")]
    pub rev: Vec<String>,
    /// Print the root of the working tree
    #[arg(long = "show-toplevel")]
    pub show_toplevel: bool,
    /// Print the directory the repository is kept in: `.alt` from the
    /// root of its tree, else in full
    #[arg(long = "git-dir", id = "show_git_dir")]
    pub git_dir: bool,
    /// Print the directory the repository is kept in, in full
    #[arg(long = "absolute-git-dir")]
    pub absolute_git_dir: bool,
    /// Print where the command runs, from the root of the tree
    #[arg(long = "show-prefix")]
    pub show_prefix: bool,
    /// Print `true`: the command runs in a working tree
    #[arg(long = "is-inside-work-tree")]
    pub is_inside_work_tree: bool,
    /// Print the short name of the branch or ref a revision is, where it
    /// is one: `main` for `HEAD`, `origin/main` for `@{u}`
    #[arg(long = "abbrev-ref")]
    pub abbrev_ref: bool,
    /// Print the full name of the ref a revision is, where it is one
    #[arg(long = "symbolic-full-name")]
    pub symbolic_full_name: bool,
    /// Print ids shortened to as many digits as tell them apart, at
    /// least LENGTH of them
    #[arg(long, value_name = "LENGTH", default_missing_value = "0", hint = none)]
    pub short: Option<usize>,
    /// Take one revision and fail where it names no object
    #[arg(long = "verify", id = "verify_one")]
    pub verify: bool,
    /// With --verify, say nothing where the revision names no object
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

fn fatal(said: String) -> Box<dyn std::error::Error> {
    crate::git_settings::fatal(said)
}

/// git's words for a revision that names nothing.
fn unknown(spec: &str) -> Box<dyn std::error::Error> {
    fatal(format!(
        "ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    ))
}

/// The index of the workspace whose HEAD is the ref `head`.
fn index_of(repo: &Repository, head: &str) -> Res<alt_git_index::Index> {
    let dir = match head.strip_suffix("/HEAD") {
        Some(workspace) => repo.git_dir().join(workspace),
        None => repo.git_dir().to_owned(),
    };
    match std::fs::read(dir.join("index")) {
        Ok(bytes) => Ok(alt_git_index::Index::parse(&bytes, repo.algo()?)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(alt_git_index::Index {
            version: 2,
            entries: Vec::new(),
            extensions: Vec::new(),
        }),
        Err(e) => Err(e.into()),
    }
}

/// What the index has at `:<path>` or `:<n>:<path>`; `None` where `spec`
/// is neither.
fn staged(repo: &Repository, at: &Place, spec: &str) -> Res<Option<ObjectId>> {
    let Some(rest) = spec.strip_prefix(':') else {
        return Ok(None);
    };
    let (stage, path) = match rest.split_once(':') {
        Some((stage @ ("0" | "1" | "2" | "3"), path)) => (stage.parse::<u8>()?, path),
        _ => (0, rest),
    };
    let path = match path.starts_with("./") || path.starts_with("../") {
        true => at.path(path)?,
        false => path.to_owned(),
    };
    let index = index_of(repo, &at.head)?;
    let at_path = |e: &&alt_git_index::IndexEntry| e.path.as_slice() == path.as_bytes();
    let stages: Vec<_> = index.entries.iter().filter(at_path).collect();
    match stages.iter().find(|e| e.stage() == stage) {
        Some(entry) => Ok(Some(entry.oid)),
        None if stages.is_empty() => Err(fatal(format!(
            "path '{path}' does not exist (neither on disk nor in the index)"
        ))),
        None => Err(fatal(format!(
            "path '{path}' is in the index, but not at stage {stage}"
        ))),
    }
}

/// The full name of the ref `spec` is, where it is one: a symbolic ref
/// for the branch it names, an upstream for the branch that is followed.
fn full_name(repo: &Repository, at: &Place, spec: &str) -> Res<Option<String>> {
    let mapped = at.at_head(spec);
    if let Some(named) = ["@{u}", "@{upstream}"]
        .iter()
        .find_map(|u| mapped.strip_suffix(u))
    {
        return Ok(repo.upstream_of(named)?);
    }
    if let Some(branch) = repo.branch_of(&mapped)? {
        return Ok(Some(format!("refs/heads/{branch}")));
    }
    let candidates = [
        mapped.clone(),
        format!("refs/{mapped}"),
        format!("refs/tags/{mapped}"),
        format!("refs/heads/{mapped}"),
        format!("refs/remotes/{mapped}"),
    ];
    for name in candidates
        .into_iter()
        .filter(|name| name.starts_with("refs/"))
    {
        if repo.resolve_ref(&name)?.is_some() {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

impl RevParse {
    /// The lines about where the repository is.
    fn places(&self, out: &mut impl Write, repo: &Repository, at: &Place) -> Res<()> {
        if self.show_toplevel {
            writeln!(out, "{}", at.root.display())?;
        }
        let kept = repo.git_dir();
        if self.git_dir {
            let here = at.prefix.is_empty() && kept.parent() == Some(at.root.as_path());
            match (here, kept.file_name()) {
                (true, Some(name)) => writeln!(out, "{}", name.to_string_lossy())?,
                _ => writeln!(out, "{}", kept.display())?,
            }
        }
        if self.absolute_git_dir {
            writeln!(out, "{}", kept.display())?;
        }
        if self.show_prefix {
            let slash = if at.prefix.is_empty() { "" } else { "/" };
            writeln!(out, "{}{slash}", at.prefix)?;
        }
        if self.is_inside_work_tree {
            writeln!(out, "true")?;
        }
        Ok(())
    }

    /// The object `spec` names.
    fn resolve(&self, repo: &Repository, at: &Place, spec: &str) -> Res<ObjectId> {
        if let Some(oid) = staged(repo, at, spec)? {
            return Ok(oid);
        }
        let mapped = at.at_head(spec);
        let named = match mapped.split_once(':') {
            // only a path that starts with `./` or `../` is read from
            // where the command runs, as for git
            Some((rev, path)) if path.starts_with("./") || path.starts_with("../") => {
                format!("{rev}:{}", at.path(path)?)
            }
            _ => mapped,
        };
        if let Some(oid) = repo.rev_parse(&named)? {
            return Ok(oid);
        }
        // git tells an upstream that is not set and a path that is not
        // there apart from a revision that names nothing
        if spec.ends_with("@{u}") || spec.ends_with("@{upstream}") {
            let branch = named.split('@').next().unwrap_or_default();
            let branch = repo
                .branch_of(branch)?
                .map_or(branch.to_owned(), |b| b.to_string());
            return Err(fatal(format!(
                "no upstream configured for branch '{branch}'"
            )));
        }
        match spec.split_once(':') {
            Some((rev, path)) if repo.rev_parse(&at.at_head(rev))?.is_some() => {
                Err(fatal(format!("path '{path}' does not exist in '{rev}'")))
            }
            _ => Err(unknown(spec)),
        }
    }

    /// One revision as the options have it printed.
    fn print(&self, out: &mut impl Write, repo: &Repository, at: &Place, spec: &str) -> Res<()> {
        let oid = self.resolve(repo, at, spec)?;
        if self.abbrev_ref || self.symbolic_full_name {
            let Some(full) = full_name(repo, at, spec)? else {
                // no ref: git prints nothing for the short name
                return Ok(());
            };
            let prefixes = ["refs/heads/", "refs/tags/", "refs/remotes/"];
            let short = prefixes.iter().find_map(|prefix| full.strip_prefix(prefix));
            let shown = match self.abbrev_ref {
                true => short.unwrap_or(&full),
                false => &full,
            };
            return Ok(writeln!(out, "{shown}")?);
        }
        let shown = match (self.short, repo.abbrev()?) {
            (None, _) => oid.to_string(),
            (Some(0), Some(abbrev)) => abbrev.of(&oid)?,
            (Some(length), Some(abbrev)) => abbrev.from_len(length.max(4)).of(&oid)?,
            (Some(length), None) => oid.to_string()[..length.clamp(7, 40)].to_owned(),
        };
        Ok(writeln!(out, "{shown}")?)
    }

    /// Runs the command. A quiet `--verify` that finds nothing ends with
    /// status 1 and says nothing.
    pub fn run(&self, out: &mut impl Write, repo: &Repository, at: &Place) -> Res<()> {
        self.places(out, repo, at)?;
        let single = || fatal("Needed a single revision".to_owned());
        if self.verify {
            let [spec] = &self.rev[..] else {
                return Err(single());
            };
            return self
                .print(out, repo, at, spec)
                .map_err(|_| match self.quiet {
                    true => {
                        let kind = crate::machine::REFUSED_AS_SAID;
                        crate::machine::Failure::refusal(kind, "Needed a single revision").into()
                    }
                    false => single(),
                });
        }
        if self.short.is_some() && self.rev.is_empty() {
            return Err(single());
        }
        for spec in &self.rev {
            self.print(out, repo, at, spec)?;
        }
        Ok(())
    }
}

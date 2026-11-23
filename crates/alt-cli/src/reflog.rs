//! `alt reflog`: the log of a ref, the latest move first.

use std::io::Write;

use alt_repo::Repository;

use crate::native::pathspec::Place;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(alt_args::Args)]
pub struct Reflog {
    /// The ref whose log to show: a branch, `HEAD` where none is given.
    /// `show` may come before it, as in git
    #[arg(value_name = "REF", hint = "rev")]
    pub words: Vec<String>,
    /// Show the latest COUNT moves only
    #[arg(short = 'n', value_name = "COUNT", hint = none)]
    pub max_count: Option<usize>,
    /// Emit the log as a stable JSON object
    #[arg(long)]
    pub json: bool,
}

fn fatal(said: String) -> Box<dyn std::error::Error> {
    crate::git_settings::fatal(said)
}

impl Reflog {
    /// The ref asked for, as it is written, and its full name.
    fn asked(&self, repo: &Repository, at: &Place) -> Res<(String, String)> {
        let given = match &self.words[..] {
            [] => "HEAD",
            [show] if show == "show" => "HEAD",
            [show, given] if show == "show" => given,
            [given] => given,
            _ => return Err(crate::machine::Failure::usage("reflog takes one ref").into()),
        };
        let mapped = at.at_head(given);
        if given == "HEAD" {
            if repo.resolve_ref(&mapped)?.is_none() {
                let on = repo.branch_of(&mapped)?.unwrap_or_default();
                let said = format!("your current branch '{on}' does not have any commits yet");
                return Err(fatal(said));
            }
            return Ok((given.to_owned(), mapped));
        }
        let Some(full) = repo.full_name(&mapped)? else {
            return Err(fatal(format!(
                "ambiguous argument '{given}': unknown revision or path not in the working tree.\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'git <command> [<revision>...] -- [<file>...]'"
            )));
        };
        // git shows the name as it is written where the log is found
        // by it, under `refs/` or among the branches, else in full
        let written = [
            given.to_owned(),
            format!("refs/{given}"),
            format!("refs/heads/{given}"),
        ];
        let shown = match written.contains(&full) {
            true => given.to_owned(),
            false => full.clone(),
        };
        Ok((shown, full))
    }

    pub fn run(&self, out: &mut impl Write, repo: &Repository, at: &Place) -> Res<()> {
        use crate::json::Json;
        let (shown, full) = self.asked(repo, at)?;
        let entries = repo.reflog(&full)?;
        let latest = entries
            .iter()
            .rev()
            .take(self.max_count.unwrap_or(usize::MAX));
        let abbrev = repo.abbrev()?;
        let mut listed = Vec::new();
        for (back, entry) in latest.enumerate() {
            let said = entry.said.as_deref().unwrap_or_default();
            if self.json {
                let id = |id: &Option<alt_git_codec::ObjectId>| match id {
                    Some(id) => Json::str(id.to_string()),
                    None => Json::Null,
                };
                listed.push(Json::Object(vec![
                    ("selector", Json::str(format!("{shown}@{{{back}}}"))),
                    ("old", id(&entry.old)),
                    ("new", id(&entry.new)),
                    ("message", Json::str(said)),
                    (
                        "timestamp_ms",
                        entry.at_ms.map_or(Json::Null, |at| Json::Num(at as i64)),
                    ),
                ]));
                continue;
            }
            let short = match (&entry.new, &abbrev) {
                (Some(new), Some(abbrev)) => abbrev.of(new)?,
                (Some(new), None) => new.to_string()[..7].to_owned(),
                (None, _) => "0000000".to_owned(),
            };
            write!(out, "{short} {shown}@{{{back}}}: ")?;
            out.write_all(said)?;
            out.write_all(b"\n")?;
        }
        if self.json {
            let fields = vec![("ref", Json::str(full)), ("entries", Json::Array(listed))];
            crate::json::emit(out, fields)?;
        }
        Ok(())
    }
}

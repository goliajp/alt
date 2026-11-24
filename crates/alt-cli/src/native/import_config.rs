//! What an import takes over from the configuration of the git
//! repository besides the file itself, which the store keeps a copy of
//! and reads under its own settings.
//!
//! A git repository has its remotes in that file; a store has one file
//! for each under `remotes/`. An import makes those files, so that
//! `alt fetch` and `alt push` know the remotes the git repository knew.

use super::*;

/// The remotes `config` has: each with its URL and the first refspec it
/// fetches by, in the order they are first named.
fn remotes_of(config: &alt_git_config::Config) -> Vec<(String, String, Option<String>)> {
    let mut remotes: Vec<(String, String, Option<String>)> = Vec::new();
    for entry in config.entries.iter().filter(|e| e.section == "remote") {
        let (Some(name), Some(value)) = (&entry.subsection, &entry.value) else {
            continue;
        };
        let (name, value) = (name.to_string(), value.to_string());
        let at = remotes.iter().position(|(known, ..)| *known == name);
        let at = at.unwrap_or_else(|| {
            remotes.push((name, String::new(), None));
            remotes.len() - 1
        });
        match entry.key.as_str() {
            // as for git, the last URL given is the one in force
            "url" => remotes[at].1 = value,
            "fetch" if remotes[at].2.is_none() => remotes[at].2 = Some(value),
            _ => {}
        }
    }
    remotes.retain(|(_, url, _)| !url.is_empty());
    remotes
}

impl NativeRepo<'_> {
    /// Makes a remote of this store of each remote the git repository
    /// `source` has, where the store has none of that name: one it has is
    /// the store's own to change. Returns the names taken over.
    pub(super) fn take_remotes(&mut self, source: &alt_repo::Repository) -> Res<Vec<String>> {
        let config = source.config_at(crate::git_settings::home())?;
        let mut taken = Vec::new();
        for (name, url, fetch) in remotes_of(&config) {
            let path = self.remote_path(&name);
            if check_remote_name(&name).is_err() || path.exists() {
                continue;
            }
            std::fs::create_dir_all(path.parent().ok_or("remote path has no parent")?)?;
            let fetch = fetch.unwrap_or_else(|| default_fetch_refspec(&name));
            crate::durable::write(&path, format!("url={url}\nfetch={fetch}\n"))?;
            taken.push(name);
        }
        Ok(taken)
    }
}

/// Whether the file system that holds the store `alt_dir` takes names
/// that differ in case for one: told, as git tells it, by reaching the
/// directory under another spelling.
pub(super) fn folds_case(alt_dir: &Path) -> bool {
    alt_dir.with_file_name(".ALT").symlink_metadata().is_ok()
}

/// Writes `core.ignoreCase` into the configuration of the new store
/// `alt_dir` where its file system ignores case, as `git init` does.
pub(super) fn note_case(alt_dir: &Path) -> Res<()> {
    if folds_case(alt_dir) {
        let kept = alt_dir.join("git-import");
        std::fs::create_dir_all(&kept)?;
        crate::durable::write(kept.join("config"), "[core]\n\tignorecase = true\n")?;
    }
    Ok(())
}

/// Why a variable the git repository read is not read here.
pub(super) const WHY: &str = "a file the git configuration includes sets it, and the path to that file leads elsewhere from here";

impl NativeRepo<'_> {
    /// Checks out the head of a store imported into an empty directory,
    /// and says so. An import whose files cannot be written stands.
    pub(super) fn materialise_fresh(&mut self, target: &Path, out: &mut impl Write) -> Res<()> {
        if let Err(e) = self.materialise_head() {
            let said = "warning: import succeeded but work-tree materialize failed";
            return Ok(writeln!(out, "{said}: {e}")?);
        }
        let branch = self.head_branch().unwrap_or_else(|_| "HEAD".to_string());
        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        Ok(writeln!(
            out,
            "materialized {short} into {}",
            target.display()
        )?)
    }

    /// The variables the git repository `source` read from its own
    /// configuration that the store does not read from its copy of it,
    /// or reads other values of: a file the configuration includes by a
    /// relative path is found from the store only where it lay in the
    /// git directory or lies in the working tree.
    pub(super) fn not_carried(&self, source: &alt_repo::Repository) -> Res<Vec<String>> {
        let home = crate::git_settings::home();
        let theirs = source.config_at(home.clone())?.entries;
        let control = self.store.alt_dir.join("git-import");
        let ctx = alt_git_config::IncludeContext {
            git_dir: Some(control.clone()),
            branch: source.branch_of("HEAD")?,
            home,
        };
        let (ours, _) = alt_repo::kept_config::source(&control, &ctx)?;
        let values = |entries: &[alt_git_config::Entry], name| -> Vec<Option<BString>> {
            let named = entries
                .iter()
                .filter(|e| alt_git_config::Name::of(e) == name);
            named.map(|e| e.value.clone()).collect()
        };
        let mut differing: Vec<String> = Vec::new();
        for entry in theirs.iter().chain(&ours) {
            let name = alt_git_config::Name::of(entry);
            let key = entry.display_key();
            if values(&theirs, name) != values(&ours, name) && !differing.contains(&key) {
                differing.push(key);
            }
        }
        Ok(differing)
    }
}

/// What an import says it brought into the store under `alt_dir`.
pub(super) fn say_imported(
    out: &mut impl Write,
    report: &alt_import::ImportReport,
    alt_dir: &Path,
) -> Res<()> {
    writeln!(
        out,
        "imported {} objects ({} new), {} refs ({} changed), \
         {} lineage deltas into {}",
        report.objects_seen,
        report.objects_new,
        report.refs_seen,
        report.refs_changed,
        report.lineage_deltas,
        alt_dir.display()
    )?;
    match report.op {
        Some(op) => writeln!(out, "op {op}")?,
        None => writeln!(out, "already up to date, no op recorded")?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_are_read_with_their_last_url_and_first_refspec() {
        let text = b"[remote \"origin\"]\n\turl = https://one\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
            [core]\n\tbare = false\n[remote \"mirror\"]\n\tpushurl = https://push\n\
            [remote \"origin\"]\n\turl = https://two\n\tfetch = +refs/tags/*:refs/tags/*\n\
            [remote \"up\"]\n\turl = https://up\n";
        let config = alt_git_config::Config {
            entries: alt_git_config::parse_file(text).unwrap(),
        };
        let read = remotes_of(&config);
        let origin = "+refs/heads/*:refs/remotes/origin/*".to_owned();
        assert_eq!(
            read,
            [
                ("origin".to_owned(), "https://two".to_owned(), Some(origin)),
                ("up".to_owned(), "https://up".to_owned(), None),
            ]
        );
    }
}

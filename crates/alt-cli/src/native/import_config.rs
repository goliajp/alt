//! What an import takes over from the configuration of the git
//! repository besides the file itself, which the store keeps as it is and
//! reads its own settings from.
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

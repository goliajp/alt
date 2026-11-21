//! Which of many paths are offered at once.

/// The most paths one group offers; more than that are offered a
/// directory level at a time.
pub(super) const MOST: usize = 1000;

/// The files and the directories to offer of `paths` for what is typed.
/// A directory ends in `/` and is gone into, so no space follows it.
/// More than [`MOST`] paths are cut to the level of the directory typed
/// so far, and that level, where it is still too long, to those that
/// begin as typed.
pub(super) fn offered(paths: Vec<String>, typed: &str) -> (Vec<String>, Vec<String>) {
    let (mut directories, mut files): (Vec<String>, Vec<String>) =
        paths.into_iter().partition(|path| path.ends_with('/'));
    if files.len() + directories.len() <= MOST {
        return (files, directories);
    }
    let within = typed.rfind('/').map_or("", |slash| &typed[..=slash]);
    let mut level = Vec::new();
    files.retain(
        |path| match path.strip_prefix(within).map(|rest| rest.split_once('/')) {
            Some(Some((directory, _))) => {
                level.push(format!("{within}{directory}/"));
                false
            }
            Some(None) => true,
            None => false,
        },
    );
    directories.retain(|path| {
        let rest = path
            .strip_prefix(within)
            .map(|rest| rest.trim_end_matches('/'));
        match rest.map(|rest| rest.split_once('/')) {
            Some(Some((directory, _))) => {
                level.push(format!("{within}{directory}/"));
                false
            }
            Some(None) => true,
            None => false,
        }
    });
    directories.extend(level);
    directories.sort();
    directories.dedup();
    if files.len() + directories.len() > MOST {
        files.retain(|path| path.starts_with(typed));
        directories.retain(|path| path.starts_with(typed));
        files.truncate(MOST);
        directories.truncate(MOST - files.len());
    }
    (files, directories)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn many() -> Vec<String> {
        let mut paths: Vec<String> = (0..MOST).map(|n| format!("src/gen/f{n:04}.rs")).collect();
        paths.extend(["README.md", "src/lib.rs", "src/gen/deep/x.rs", "docs/"].map(String::from));
        paths
    }

    #[test]
    fn few_paths_are_all_offered() {
        let paths = ["a.rs", "new/", "src/b.rs"].map(String::from).to_vec();
        let (files, directories) = offered(paths, "sr");
        assert_eq!(files, ["a.rs", "src/b.rs"]);
        assert_eq!(directories, ["new/"]);
    }

    #[test]
    fn many_paths_are_offered_a_level_at_a_time() {
        let (files, directories) = offered(many(), "");
        assert_eq!(files, ["README.md"]);
        assert_eq!(directories, ["docs/", "src/"]);
        let (files, directories) = offered(many(), "src/l");
        assert_eq!(files, ["src/lib.rs"]);
        assert_eq!(directories, ["src/gen/"]);
    }

    #[test]
    fn a_level_too_long_is_cut_to_what_begins_as_typed() {
        let mut paths = many();
        paths.push("src/gen/extra.rs".to_owned());
        let (files, directories) = offered(paths.clone(), "src/gen/f00");
        assert_eq!(files.len(), 100);
        assert!(files.iter().all(|path| path.starts_with("src/gen/f00")));
        assert!(directories.is_empty());
        let (files, directories) = offered(paths, "src/gen/");
        assert_eq!(files.len() + directories.len(), MOST);
    }
}

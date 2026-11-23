//! The stack of stashed entries.
//!
//! git has one ref for it, `refs/stash`, at the latest entry, and reads
//! the stack from the log of that ref: a line for each entry, the oldest
//! first, with the commit of the entry before it, who made it and the
//! subject of its commit. A native store has a ref for each entry
//! instead, `refs/stashes/<n>`, a later entry a higher number.

use std::fs;
use std::io;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};

use crate::RefError;

/// The ref that names the latest entry.
pub const TOP: &str = "refs/stash";

/// What the name of the ref of an entry begins with; its number follows.
pub const ENTRIES: &str = "refs/stashes/";

/// The number of the entry the ref `name` is of, if it is one.
pub fn entry_number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix(ENTRIES)?;
    let plain = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    plain.then(|| digits.parse().ok()).flatten()
}

/// One entry as the log tells it.
pub struct Logged<'a> {
    pub commit: ObjectId,
    /// who made it and when, as the commit has its committer
    pub by: &'a [u8],
    /// the first line of the message of the commit
    pub subject: &'a [u8],
}

/// Writes the log of `refs/stash` under `git_dir` for `entries`, the
/// oldest first; nothing where there are none.
pub fn write_log(git_dir: &Path, entries: &[Logged<'_>], algo: HashAlgo) -> Result<(), RefError> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut before = "0".repeat(algo.hex_len());
    let mut log = Vec::new();
    for entry in entries {
        log.extend_from_slice(format!("{before} {} ", entry.commit).as_bytes());
        log.extend_from_slice(entry.by);
        log.push(b'\t');
        log.extend_from_slice(entry.subject);
        log.push(b'\n');
        before = entry.commit.to_string();
    }
    let path = git_dir.join("logs").join(TOP);
    fs::create_dir_all(path.parent().ok_or(RefError::Format("bad log path"))?)?;
    Ok(fs::write(path, log)?)
}

/// The commits of the entries the log of `refs/stash` under `git_dir`
/// lists, the oldest first; none where there is no log.
pub fn read_log(git_dir: &Path) -> Result<Vec<ObjectId>, RefError> {
    let log = match fs::read(git_dir.join("logs").join(TOP)) {
        Ok(log) => log,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut commits = Vec::new();
    for line in log.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        // the id before, a space, the id of the entry
        let mut ids = line.split(|b| *b == b' ');
        let new = ids.nth(1).ok_or(RefError::Format("stash log line"))?;
        let commit = ObjectId::from_hex(new).map_err(|_| RefError::Format("stash log id"))?;
        commits.push(commit);
    }
    Ok(commits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_written_is_the_one_read_and_the_one_git_writes() {
        let dir = std::env::temp_dir().join(format!("alt-stash-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(read_log(&dir).unwrap().is_empty());
        write_log(&dir, &[], HashAlgo::Sha1).unwrap();
        assert!(!dir.join("logs").exists());
        let id = |byte: u8| ObjectId::Sha1([byte; 20]);
        let by = b"t <t@e> 1700000000 +0000";
        let entries = [
            Logged {
                commit: id(1),
                by,
                subject: b"WIP on main: 1234567 base",
            },
            Logged {
                commit: id(2),
                by,
                subject: b"On main: second",
            },
        ];
        write_log(&dir, &entries, HashAlgo::Sha1).unwrap();
        let written = fs::read_to_string(dir.join("logs/refs/stash")).unwrap();
        let (zero, one, two) = ("0".repeat(40), "01".repeat(20), "02".repeat(20));
        let lines = format!(
            "{zero} {one} t <t@e> 1700000000 +0000\tWIP on main: 1234567 base\n\
             {one} {two} t <t@e> 1700000000 +0000\tOn main: second\n"
        );
        assert_eq!(written, lines);
        assert_eq!(read_log(&dir).unwrap(), [id(1), id(2)]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_ref_with_a_number_is_one_of_an_entry() {
        assert_eq!(entry_number("refs/stashes/12"), Some(12));
        assert_eq!(entry_number("refs/stashes/"), None);
        assert_eq!(entry_number("refs/stashes/1a"), None);
        assert_eq!(entry_number("refs/stash"), None);
    }
}

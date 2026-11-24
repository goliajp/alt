//! The message a pick is committed with is git's byte for byte, whatever
//! the message picked: with no newline at its end, with blank lines and
//! space git cleans up for `-x` alone, and with bytes that are no UTF-8.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::like_git::{alt, export, git, ok};

const MESSAGES: [(&str, &[u8]); 11] = [
    ("nonl", b"no newline at end"),
    ("only", b"subject"),
    ("two", b"subject\n\nbody\n\n\n"),
    (
        "ws",
        b"subject  \n\n  body with trailing   \n\n\n\nlast\n   \n",
    ),
    ("lead", b"\n \n\nsubject after blanks\n"),
    ("tr", b"subject\n\nSigned-off-by: A <a@b>\n"),
    ("trnonl", b"subject\n\nSigned-off-by: A <a@b>"),
    ("latin", b"caf\xe9 subject\n\nbody \xff\n"),
    ("cr", b"subject\r\n\r\nbody\r\n"),
    ("hash", b"subject\n\n# not a comment\nbody\n"),
    ("feed", b"subject\x0c\n\nbody\n"),
];

/// A commit on top of `main` that adds a file, with `message` as it is.
fn commit(repo: &Path, name: &str, message: &[u8]) {
    std::fs::write(repo.join(format!("file-{name}")), name).unwrap();
    ok(git(repo, &["add", "."]));
    let tree = ok(git(repo, &["write-tree"]));
    let mut made = Command::new("git")
        .current_dir(repo)
        .args(["commit-tree", tree.trim(), "-p", "main"])
        .envs(crate::like_git::SIGNING)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    made.stdin.take().unwrap().write_all(message).unwrap();
    let made = ok(made.wait_with_output().unwrap());
    ok(git(repo, &["branch", &format!("src-{name}"), made.trim()]));
    ok(git(repo, &["reset", "-q", "--hard", "main"]));
}

/// The message of the commit `rev` names, as the object has it.
fn message(repo: &Path, rev: &str) -> Vec<u8> {
    let object = git(repo, &["cat-file", "commit", rev]);
    assert!(object.status.success(), "{rev}");
    let begins = object.stdout.windows(2).position(|pair| pair == b"\n\n");
    object.stdout[begins.unwrap() + 2..].to_vec()
}

#[test]
fn a_pick_has_the_message_git_gives_it_byte_for_byte() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, to) = (dir.path().join("source"), dir.path().join("to"));
    std::fs::create_dir(&source).unwrap();
    ok(git(&source, &["init", "-q", "."]));
    std::fs::write(source.join("f"), "base\n").unwrap();
    ok(git(&source, &["add", "f"]));
    ok(git(&source, &["commit", "-q", "-m", "base"]));
    for (name, message) in MESSAGES {
        commit(&source, name, message);
    }
    ok(alt(&source, &["import", to.to_str().unwrap()]));
    let picks = || {
        let noted = [("plain", None), ("noted", Some("-x"))];
        MESSAGES
            .iter()
            .flat_map(move |(name, _)| noted.map(|(how, flag)| (*name, how, flag)))
    };
    for (name, how, flag) in picks() {
        let (branch, from) = (format!("pick-{name}-{how}"), format!("src-{name}"));
        let mut pick = vec!["cherry-pick"];
        pick.extend(flag);
        pick.push(&from);
        ok(alt(&to, &["switch", "-c", &branch, "main"]));
        ok(alt(&to, &pick));
        ok(git(&source, &["switch", "-q", "-c", &branch, "main"]));
        ok(git(&source, &pick));
    }
    let out = export(&to);
    let exported = out.path().join("exported");
    for (name, how, _) in picks() {
        let branch = format!("pick-{name}-{how}");
        let (ours, theirs) = (message(&exported, &branch), message(&source, &branch));
        assert_eq!(
            String::from_utf8_lossy(&ours),
            String::from_utf8_lossy(&theirs),
            "{branch}"
        );
        assert_eq!(ours, theirs, "{branch}");
    }
}

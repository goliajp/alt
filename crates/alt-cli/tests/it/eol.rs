//! `.gitattributes` line endings: `eol=crlf` paths are checked out with
//! CRLF and stored with LF, each step compared against git doing the same.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn read(root: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(root.join(rel)).unwrap()
}

fn tree(run: &dyn Fn(&[&str]) -> Output) -> String {
    ok(run(&["cat-file", "-p", "HEAD"]))
        .lines()
        .next()
        .unwrap()
        .to_owned()
}

fn hunks(diff: &str) -> String {
    diff.lines()
        .filter(|l| !l.starts_with("index ") && !l.starts_with("diff --git"))
        .collect::<Vec<_>>()
        .join("\n")
}

const FILES: [&str; 5] = [
    "run.bat",
    "notes.txt",
    "plain.c",
    "unix/run.bat",
    "mixed.bat",
];

/// A git repository with text rules, checked out afresh so that git has
/// applied them, and its alt import.
fn imported(src: &Path, dst: &Path) {
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    write(
        src,
        ".gitattributes",
        b"*.bat text eol=crlf\n*.txt text\n*.bin binary\n",
    );
    write(src, "unix/.gitattributes", b"*.bat eol=lf\n");
    write(src, "run.bat", b"@echo off\necho one\n");
    write(src, "notes.txt", b"plain\ntext\n");
    write(src, "plain.c", b"int main(void)\n{\n}\n");
    write(src, "unix/run.bat", b"echo unix\n");
    write(src, "mixed.bat", b"a\nb\r\nlast");
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "base"]));
    for f in FILES {
        std::fs::remove_file(src.join(f)).unwrap();
    }
    ok(git(src, &["checkout", "-q", "--", "."]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));
}

#[test]
fn import_checks_out_crlf_paths_as_git_does() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    imported(src, dst);
    assert_eq!(read(src, "run.bat"), b"@echo off\r\necho one\r\n");
    for f in FILES {
        assert_eq!(read(dst, f), read(src, f), "{f}");
    }
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));
    assert!(ok(alt(dst, &["diff"])).is_empty());
}

#[test]
fn crlf_edits_are_diffed_and_stored_with_lf() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    imported(src, dst);
    for root in [src, dst] {
        write(root, "run.bat", b"@echo off\r\necho one\r\necho two\r\n");
        write(root, "notes.txt", b"plain\r\ntext\r\nmore\r\n");
        write(root, "plain.c", b"int main(void)\r\n{\r\n}\r\n");
        write(root, "new.bat", b"fresh\r\n");
    }
    assert_eq!(
        hunks(&ok(alt(dst, &["diff"]))),
        hunks(&ok(git(src, &["-c", "core.pager=cat", "diff"])))
    );
    ok(alt(dst, &["add", "."]));
    ok(alt(dst, &["commit", "-m", "edit"]));
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "edit"]));
    assert_eq!(tree(&|a| alt(dst, a)), tree(&|a| git(src, a)));
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));
    // the working tree keeps what was typed
    assert_eq!(
        read(dst, "run.bat"),
        b"@echo off\r\necho one\r\necho two\r\n"
    );
}

#[test]
fn switch_writes_crlf_for_the_target_tree() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    write(src, "run.bat", b"one\n");
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "no rules yet"]));
    ok(git(src, &["switch", "-q", "-c", "rules"]));
    write(src, ".gitattributes", b"*.bat eol=crlf\n");
    write(src, "run.bat", b"one\ntwo\n");
    write(src, "sub/other.bat", b"three\n");
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "rules"]));
    ok(git(src, &["switch", "-q", "main"]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));
    assert_eq!(read(dst, "run.bat"), b"one\n");

    ok(alt(dst, &["switch", "rules"]));
    ok(git(src, &["switch", "-q", "rules"]));
    for f in ["run.bat", "sub/other.bat"] {
        assert_eq!(read(dst, f), read(src, f), "{f}");
    }
    assert_eq!(read(dst, "run.bat"), b"one\r\ntwo\r\n");
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));

    ok(alt(dst, &["switch", "main"]));
    assert_eq!(read(dst, "run.bat"), b"one\n");
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));
}

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

#[test]
fn switch_between_branches_that_differ_only_in_attributes_rewrites_nothing() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    write(src, "run.bat", b"one\ntwo\n");
    write(src, ".gitattributes", b"*.bat -text\n");
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "stored as it is"]));
    ok(git(src, &["switch", "-q", "-c", "rules"]));
    write(src, ".gitattributes", b"*.bat eol=crlf\n");
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "only the rule changes"]));
    ok(git(src, &["switch", "-q", "main"]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));

    // the file is the same blob on both branches: neither tool touches it
    ok(alt(dst, &["switch", "rules"]));
    ok(git(src, &["switch", "-q", "rules"]));
    assert_eq!(read(dst, "run.bat"), b"one\ntwo\n");
    assert_eq!(read(dst, "run.bat"), read(src, "run.bat"));
    assert_eq!(read(dst, ".gitattributes"), b"*.bat eol=crlf\n");
}

/// Runs `tool` with `home` as the home directory, where git and alt both
/// look for the user's attributes file.
fn at_home(tool: &str, home: &Path, repo: &Path, args: &[&str]) -> Output {
    let program = match tool {
        "alt" => env!("CARGO_BIN_EXE_alt"),
        other => other,
    };
    Command::new(program)
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

/// Builds a git repository holding `files` under the given configuration
/// and attribute files, imports it, and checks at every step that alt
/// leaves the same bytes in the working tree and the same tree in the
/// store as git: after checkout, and after the same edit is committed.
fn agrees_with_git(
    config: &[(&str, &str)],
    info: Option<&str>,
    global: Option<&str>,
    files: &[(&str, &[u8])],
    edit: &[u8],
) {
    let (home, src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (home, src, dst) = (home.path(), src.path(), dst.path());
    let git = |args: &[&str]| ok(at_home("git", home, src, args));
    let alt = |args: &[&str]| ok(at_home("alt", home, dst, args));
    if let Some(text) = global {
        write(home, ".config/git/attributes", text.as_bytes());
    }
    git(&["init", "-q", "-b", "main", "."]);
    for (key, value) in config {
        git(&["config", key, value]);
    }
    if let Some(text) = info {
        write(src, ".git/info/attributes", text.as_bytes());
    }
    for (path, content) in files {
        write(src, path, content);
    }
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    // checked out afresh, so that git has converted every file
    for (path, _) in files {
        std::fs::remove_file(src.join(path)).unwrap();
    }
    git(&["checkout", "-q", "--", "."]);
    ok(at_home(
        "alt",
        home,
        src,
        &["import", dst.to_str().unwrap()],
    ));
    for (path, _) in files {
        assert_eq!(read(dst, path), read(src, path), "{path} after checkout");
    }
    assert!(alt(&["status"]).contains("nothing to commit"));
    assert!(alt(&["diff"]).is_empty());

    for (path, _) in files.iter().filter(|(p, _)| !p.ends_with(".gitattributes")) {
        for root in [src, dst] {
            let mut content = read(root, path);
            content.extend_from_slice(edit);
            write(root, path, &content);
        }
    }
    alt(&["add", "."]);
    alt(&["commit", "-m", "edit"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "edit"]);
    assert_eq!(
        tree(&|a| at_home("alt", home, dst, a)),
        tree(&|a| at_home("git", home, src, a))
    );
    assert!(alt(&["status"]).contains("nothing to commit"));
}

const TEXTS: [(&str, &[u8]); 4] = [
    ("unix.txt", b"one\ntwo\n"),
    ("dos.txt", b"one\r\ntwo\r\n"),
    ("data.bin", b"a\n\0b\n"),
    ("sub/deep.c", b"int x;\n"),
];

#[test]
fn core_autocrlf_converts_what_looks_like_text() {
    for value in ["true", "input"] {
        agrees_with_git(&[("core.autocrlf", value)], None, None, &TEXTS, b"more\r\n");
    }
}

#[test]
fn core_eol_sets_the_line_ending_of_text_files() {
    let mut files = TEXTS.to_vec();
    files.push((".gitattributes", b"* text=auto\n*.c text\n"));
    for value in ["crlf", "lf"] {
        agrees_with_git(&[("core.eol", value)], None, None, &files, b"more\n");
    }
}

#[test]
fn the_repositorys_and_the_users_attribute_files_are_read() {
    let mut files = TEXTS.to_vec();
    files.push((".gitattributes", b"*.c eol=crlf\n"));
    // the user's file is the weakest, the repository's own the strongest
    agrees_with_git(
        &[],
        Some("sub/*.c eol=lf\n"),
        Some("*.txt eol=crlf\n*.c eol=lf\n"),
        &files,
        b"more\r\n",
    );
    // core.attributesFile names the user's file
    let named = alt_os::temp::tempdir().unwrap();
    write(named.path(), "attrs", b"*.txt eol=crlf\n");
    let path = named.path().join("attrs");
    agrees_with_git(
        &[("core.attributesFile", path.to_str().unwrap())],
        None,
        None,
        &TEXTS,
        b"more\r\n",
    );
}

#[test]
fn the_ident_keyword_is_expanded_on_checkout() {
    let files: [(&str, &[u8]); 3] = [
        (".gitattributes", b"*.c ident\n"),
        ("a.c", b"/* $Id$ */\nint a;\n"),
        ("b.txt", b"$Id$ stays\n"),
    ];
    agrees_with_git(&[], None, None, &files, b"/* $Id: stale $ */\n");
}

#[test]
fn filter_commands_run_on_checkout_and_when_staging() {
    let files: [(&str, &[u8]); 3] = [
        (
            ".gitattributes",
            b"*.up filter=upper\n*.gone filter=missing\n",
        ),
        ("shout.up", b"quiet words\n"),
        ("plain.gone", b"no such driver\n"),
    ];
    agrees_with_git(
        &[
            ("filter.upper.clean", "tr A-Z a-z"),
            ("filter.upper.smudge", "tr a-z A-Z"),
        ],
        None,
        None,
        &files,
        b"MORE Words\n",
    );
}

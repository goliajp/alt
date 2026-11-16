//! `alt blame <path>`: each line is attributed to the commit that
//! introduced it, through merges and renames, as `git blame` does.

use std::fs;
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

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn blame_single_commit_attributes_every_line_to_that_commit() {
    let tmp = alt_os::temp::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
    ok(alt(tmp.path(), &["add", "a.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "first"]));
    let head = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    let short = &head[..8];

    let blame_out = ok(alt(tmp.path(), &["blame", "a.txt"]));
    let lines: Vec<&str> = blame_out.lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 lines: {blame_out}");
    for line in &lines {
        assert!(
            line.starts_with(short),
            "expected origin {short} on every line, got: {line}",
        );
    }
}

#[test]
fn blame_assigns_lines_to_the_commits_that_introduced_them() {
    let tmp = alt_os::temp::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("f.txt"), "alpha\nbeta\n").unwrap();
    ok(alt(tmp.path(), &["add", "f.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c1"]));
    let c1 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    fs::write(tmp.path().join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
    ok(alt(tmp.path(), &["add", "f.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c2"]));
    let c2 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    let blame_out = ok(alt(tmp.path(), &["blame", "f.txt"]));
    let lines: Vec<&str> = blame_out.lines().collect();
    assert_eq!(lines.len(), 3, "got: {blame_out}");

    let s1 = &c1[..8];
    let s2 = &c2[..8];
    assert!(lines[0].starts_with(s1), "alpha → c1: {}", lines[0]);
    assert!(lines[1].starts_with(s1), "beta → c1: {}", lines[1]);
    assert!(lines[2].starts_with(s2), "gamma → c2: {}", lines[2]);
}

#[test]
fn blame_missing_file_fails_clearly() {
    let tmp = alt_os::temp::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("a.txt"), "x\n").unwrap();
    ok(alt(tmp.path(), &["add", "a.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "first"]));

    let out = alt(tmp.path(), &["blame", "no/such/file.txt"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no such path 'no/such/file.txt'"), "{err}");
}

fn git(repo: &Path, date: u32, args: &[&str]) -> String {
    let stamp = format!("@{date} +0000");
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .args(args)
        .output()
        .unwrap();
    ok(out)
}

fn numbered(lines: std::ops::RangeInclusive<u32>) -> Vec<String> {
    lines.map(|n| format!("line {n}")).collect()
}

fn put(repo: &Path, file: &str, lines: &[String]) {
    fs::write(repo.join(file), lines.join("\n") + "\n").unwrap();
}

/// Commits everything in the work tree at `date`, returning the commit.
fn commit(repo: &Path, date: u32, message: &str) -> String {
    git(repo, date, &["add", "-A"]);
    git(repo, date, &["commit", "-qm", message]);
    git(repo, date, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// The commit each line of `file` is attributed to, by alt (in the import
/// of `src` at `dst`) and by git, as eight hex digits.
fn attributions(src: &Path, dst: &Path, file: &str) -> (Vec<String>, Vec<String>) {
    ok(alt(src, &["import", dst.to_str().unwrap()]));
    let first8 = |text: String| -> Vec<String> {
        text.lines()
            .map(|l| l.trim_start_matches('^')[..8].to_owned())
            .collect()
    };
    (
        first8(ok(alt(dst, &["blame", file]))),
        first8(git(src, 0, &["blame", "-l", "--root", file])),
    )
}

#[test]
fn blame_looks_through_a_merge_at_the_commits_of_both_sides() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    git(src, 1000, &["init", "-q", "-b", "main", "."]);
    let mut lines = numbered(1..=12);
    put(src, "f.txt", &lines);
    let base = commit(src, 1000, "base");

    git(src, 1100, &["switch", "-q", "-c", "side"]);
    let mut side = lines.clone();
    side[1] = "side two".into();
    side.push("side tail".into());
    put(src, "f.txt", &side);
    let on_side = commit(src, 1100, "side edits");

    git(src, 1200, &["switch", "-q", "main"]);
    lines[9] = "main ten".into();
    put(src, "f.txt", &lines);
    let on_main = commit(src, 1200, "main edit");
    git(
        src,
        1300,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );

    let mut merged: Vec<String> = fs::read_to_string(src.join("f.txt"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    merged[4] = "after five".into();
    put(src, "f.txt", &merged);
    let after = commit(src, 1400, "after the merge");

    let (ours, theirs) = attributions(src, dst, "f.txt");
    assert_eq!(ours, theirs);
    assert_eq!(ours[0], base[..8]);
    assert_eq!(ours[1], on_side[..8], "a line changed on the merged branch");
    assert_eq!(ours[12], on_side[..8]);
    assert_eq!(ours[9], on_main[..8]);
    assert_eq!(ours[4], after[..8]);
}

#[test]
fn blame_follows_a_file_taken_whole_from_the_second_parent() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    git(src, 1000, &["init", "-q", "-b", "main", "."]);
    let lines = numbered(1..=8);
    put(src, "f.txt", &lines);
    put(src, "other.txt", &lines);
    commit(src, 1000, "base");
    git(src, 1100, &["switch", "-q", "-c", "side"]);
    let mut side = lines.clone();
    side[3] = "side four".into();
    put(src, "f.txt", &side);
    let on_side = commit(src, 1100, "side edit");
    git(src, 1200, &["switch", "-q", "main"]);
    put(src, "other.txt", &numbered(1..=9));
    commit(src, 1200, "main touches another file");
    git(
        src,
        1300,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );

    let (ours, theirs) = attributions(src, dst, "f.txt");
    assert_eq!(ours, theirs);
    assert_eq!(ours[3], on_side[..8]);
}

#[test]
fn blame_follows_a_renamed_file() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    git(src, 1000, &["init", "-q", "-b", "main", "."]);
    let mut lines = numbered(1..=10);
    put(src, "old.txt", &lines);
    put(src, "unrelated.txt", &["something else".to_owned()]);
    let base = commit(src, 1000, "base");

    fs::rename(src.join("old.txt"), src.join("new.txt")).unwrap();
    lines[9] = "renamed ten".into();
    put(src, "new.txt", &lines);
    let renamed = commit(src, 1100, "rename and edit");

    fs::create_dir(src.join("dir")).unwrap();
    fs::rename(src.join("new.txt"), src.join("dir/new.txt")).unwrap();
    let moved = commit(src, 1200, "move without a change");
    lines[0] = "later one".into();
    put(src, "dir/new.txt", &lines);
    let later = commit(src, 1300, "edit after the move");

    let (ours, theirs) = attributions(src, dst, "dir/new.txt");
    assert_eq!(ours, theirs);
    assert_eq!(ours[0], later[..8]);
    assert_eq!(ours[4], base[..8], "through both renames");
    assert_eq!(ours[9], renamed[..8]);
    assert!(!ours.contains(&moved[..8].to_owned()));
}

#[test]
fn blame_at_revision_uses_that_history_only() {
    // Build a 2-commit history; blame at <c1> must attribute lines to
    // c1 only — never reaches forward to c2's later edits.
    let tmp = alt_os::temp::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("g.txt"), "a\nb\n").unwrap();
    ok(alt(tmp.path(), &["add", "g.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c1"]));
    let c1 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    fs::write(tmp.path().join("g.txt"), "a\nb\nc\n").unwrap();
    ok(alt(tmp.path(), &["add", "g.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c2"]));

    let at_c1 = ok(alt(tmp.path(), &["blame", "g.txt", &c1]));
    let lines: Vec<&str> = at_c1.lines().collect();
    assert_eq!(lines.len(), 2, "at c1 the file has 2 lines: {at_c1}");
    let s1 = &c1[..8];
    for line in &lines {
        assert!(line.starts_with(s1), "got: {line}");
    }
}

/// Twelve lines that each carry enough text to be searched for.
fn paragraph(tag: &str) -> Vec<String> {
    (1..=12)
        .map(|n| format!("{tag} sentence number {n} with words"))
        .collect()
}

/// A history in which lines move within a file and between files:
/// `main.txt` has its second paragraph moved to the top, receives a
/// paragraph cut out of `other.txt` in the same commit, and one copied
/// from `library.txt`, which that commit does not touch.
fn moved_and_copied(src: &Path) -> Vec<String> {
    git(src, 1000, &["init", "-q", "-b", "main", "."]);
    let (first, second) = (paragraph("first"), paragraph("second"));
    put(src, "main.txt", &[first.clone(), second.clone()].concat());
    let base = commit(src, 1000, "base");
    put(
        src,
        "other.txt",
        &[paragraph("other"), paragraph("donated")].concat(),
    );
    put(src, "library.txt", &paragraph("library"));
    let others = commit(src, 1100, "two more files");

    put(src, "main.txt", &[second, first].concat());
    let moved = commit(src, 1200, "second paragraph first");

    let mut text: Vec<String> = fs::read_to_string(src.join("main.txt"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    text.extend(paragraph("donated"));
    text.extend(paragraph("library"));
    text.push("written here".to_owned());
    put(src, "main.txt", &text);
    put(src, "other.txt", &paragraph("other"));
    let gathered = commit(src, 1300, "gather paragraphs");
    vec![base, others, moved, gathered]
}

#[test]
fn blame_traces_moved_and_copied_lines_as_git_does() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    let commits = moved_and_copied(src);
    let (base, others, moved, gathered) = (&commits[0], &commits[1], &commits[2], &commits[3]);
    ok(alt(src, &["import", dst.to_str().unwrap()]));
    let first8 = |text: String| -> Vec<String> {
        text.lines()
            .map(|l| l.trim_start_matches('^')[..8].to_owned())
            .collect()
    };
    let mut seen = Vec::new();
    for flags in [
        &[][..],
        &["-M"],
        &["-C"],
        &["-C", "-C"],
        &["-C", "-C", "-C"],
    ] {
        let args =
            |tail: &[&'static str]| -> Vec<&'static str> { [&["blame"], flags, tail].concat() };
        let ours = first8(ok(alt(dst, &args(&["main.txt"]))));
        let theirs = first8(git(src, 0, &args(&["-l", "--root", "main.txt"])));
        assert_eq!(ours, theirs, "blame {flags:?}");
        seen.push(ours);
    }
    let short = |c: &String| c[..8].to_owned();
    // plainly, the paragraph that changed places belongs to the commit that
    // moved it; with -M it is traced to where it was written
    assert_eq!(seen[0][12], short(moved));
    assert_eq!(seen[1][12], short(base));
    assert_eq!(seen[0][0], short(base));
    // lines cut out of a file the same commit changed: -C finds them
    assert_eq!(seen[1][24], short(gathered));
    assert_eq!(seen[2][24], short(others));
    // lines copied from a file the commit left alone: only from -C -C -C
    assert_eq!(seen[3][36], short(gathered));
    assert_eq!(seen[4][36], short(others));
    assert_eq!(
        seen[4][48],
        short(gathered),
        "the line written in that commit"
    );
}

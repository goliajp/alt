//! Corners of how a pattern for hunk headers is chosen, read and
//! applied, each against git.

use std::path::Path;

use crate::funcname_drivers::{ok, pair, run, same_headers};

/// A head, lines no pattern here takes for one, and a line to change.
fn body(head: &str, mark: &str, end: &str) -> String {
    let mut out = format!("{head}{end}");
    for line in 0..8 {
        let mark = if line == 5 { mark } else { "" };
        out.push_str(&format!("  0_{line}{mark}{end}"));
    }
    out
}

/// Both tools' hunk headers of files named `<kind>.txt`, one for each
/// attribute line, with the pattern `pattern` as that of `driver`.
fn headers(lines: &[(&str, &str)], driver: &str, pattern: &str, head: &str) -> Vec<String> {
    let write = |root: &Path, mark: &str| {
        for (kind, _) in lines {
            let end = if *kind == "crlf" { "\r\n" } else { "\n" };
            std::fs::write(root.join(format!("{kind}.txt")), body(head, mark, end)).unwrap();
        }
    };
    let prepare = |root: &Path| {
        write(root, "");
        let attributes: String = lines.iter().map(|(_, line)| format!("{line}\n")).collect();
        std::fs::write(root.join(".gitattributes"), attributes).unwrap();
        let key = format!("diff.{driver}.xfuncname");
        ok(run("git", root, "C", &["config", &key, pattern]));
    };
    let dirs = pair(prepare, |root: &Path| write(root, " changed"));
    same_headers(&dirs, &["diff"], 1)
}

#[test]
fn the_diff_attribute_decides_whether_the_default_pattern_is_used() {
    let lines = [
        ("set", "set.txt diff"),
        ("unset", "unset.txt -diff"),
        ("unspecified", "unspecified.txt !diff"),
        ("named", "named.txt diff=default"),
        ("none", "other.txt diff"),
    ];
    let found = headers(&lines, "default", "^[a-z]+", "pub fn main(x)");
    let named = found.iter().filter(|h| h.ends_with("@@ pub")).count();
    let bare = found.iter().filter(|h| h.ends_with("@@")).count();
    // by the pattern where no driver or the one of that name is given;
    // by none where the attribute is set without a name
    let each = crate::funcname_drivers::locales().len();
    assert_eq!((named, bare), (3 * each, each), "{found:?}");
}

#[test]
fn a_reference_to_a_group_behind_the_first_names_nothing() {
    let lines = [("second", "*.txt diff=x")];
    let found = headers(&lines, "x", "^([a-z]+) ([a-z]+) \\2", "aa bb bb()");
    assert!(found.iter().all(|h| h.ends_with("@@")), "{found:?}");
    let found = headers(&lines, "x", "^([a-z]+) \\1", "aa aa()");
    assert!(found.iter().all(|h| h.ends_with("@@ aa")), "{found:?}");
}

#[test]
fn a_carriage_return_before_the_end_of_a_line_is_no_part_of_it() {
    let lines = [("crlf", "*.txt diff=x")];
    let found = headers(&lines, "x", "^[a-z ]+\\(x\\)$", "pub fn main(x)");
    assert!(
        found.iter().all(|h| h.contains("@@ pub fn main(x)")),
        "{found:?}"
    );
}

#[test]
fn a_pattern_that_does_not_compile_is_refused_in_the_words_of_git() {
    let lines = [("broken", "*.txt diff=x")];
    let write = |root: &Path, mark: &str| {
        std::fs::write(root.join("broken.txt"), body("head", mark, "\n")).unwrap();
    };
    let prepare = |root: &Path| {
        write(root, "");
        std::fs::write(root.join(".gitattributes"), format!("{}\n", lines[0].1)).unwrap();
        ok(run(
            "git",
            root,
            "C",
            &["config", "diff.x.xfuncname", "^(unclosed"],
        ));
    };
    let dirs = pair(prepare, |root: &Path| write(root, " changed"));
    let theirs = run("git", dirs[0].path(), "C", &["diff"]);
    let ours = run("alt", dirs[1].path(), "C", &["diff"]);
    assert!(!theirs.status.success() && !ours.status.success());
    assert_eq!(
        String::from_utf8_lossy(&ours.stderr),
        String::from_utf8_lossy(&theirs.stderr)
    );
}

#[test]
fn a_search_given_up_names_nothing_and_stops_nothing() {
    // every way of splitting the run is tried before the end fails
    let lines = [("given_up", "*.txt diff=x")];
    let write = |root: &Path, mark: &str| {
        let head = "a".repeat(200);
        std::fs::write(root.join("given_up.txt"), body(&head, mark, "\n")).unwrap();
    };
    let prepare = |root: &Path| {
        write(root, "");
        std::fs::write(root.join(".gitattributes"), format!("{}\n", lines[0].1)).unwrap();
        ok(run(
            "git",
            root,
            "C",
            &["config", "diff.x.xfuncname", "^(a|aa)+\\1b$"],
        ));
    };
    let dirs = pair(prepare, |root: &Path| write(root, " changed"));
    let patch = ok(run("alt", dirs[1].path(), "C", &["diff"]));
    let patch = String::from_utf8_lossy(&patch);
    let head = patch.lines().find(|line| line.starts_with("@@")).unwrap();
    assert!(head.ends_with("@@"), "{head}");
}

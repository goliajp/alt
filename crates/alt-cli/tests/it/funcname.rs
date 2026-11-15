//! Hunk headers name the function or section a hunk is in, by the diff
//! driver of the path, as git's do.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// A file of sections: each a head line and a body of numbered lines.
fn sections(heads: &[&str], body: &str) -> String {
    let mut out = String::new();
    for (at, head) in heads.iter().enumerate() {
        out.push_str(head);
        out.push('\n');
        for line in 0..12 {
            out.push_str(&format!("{body}{at}_{line};\n"));
        }
    }
    out
}

fn hunk_headers(patch: &str) -> Vec<&str> {
    patch.lines().filter(|l| l.starts_with("@@")).collect()
}

const FILES: [(&str, &[&str], &str); 6] = [
    (
        "code.c",
        &[
            "static int helper(int a)",
            "out:",
            "int main(void)",
            "public:",
        ],
        "\tx += ",
    ),
    (
        "tool.py",
        &[
            "class Thing:",
            "    def first(self):",
            "    def second(self):",
        ],
        "        v = ",
    ),
    (
        "calc.f",
        &[
            "      SUBROUTINE FOO(X)",
            "C     note",
            "      integer function add(a)",
        ],
        "      y = ",
    ),
    (
        "notes.x",
        &["section one   ", "section skip", "section three"],
        " item ",
    ),
    (
        "parts.y",
        &["part 12 of many", "part x", "part 7"],
        " item ",
    ),
    ("plain.txt", &["Heading", "  indented", "_other"], " item "),
];

#[test]
fn hunk_headers_name_sections_by_the_diff_driver_as_git_does() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    let attributes = "*.c diff=cpp\n*.py diff=python\n*.f diff=fortran\n\
                      *.x diff=custom\n*.y diff=old\n";
    std::fs::write(src.join(".gitattributes"), attributes).unwrap();
    ok(git(
        src,
        &[
            "config",
            "diff.custom.xfuncname",
            "!^section skip\n^(section .*)$",
        ],
    ));
    ok(git(
        src,
        &[
            "config",
            "diff.old.funcname",
            "^\\(part [0-9]\\{1,\\}\\).*$",
        ],
    ));
    for (name, heads, body) in FILES {
        std::fs::write(src.join(name), sections(heads, body)).unwrap();
    }
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "base"]));
    // one changed line deep in every section
    let edit = |root: &Path, mark: &str| {
        for (name, heads, body) in FILES {
            let text = sections(heads, body);
            let mut changed = String::new();
            for line in text.lines() {
                changed.push_str(line);
                if line.ends_with("_9;") {
                    changed.push_str(mark);
                }
                changed.push('\n');
            }
            std::fs::write(root.join(name), changed).unwrap();
        }
    };
    edit(src, " // first");
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "edit"]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));

    let theirs = ok(git(src, &["log", "-p", "--pretty=oneline"]));
    let ours = ok(alt(dst, &["log", "-p", "--pretty=oneline"]));
    let expected = hunk_headers(&theirs);
    assert_eq!(hunk_headers(&ours), expected);
    // the drivers decided, not the plain rule
    for shown in [
        "@@ static int helper(int a)",
        "@@ def second(self):",
        "@@ integer function add(a)",
        "@@ section one",
        "@@ part 12",
        "@@ _other",
    ] {
        assert!(
            expected.iter().any(|h| h.ends_with(shown)),
            "{shown}: {theirs}"
        );
    }
    assert!(
        !expected
            .iter()
            .any(|h| h.ends_with("out:") || h.contains("skip"))
    );

    // and the same for what the working tree changes
    for root in [src, dst] {
        edit(root, " // second");
    }
    let theirs = ok(git(src, &["diff"]));
    let ours = ok(alt(dst, &["diff"]));
    assert_eq!(hunk_headers(&ours), hunk_headers(&theirs));
    assert_eq!(hunk_headers(&ours).len(), expected.len() - 7);
}

#[test]
fn a_pattern_that_does_not_compile_is_reported() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(git(root, &["init", "-q", "-b", "main", "."]));
    std::fs::write(root.join(".gitattributes"), "*.x diff=broken\n").unwrap();
    ok(git(
        root,
        &["config", "diff.broken.xfuncname", "^(unclosed"],
    ));
    std::fs::write(root.join("a.x"), "one\n").unwrap();
    ok(git(root, &["add", "."]));
    ok(git(root, &["commit", "-qm", "base"]));
    let dst = alt_os::temp::tempdir().unwrap();
    ok(alt(root, &["import", dst.path().to_str().unwrap()]));
    std::fs::write(dst.path().join("a.x"), "two\n").unwrap();
    let refused = alt(dst.path(), &["diff"]);
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(
        message.contains("Invalid regexp to look for hunk header"),
        "{message}"
    );
}

/// Every expression of every built-in driver, given to this program's
/// matcher and to the C library's: the same lines match, at the same
/// place, with the same first group.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn built_in_patterns_match_as_the_c_library_matches_them() {
    use std::ffi::{CString, c_char, c_int};

    #[repr(C, align(16))]
    struct Compiled([u8; 256]);
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Span {
        from: c_int,
        to: c_int,
    }
    unsafe extern "C" {
        fn regcomp(compiled: *mut Compiled, pattern: *const c_char, flags: c_int) -> c_int;
        fn regexec(
            compiled: *const Compiled,
            text: *const c_char,
            spans: usize,
            span: *mut Span,
            flags: c_int,
        ) -> c_int;
        fn regfree(compiled: *mut Compiled);
    }

    let lines = include_str!("../fixtures/funcname/lines.txt");
    let mut compared = 0;
    let mut matched = 0;
    for (name, ignore_case, expressions) in alt_cli::funcname::builtin() {
        for expression in expressions {
            let expression = expression.strip_prefix('!').unwrap_or(expression);
            let options = alt_regex::Options {
                ignore_case,
                ..Default::default()
            };
            let ours = alt_regex::Regex::with_options(expression.as_bytes(), options).unwrap();
            let pattern = CString::new(expression).unwrap();
            let mut theirs = Compiled([0; 256]);
            let flags = 1 | if ignore_case { 2 } else { 0 };
            // SAFETY: the buffer is larger than regex_t and the pattern ends in NUL
            assert_eq!(unsafe { regcomp(&mut theirs, pattern.as_ptr(), flags) }, 0);
            for line in lines.lines() {
                let text = CString::new(line).unwrap();
                let mut spans = [Span { from: -1, to: -1 }; 2];
                // SAFETY: compiled above, and the array holds two spans
                let missed = unsafe { regexec(&theirs, text.as_ptr(), 2, spans.as_mut_ptr(), 0) };
                let span = |s: Span| (s.from >= 0).then_some(s.from as usize..s.to as usize);
                let expected = (missed == 0).then(|| [span(spans[0]), span(spans[1])]);
                let found = ours.captures_upto(line.as_bytes(), 1);
                let found = found.map(|c| [c.get(0), c.get(1)]);
                assert_eq!(found, expected, "{name}: {expression:?} on {line:?}");
                compared += 1;
                matched += usize::from(expected.is_some());
            }
            // SAFETY: compiled by regcomp and freed once
            unsafe { regfree(&mut theirs) };
        }
    }
    assert!(compared > 6_000 && matched > 300, "{compared} {matched}");
}

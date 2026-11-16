//! The hunk headers of every built-in driver, and of the ways a pattern
//! is configured, against git's, in a locale of bytes and in a UTF-8
//! one.

use std::path::Path;
use std::process::{Command, Output};

/// Runs git or alt in `repo` with the locale `locale`.
fn run(tool: &str, repo: &Path, locale: &str, args: &[&str]) -> Output {
    run_in(tool, repo, &[("LC_ALL", locale)], args)
}

/// Runs git or alt in `repo` with only `locale` of the variables that
/// name a locale.
fn run_in(tool: &str, repo: &Path, locale: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = match tool {
        "alt" => {
            let mut alt = crate::alt_command();
            alt.env("ALT_NO_DAEMON", "1");
            alt
        }
        _ => Command::new("git"),
    };
    command
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .env_remove("LC_ALL")
        .env_remove("LC_CTYPE")
        .env_remove("LANG")
        .envs(locale.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> Vec<u8> {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o.stdout
}

fn hunk_headers(patch: &[u8]) -> Vec<String> {
    let lines = patch.split(|&b| b == b'\n');
    let heads = lines.filter(|l| l.starts_with(b"@@"));
    heads.map(|l| l.escape_ascii().to_string()).collect()
}

/// The locales git matches in: one of bytes everywhere, and a UTF-8 one
/// where the C library is the one this crate follows.
fn locales() -> &'static [&'static str] {
    if cfg!(all(target_os = "linux", target_env = "gnu")) {
        &["C", "C.UTF-8"]
    } else {
        &["C"]
    }
}

/// A git repository and its import into alt. `prepare` fills and
/// commits the first; `change` then runs on the working tree of each.
fn pair(prepare: impl Fn(&Path), change: impl Fn(&Path)) -> [alt_os::temp::TempDir; 2] {
    let dirs = [0; 2].map(|_| alt_os::temp::tempdir().unwrap());
    let (src, dst) = (dirs[0].path(), dirs[1].path());
    ok(run("git", src, "C", &["init", "-q", "-b", "main", "."]));
    prepare(src);
    ok(run("git", src, "C", &["add", "."]));
    ok(run("git", src, "C", &["commit", "-qm", "base"]));
    ok(run("alt", src, "C", &["import", dst.to_str().unwrap()]));
    change(src);
    change(dst);
    dirs
}

/// Both tools' hunk headers for `args`, which must be the same and
/// `least` or more, in each locale.
fn same_headers(dirs: &[alt_os::temp::TempDir; 2], args: &[&str], least: usize) -> Vec<String> {
    let mut all = Vec::new();
    for locale in locales() {
        let theirs = hunk_headers(&ok(run("git", dirs[0].path(), locale, args)));
        let ours = hunk_headers(&ok(run("alt", dirs[1].path(), locale, args)));
        assert!(theirs.len() >= least, "{locale}: {} headers", theirs.len());
        for (ours, theirs) in ours.iter().zip(&theirs) {
            assert_eq!(ours, theirs, "{locale}");
        }
        assert_eq!(ours.len(), theirs.len(), "{locale}");
        all.extend(theirs);
    }
    all
}

/// Each line of `heads` with lines no driver takes for a head behind
/// it; `mark` goes into one of those of every head.
fn blocks(heads: &[&[u8]], mark: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for (at, head) in heads.iter().enumerate() {
        out.extend_from_slice(head);
        out.push(b'\n');
        for line in 0..8 {
            let mark = if line == 5 { mark } else { "" };
            out.extend(format!("        0_{at}_{line}{mark}\n").bytes());
        }
    }
    out
}

#[test]
fn every_built_in_driver_names_the_sections_git_names() {
    let lines = include_str!("../fixtures/funcname/lines.txt");
    let mut heads: Vec<&[u8]> = lines.lines().map(str::as_bytes).collect();
    // a byte that is no character, which a UTF-8 locale does not match
    heads.push(b"int caf\xe9(void) {");
    heads.push(b"def f\xfcr(self):");
    let all: Vec<&str> = alt_cli::funcname::builtin()
        .map(|(name, ..)| name)
        .collect();
    assert_eq!(all.len(), 27);
    // the drivers are those of git 2.56; an older git has these three
    // otherwise or not at all, from the version named on
    let later = [("bash", 50), ("ini", 50), ("r", 51)];
    let version = ok(run("git", Path::new("."), "C", &["--version"]));
    let version = String::from_utf8(version).unwrap();
    // `git version 2.39.3 (Apple Git-146)`
    let mut numbers = version.split(' ').nth(2).unwrap().split('.');
    let major: u32 = numbers.next().unwrap().parse().unwrap();
    let minor: u32 = numbers.next().unwrap().parse().unwrap();
    let known = |name: &&str| {
        let since = later.iter().find(|(driver, _)| driver == name);
        since.is_none_or(|&(_, since)| major > 2 || minor >= since)
    };
    let drivers: Vec<&str> = all.into_iter().filter(known).collect();
    assert!(drivers.len() >= 24, "{version}");
    let prepare = |root: &Path| {
        let mut attributes = String::new();
        for driver in &drivers {
            attributes.push_str(&format!("*.{driver} diff={driver}\n"));
            std::fs::write(root.join(format!("f.{driver}")), blocks(&heads, "")).unwrap();
        }
        std::fs::write(root.join(".gitattributes"), attributes).unwrap();
    };
    let change = |root: &Path| {
        for driver in &drivers {
            std::fs::write(root.join(format!("f.{driver}")), blocks(&heads, " changed")).unwrap();
        }
    };
    let dirs = pair(prepare, change);
    let headers = same_headers(&dirs, &["diff"], drivers.len() * heads.len());
    // the drivers do name sections, and not all the same ones
    let named = headers.iter().filter(|h| !h.ends_with("@@")).count();
    assert!(named > headers.len() / 2, "{named} of {}", headers.len());
    // the two lines with a byte that is no character are heads only
    // where lines are bytes; git ends a header before such a byte
    let (bytes, characters) = headers.split_at(headers.len() / locales().len());
    let cut = |headers: &[String]| headers.iter().filter(|h| h.ends_with(" int caf")).count();
    assert!(cut(bytes) > 0);
    if locales().len() == 2 {
        assert!(cut(characters) < cut(bytes));
    }
}

/// The three drivers an older git has otherwise or not at all name the
/// sections git 2.56.1 names, whichever git is at hand.
#[test]
fn the_drivers_an_older_git_lacks_name_the_sections_of_the_git_they_are_from() {
    // a driver, lines of a file, and the section of the hunk behind each
    type Case = (
        &'static str,
        &'static [&'static [u8]],
        &'static [&'static str],
    );
    let cases: [Case; 3] = [
        (
            "bash",
            &[
                b"run() {",
                b"  my_func ( ) # note",
                b"function build {",
                b"function deploy() {",
                b"function x",
                b"echo not_a_function",
                b"x=1 # ()",
                b"local()name",
            ],
            &[
                "run() {",
                "my_func ( ) # note",
                "function build {",
                "function deploy() {",
                "function deploy() {",
                "function deploy() {",
                "function deploy() {",
                "local()name",
            ],
        ),
        (
            "ini",
            &[
                b"[core]",
                b"  [remote \"origin\"] ; note",
                b"[]",
                b"key = value",
                b"[section.sub]\ttrailing",
                b"; [comment]",
            ],
            &[
                "[core]",
                "  [remote \\\"origin\\\"]",
                "  [remote \\\"origin\\\"]",
                "  [remote \\\"origin\\\"]",
                "[section.sub]",
                "[section.sub]",
            ],
        ),
        (
            "r",
            &[
                b"fit <- function(x) {",
                b"  plot.all = function(...)",
                b"_bad <- function()",
                b"x <- 1",
                b"Name2 <-function(a)",
                b"^odd = function()",
            ],
            &[
                "fit <- function(x) {",
                "plot.all = function(...)",
                "_bad <- function()",
                "_bad <- function()",
                "Name2 <-function(a)",
                "^odd = function()",
            ],
        ),
    ];
    let files = |root: &Path, mark: &str| {
        for (driver, heads, _) in cases {
            std::fs::write(root.join(format!("f.{driver}")), blocks(heads, mark)).unwrap();
        }
    };
    let prepare = |root: &Path| {
        let attributes = cases.map(|(driver, ..)| format!("*.{driver} diff={driver}\n"));
        std::fs::write(root.join(".gitattributes"), attributes.concat()).unwrap();
        files(root, "");
    };
    let dirs = pair(prepare, |root| files(root, " changed"));
    let expected = cases.iter().flat_map(|(_, _, sections)| sections.iter());
    let expected: Vec<&str> = expected.copied().collect();
    for locale in locales() {
        let ours = hunk_headers(&ok(run("alt", dirs[1].path(), locale, &["diff"])));
        let sections = ours.iter().map(|h| h.splitn(5, ' ').nth(4).unwrap());
        assert_eq!(sections.collect::<Vec<_>>(), expected, "{locale}");
    }
}

#[test]
fn the_default_driver_a_nested_attributes_file_and_an_empty_match_are_as_in_git() {
    let heads: [&[u8]; 4] = [
        b"SECTION one",
        b"part two",
        "SECTION drei größer".as_bytes(),
        b"abc",
    ];
    let prepare = |root: &Path| {
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        for name in [
            "plain.txt",
            "sub/inner.txt",
            "sub/deeper/low.txt",
            "empty.e",
            "group.g",
        ] {
            std::fs::write(root.join(name), blocks(&heads, "")).unwrap();
        }
        std::fs::write(
            root.join(".gitattributes"),
            "*.e diff=empty\n*.g diff=group\n",
        )
        .unwrap();
        std::fs::write(root.join("sub/.gitattributes"), "*.txt diff=nested\n").unwrap();
        for (key, value) in [
            ("diff.default.xfuncname", "^SECTION .*$"),
            ("diff.nested.xfuncname", "^part (.*)$"),
            // a match of nothing, and a group that takes nothing
            ("diff.empty.xfuncname", "x*"),
            ("diff.group.xfuncname", "^()abc"),
        ] {
            ok(run("git", root, "C", &["config", key, value]));
        }
    };
    let change = |root: &Path| {
        for name in [
            "plain.txt",
            "sub/inner.txt",
            "sub/deeper/low.txt",
            "empty.e",
            "group.g",
        ] {
            std::fs::write(root.join(name), blocks(&heads, " changed")).unwrap();
        }
    };
    let dirs = pair(prepare, change);
    let headers = same_headers(&dirs, &["diff"], 20);
    let count = |end: &str| headers.iter().filter(|h| h.ends_with(end)).count();
    let each = locales().len();
    assert_eq!(count("@@ SECTION one"), 2 * each);
    // the file beside the nested attributes file, and the one below it
    assert_eq!(count("@@ two"), 6 * each);
}

#[test]
fn a_renamed_file_is_named_by_the_driver_git_takes() {
    let heads: [&[u8]; 3] = [b"def one(self):", b"sub two {", b"class Three"];
    let prepare = |root: &Path| {
        std::fs::write(
            root.join(".gitattributes"),
            "*.py diff=python\n*.pl diff=perl\n",
        )
        .unwrap();
        std::fs::write(root.join("old.py"), blocks(&heads, "")).unwrap();
        ok(run("git", root, "C", &["add", "."]));
        ok(run("git", root, "C", &["commit", "-qm", "first"]));
        std::fs::rename(root.join("old.py"), root.join("new.pl")).unwrap();
        std::fs::write(root.join("new.pl"), blocks(&heads, " changed")).unwrap();
    };
    let dirs = pair(prepare, |_| {});
    let args = ["log", "-p", "-n", "1"];
    let headers = same_headers(&dirs, &args, 3);
    let theirs = ok(run("git", dirs[0].path(), "C", &args));
    assert!(theirs.windows(11).any(|w| w == b"rename from"));
    assert!(headers.iter().any(|h| h.contains("def one")), "{headers:?}");
}

/// Which locale a text is matched in is what git makes of the
/// variables: the first that is set, when the system has that locale.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn a_text_is_utf8_in_the_locales_git_finds_installed() {
    // the second head is one only where lines are bytes
    let heads: [&[u8]; 2] = [b"def first(self):", b"def caf\xe9(self):"];
    let prepare = |root: &Path| {
        std::fs::write(root.join(".gitattributes"), "*.py diff=python\n").unwrap();
        std::fs::write(root.join("f.py"), blocks(&heads, "")).unwrap();
    };
    let change =
        |root: &Path| std::fs::write(root.join("f.py"), blocks(&heads, " changed")).unwrap();
    let dirs = pair(prepare, change);
    let cases: [&[(&str, &str)]; 16] = [
        &[],
        &[("LC_ALL", "C")],
        &[("LC_ALL", "POSIX")],
        &[("LC_ALL", "C.UTF-8")],
        &[("LC_ALL", "C.utf8")],
        &[("LANG", "C.UTF-8")],
        &[("LC_CTYPE", "C.UTF-8")],
        // not installed
        &[("LC_ALL", "xx_XX.UTF-8")],
        &[("LC_ALL", "xx_XX.UTF-8"), ("LANG", "C.UTF-8")],
        &[("LC_CTYPE", "xx_XX.UTF-8"), ("LANG", "C.UTF-8")],
        &[("LC_ALL", "UTF-8")],
        // one that is set to nothing is not set
        &[("LC_ALL", ""), ("LC_CTYPE", ""), ("LANG", "C.UTF-8")],
        &[("LC_ALL", "C"), ("LANG", "C.UTF-8")],
        &[("LC_ALL", "C.UTF-8"), ("LANG", "C")],
        &[("LC_CTYPE", "C"), ("LANG", "C.UTF-8")],
        &[("LC_CTYPE", "C.UTF-8"), ("LANG", "C")],
    ];
    let mut characters = Vec::new();
    for case in cases {
        let theirs = hunk_headers(&ok(run_in("git", dirs[0].path(), case, &["diff"])));
        let ours = hunk_headers(&ok(run_in("alt", dirs[1].path(), case, &["diff"])));
        assert_eq!(ours, theirs, "{case:?}");
        assert_eq!(theirs.len(), 2, "{case:?}");
        characters.push(theirs[1].ends_with("def first(self):"));
    }
    let expected = [0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 1, 0, 1, 0, 1].map(|utf8| utf8 == 1);
    assert_eq!(characters, expected);
}

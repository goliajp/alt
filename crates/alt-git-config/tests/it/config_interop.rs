//! Compares Config against `git config --list -z` on torture-syntax files
//! and against `git -C <repo> config --local --list -z` for includeIf
//! conditions evaluated inside a real repository.

use std::fs;
use std::process::Command;

use alt_git_config::{Config, IncludeContext};
use alt_testutil as common;

/// `git config --file <f> --list -z` → ordered `(key, value-or-None)`.
/// In `-z` output a valueless boolean has no `\n` separator.
fn git_list(args: &[&str]) -> Vec<(String, Option<String>)> {
    let out = Command::new("git").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    text.split_terminator('\0')
        .map(|record| match record.split_once('\n') {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (record.to_owned(), None),
        })
        .collect()
}

fn ours(config: &Config) -> Vec<(String, Option<String>)> {
    config
        .entries
        .iter()
        .map(|e| (e.display_key(), e.value.as_ref().map(|v| v.to_string())))
        .collect()
}

#[test]
fn torture_syntax_matches_git() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let file = tmp.path().join("cfg");
    fs::write(
        &file,
        "; leading comment\n\
         [core]\n\
         \tbare = false   # trailing comment\n\
         quoted = \"  spaces kept  \"\n\
         mixed = pre \"mi;d\" post   \n\
         escapes = a\\tb\\nc\\\\d\\\"e\n\
         continued = one \\\n   two\n\
         tabbed = a\tb\n\
         flag\n\
         empty =\n\
         [branch \"with space.and/dots\"]\n\
         merge = refs/heads/x\n\
         [Legacy.MixedCase]\n\
         Key = value\n\
         [core]\n\
         bare = true\n",
    )
    .unwrap();

    let config = Config::load(&file, &IncludeContext::default()).unwrap();
    assert_eq!(
        ours(&config),
        git_list(&["config", "--file", file.to_str().unwrap(), "--list", "-z"])
    );

    // value semantics against git --type=...
    let get_typed = |ty: &str, key: &str| -> String {
        let out = Command::new("git")
            .args([
                "config",
                "--file",
                file.to_str().unwrap(),
                "--type",
                ty,
                key,
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(
        config
            .get_bool("core", None, "flag")
            .unwrap()
            .unwrap()
            .to_string(),
        get_typed("bool", "core.flag")
    );
    assert_eq!(
        config
            .get_bool("core", None, "bare")
            .unwrap()
            .unwrap()
            .to_string(),
        get_typed("bool", "core.bare")
    );
}

#[test]
fn includes_and_include_if_match_git() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    common::make_repo(repo, "sha1");

    fs::write(repo.join(".git/extra.inc"), "[marker]\nincluded = yes\n").unwrap();
    fs::write(repo.join(".git/cond.inc"), "[marker]\nconditional = yes\n").unwrap();
    fs::write(repo.join(".git/branch.inc"), "[marker]\nonmain = yes\n").unwrap();
    let local = fs::read_to_string(repo.join(".git/config")).unwrap();
    fs::write(
        repo.join(".git/config"),
        format!(
            "{local}\
             [include]\n\
             path = extra.inc\n\
             path = missing.inc\n\
             [includeIf \"gitdir:**/{name}/**\"]\n\
             path = cond.inc\n\
             [includeIf \"gitdir:/nowhere/\"]\n\
             path = cond.inc\n\
             [includeIf \"onbranch:ma*\"]\n\
             path = branch.inc\n\
             [includeIf \"futurecond:whatever\"]\n\
             path = cond.inc\n",
            name = repo.file_name().unwrap().to_str().unwrap(),
        ),
    )
    .unwrap();

    let ctx = IncludeContext {
        git_dir: Some(repo.join(".git").canonicalize().unwrap()),
        branch: Some("main".into()),
        home: None,
    };
    let config = Config::load(&repo.join(".git/config"), &ctx).unwrap();
    // --includes: explicit-file modes default to NOT processing includes
    let git_truth = git_list(&[
        "-C",
        repo.to_str().unwrap(),
        "config",
        "--local",
        "--includes",
        "--list",
        "-z",
    ]);
    assert_eq!(ours(&config), git_truth);

    // sanity on what the comparison just proved
    let m = |k: &str| config.get_str("marker", None, k).map(|v| v.to_string());
    assert_eq!(m("included").as_deref(), Some("yes"), "plain include");
    assert_eq!(m("conditional").as_deref(), Some("yes"), "gitdir true");
    assert_eq!(m("onmain").as_deref(), Some("yes"), "onbranch glob");
}

#[test]
fn files_at_the_edges_of_the_syntax_are_read_or_refused_as_git_does() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let file = tmp.path().join("cfg");
    let path = file.to_str().unwrap();
    for text in [
        &b"\xef\xbb\xbf[a]\nx = 1\n"[..],
        b"x = 1\n[a]\ny=2\n",
        b"[a]\nflag # said\ny=2\n",
        b"[a]\nflag;said\n",
        b"[a]\r\nflag\r\nx = 1\r\ny = a\\\r\nb\r\n",
        b"[a]\nx = 1\\",
        b"[a]\nx = \x0b1\x0c\n",
        b"[a \"b\" ]\nx=1\n",
        b"[]\nx=1\n",
        b"[a\t\"b\"]\nx=1\n",
        b"[a]\nx = \"a\nb\"\n",
        b"[a \"b\\\"c\\\\d\\e\"]\nx=1\n",
        b"[A.B.c]\nx=1\n",
        b"[a]\nx_y=1\n",
        b"[a]\n1x=1\n",
        b"[a]\nx = 1",
        b"[a] x = 1\n",
        b"[a] # said\nx=1\n",
        b"[ a ]\nx=1\n",
        b"[a \"b\nc\"]\nx=1\n",
        b"[a]\nx=\n",
        b"[a]\nx y = 1\n",
        b"[core\n",
        b"[a]\n\nx = bad\\z\n",
        b"[a]\nx = 1\n[b]\ny = \"open\n",
    ] {
        fs::write(&file, text).unwrap();
        let shown = String::from_utf8_lossy(text).into_owned();
        let git = Command::new("git")
            .args(["config", "--file", path, "--list", "-z"])
            .output()
            .unwrap();
        match Config::load(&file, &IncludeContext::default()) {
            Ok(config) => {
                assert!(git.status.success(), "git refuses {shown:?}");
                let theirs = git_list(&["config", "--file", path, "--list", "-z"]);
                assert_eq!(ours(&config), theirs, "{shown:?}");
            }
            Err(refused) => {
                let said = String::from_utf8_lossy(&git.stderr).into_owned();
                assert!(!git.status.success(), "git reads {shown:?}: {refused}");
                assert_eq!(format!("fatal: {refused}\n"), said, "{shown:?}");
            }
        }
    }
}

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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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

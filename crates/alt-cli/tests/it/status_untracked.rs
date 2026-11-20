//! `alt status` shows untracked files as git does: a directory with
//! nothing tracked in it as one line by default, every file with `-uall`,
//! none with `-uno`. `alt add` still stages the files themselves.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    crate::alt_command()
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("XDG_CONFIG_HOME", repo.join("no-such-config"))
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

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// One commit, then untracked files and directories of several shapes.
fn scenario(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    write(root, ".gitignore", "*.log\n/build/\n");
    write(root, "top.txt", "top\n");
    write(root, "t/keep.txt", "keep\n");
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "base"]));
    write(root, "new.txt", "new\n");
    write(root, "u1/a.txt", "a\n");
    write(root, "u1/deep/b.txt", "b\n");
    std::fs::create_dir_all(root.join("empty/nested")).unwrap();
    write(root, "onlyign/x.log", "ignored\n");
    write(root, "mixed/x.log", "ignored\n");
    write(root, "mixed/deep/y.txt", "counts\n");
    write(root, "t/new.txt", "in a tracked directory\n");
    write(
        root,
        "t/sub/x.txt",
        "in an untracked directory of a tracked one\n",
    );
    write(root, "build/out.o", "ignored\n");
}

fn pair() -> (alt_os::temp::TempDir, alt_os::temp::TempDir) {
    let (a, g) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    ok(alt(a.path(), &["init", "."]));
    ok(git(g.path(), &["init", "-q", "-b", "main"]));
    scenario(a.path(), &|args| alt(a.path(), args));
    scenario(g.path(), &|args| git(g.path(), args));
    (a, g)
}

/// The untracked paths git lists with these `status` options.
fn git_untracked(g: &Path, options: &[&str]) -> Vec<String> {
    let args = [&["status", "--porcelain"][..], options].concat();
    ok(git(g, &args))
        .lines()
        .filter_map(|line| line.strip_prefix("?? "))
        .map(str::to_owned)
        .collect()
}

/// The `untracked` member a `status --json` document has for `paths`.
fn json_member(paths: &[String]) -> String {
    let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
    format!("\"untracked\":[{}]", quoted.join(","))
}

#[test]
fn every_untracked_files_mode_lists_what_git_lists() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    let spellings: [&[&str]; 13] = [
        &["-utrue"],
        &["-ufalse"],
        &["--untracked-files=yes"],
        &["--untracked-files=off"],
        &["-u1"],
        &[],
        &["-unormal"],
        &["--untracked-files=normal"],
        &["-uall"],
        &["-u"],
        &["--untracked-files"],
        &["-uno"],
        &["--untracked-files=no"],
    ];
    for options in spellings {
        let want = git_untracked(g, options);
        let json = ok(alt(a, &[&["status", "--json"][..], options].concat()));
        assert!(json.contains(&json_member(&want)), "{options:?}: {json}");
        let human = ok(alt(a, &[&["status"][..], options].concat()));
        let section = match want.is_empty() {
            true => "nothing to commit, working tree clean\n".to_owned(),
            false => format!("Untracked files:\n\t{}\n", want.join("\n\t")),
        };
        assert_eq!(human, format!("On branch main\n{section}"), "{options:?}");
    }
    assert_eq!(
        git_untracked(g, &[]),
        ["mixed/", "new.txt", "t/new.txt", "t/sub/", "u1/"]
    );
    assert_eq!(git_untracked(g, &["-uall"]).len(), 6);
    assert!(!alt(a, &["status", "-usome"]).status.success());
}

#[test]
fn nothing_but_untracked_files_left_out_reads_as_clean() {
    let (a, _g) = pair();
    let a = a.path();
    assert_eq!(
        ok(alt(a, &["status", "-uno"])),
        "On branch main\nnothing to commit, working tree clean\n"
    );
}

#[test]
fn an_empty_untracked_files_mode_shows_none_as_for_git() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    assert!(!git_untracked(g, &["-uno"]).is_empty() || git_untracked(g, &[]).len() > 1);
    assert!(git_untracked(g, &["--untracked-files="]).is_empty());
    let none = ok(alt(a, &["status", "--untracked-files="]));
    assert_eq!(none, ok(alt(a, &["status", "-uno"])));
    assert!(ok(alt(a, &["status"])).contains("Untracked files:"));
    assert!(!none.contains("Untracked files:"));
}

#[test]
fn add_stages_the_files_of_a_directory_shown_as_one_line() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    for args in [&["add", "u1"][..], &["add", "mixed/"]] {
        ok(alt(a, args));
        ok(git(g, args));
    }
    assert_eq!(
        ok(git(g, &["status", "--porcelain"])),
        "A  mixed/deep/y.txt\nA  u1/a.txt\nA  u1/deep/b.txt\n?? new.txt\n?? t/new.txt\n?? t/sub/\n"
    );
    assert_eq!(
        ok(alt(a, &["status"])),
        "On branch main\nChanges to be committed:\n\tnew file:   mixed/deep/y.txt\n\tnew file:   u1/a.txt\n\tnew file:   u1/deep/b.txt\nUntracked files:\n\tnew.txt\n\tt/new.txt\n\tt/sub/\n"
    );
    // a file added to a directory that now has tracked files is listed by itself
    write(a, "u1/later.txt", "later\n");
    write(g, "u1/later.txt", "later\n");
    let want = git_untracked(g, &[]);
    assert!(want.contains(&"u1/later.txt".to_owned()));
    assert!(ok(alt(a, &["status", "--json"])).contains(&json_member(&want)));
}

/// Repositories of their own in directories the index knows nothing in.
fn embed(root: &Path) {
    for repo in ["emb", "outer/inner", "t/emb", "build/emb", "alone/deep/emb"] {
        std::fs::create_dir_all(root.join(repo)).unwrap();
        ok(git(&root.join(repo), &["init", "-q", "-b", "main"]));
        write(root, &format!("{repo}/file.txt"), "its own\n");
    }
    ok(git(&root.join("emb"), &["add", "."]));
    ok(git(&root.join("emb"), &["commit", "-q", "-m", "its own"]));
    write(root, "outer/file.txt", "beside a repository\n");
}

#[test]
fn a_repository_in_an_untracked_directory_shows_as_git_shows_it() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    embed(a);
    embed(g);
    let modes: [&[&str]; 3] = [&[], &["-uall"], &["-uno"]];
    for options in modes {
        let want = git_untracked(g, options);
        let json = ok(alt(a, &[&["status", "--json"][..], options].concat()));
        assert!(json.contains(&json_member(&want)), "{options:?}: {json}");
    }
    let normal = git_untracked(g, &[]);
    for shown in ["alone/", "emb/", "outer/", "t/emb/"] {
        assert!(normal.contains(&shown.to_owned()), "{shown}: {normal:?}");
    }
    let all = git_untracked(g, &["-uall"]);
    for shown in ["alone/deep/emb/", "emb/", "outer/file.txt", "outer/inner/"] {
        assert!(all.contains(&shown.to_owned()), "{shown}: {all:?}");
    }
    // a repository with no commit cannot be staged, nor anything with it
    let before = ok(alt(a, &["status"]));
    for named in [".", "outer", "t/emb"] {
        // with git's two lines and its status
        let theirs = git(g, &["add", named]);
        let refused = alt(a, &["add", named]);
        assert_eq!(theirs.status.code(), Some(128), "{named}");
        assert_eq!(refused.status.code(), Some(128), "{named}");
        let said = String::from_utf8_lossy(&theirs.stderr).into_owned();
        assert!(said.ends_with("fatal: adding files failed\n"), "{said}");
        let why = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(why.starts_with(&said), "{why}");
        assert_eq!(ok(alt(a, &["status"])), before, "{named}");
    }
    assert!(git_untracked(g, &[]).contains(&"emb/".to_owned()));
    // a path inside a repository stages nothing and fails nothing
    for named in ["emb/file.txt", "outer/inner/file.txt"] {
        ok(git(g, &["add", named]));
        ok(alt(a, &["add", named]));
        assert_eq!(ok(alt(a, &["status"])), before, "{named}");
    }
    assert!(!alt(a, &["add", "emb-not-there/file.txt"]).status.success());
}

#[test]
fn untracked_files_beside_a_repository_in_a_tracked_directory_are_listed() {
    let (a, g) = pair();
    for root in [a.path(), g.path()] {
        // `t` has a tracked file
        ok(git(&root.join("t"), &["init", "-q", "-b", "main"]));
        write(root, "t/beside.txt", "beside\n");
        write(root, "t/keep.txt", "changed\n");
    }
    let (a, g) = (a.path(), g.path());
    for options in [&[][..], &["-uall"]] {
        let want = git_untracked(g, options);
        assert!(want.contains(&"t/beside.txt".to_owned()), "{want:?}");
        let json = ok(alt(a, &[&["status", "--json"][..], options].concat()));
        assert!(json.contains(&json_member(&want)), "{options:?}: {json}");
    }
    assert!(ok(alt(a, &["status"])).contains("modified:   t/keep.txt"));
}

#[test]
fn the_configured_untracked_files_mode_is_the_default_as_it_is_for_git() {
    let (a, g) = pair();
    let (a, g) = (a.path(), g.path());
    let set = |value: &str| {
        ok(git(g, &["config", "status.showUntrackedFiles", value]));
        let config = format!("[status]\n\tshowUntrackedFiles = {value}\n");
        write(a, ".alt/git-import/config", &config);
    };
    let modes: [&[&str]; 4] = [&[], &["-uall"], &["-uno"], &["-unormal"]];
    for value in ["no", "normal", "all", "true", "false"] {
        set(value);
        for options in modes {
            let want = git_untracked(g, options);
            let json = ok(alt(a, &[&["status", "--json"][..], options].concat()));
            assert!(
                json.contains(&json_member(&want)),
                "{value} {options:?}: {json}"
            );
        }
    }
    set("all");
    assert_eq!(git_untracked(g, &[]).len(), 6);
    set("no");
    assert!(git_untracked(g, &[]).is_empty());
    set("some");
    assert!(!git(g, &["status"]).status.success());
    let refused = alt(a, &["status"]);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).to_lowercase();
    assert!(
        said.contains("invalid untracked files mode 'some'"),
        "{said}"
    );
}

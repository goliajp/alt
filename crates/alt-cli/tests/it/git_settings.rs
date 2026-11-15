//! The user's and the system's git configuration apply beneath the
//! repository's own, as in git.

use std::path::Path;
use std::process::{Command, Output};

fn at_home(tool: &str, home: &Path, repo: &Path, args: &[&str]) -> Output {
    let mut command = match tool {
        "git" => {
            let mut git = Command::new("git");
            git.arg("-C").arg(repo);
            git
        }
        _ => {
            let mut alt = Command::new(env!("CARGO_BIN_EXE_alt"));
            alt.current_dir(repo).env("ALT_NO_DAEMON", "1");
            alt
        }
    };
    command
        .env("HOME", home)
        .env("GIT_CONFIG_SYSTEM", home.join("system"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_CONFIG_NOSYSTEM")
        .env_remove("GIT_CONFIG_GLOBAL")
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

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn notes(mark: &str) -> String {
    let mut out = String::new();
    for head in ["section one", "  section two"] {
        out.push_str(head);
        out.push('\n');
        for line in 0..12 {
            let mark = if line == 9 { mark } else { "" };
            out.push_str(&format!(" item {line}{mark}\n"));
        }
    }
    out
}

#[test]
fn the_users_and_the_systems_configuration_apply_beneath_the_repositorys() {
    for own in [None, Some("false")] {
        let (home, src, dst) = (
            alt_os::temp::tempdir().unwrap(),
            alt_os::temp::tempdir().unwrap(),
            alt_os::temp::tempdir().unwrap(),
        );
        let (home, src, dst) = (home.path(), src.path(), dst.path());
        // the system says one thing, the user another, through an include
        write(home, "system", "[core]\n\tautocrlf = input\n");
        write(home, ".gitconfig", "[include]\n\tpath = more.inc\n");
        let more = "[core]\n\tautocrlf = true\n\tattributesFile = ~/attrs\n";
        write(home, "more.inc", more);
        let driver = "[diff \"custom\"]\n\txfuncname = \"^ *(section .*)$\"\n";
        write(home, ".config/git/config", driver);
        write(home, "attrs", "*.x diff=custom\n");
        let git = |args: &[&str]| ok(at_home("git", home, src, args));
        git(&["init", "-q", "-b", "main", "."]);
        if let Some(value) = own {
            git(&["config", "core.autocrlf", value]);
        }
        write(src, "a.txt", "one\ntwo\n");
        write(src, "notes.x", &notes(""));
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        // checked out afresh, so that git has converted the files
        for name in ["a.txt", "notes.x"] {
            std::fs::remove_file(src.join(name)).unwrap();
        }
        git(&["checkout", "-q", "--", "."]);
        ok(at_home(
            "alt",
            home,
            src,
            &["import", dst.to_str().unwrap()],
        ));

        let expected: &[u8] = match own {
            None => b"one\r\ntwo\r\n",
            Some(_) => b"one\ntwo\n",
        };
        assert_eq!(std::fs::read(src.join("a.txt")).unwrap(), expected);
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), expected);
        let status = ok(at_home("alt", home, dst, &["status"]));
        assert!(status.contains("nothing to commit"), "{status}");

        // the user's attributes file names the user's diff driver
        let ending = if own.is_none() { "\r\n" } else { "\n" };
        for root in [src, dst] {
            write(root, "notes.x", &notes(" changed").replace('\n', ending));
        }
        let headers = |patch: String| -> Vec<String> {
            let lines = patch.lines().filter(|l| l.starts_with("@@"));
            lines.map(str::to_owned).collect()
        };
        let theirs = headers(git(&["diff"]));
        let ours = headers(ok(at_home("alt", home, dst, &["diff"])));
        assert_eq!(ours, theirs);
        assert!(ours[1].ends_with("@@ section two"), "{ours:?}");
    }
}

#[test]
fn the_environment_can_turn_the_outer_configuration_off() {
    let (home, src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (home, src, dst) = (home.path(), src.path(), dst.path());
    write(home, "system", "[core]\n\tautocrlf = true\n");
    write(home, ".gitconfig", "[core]\n\tautocrlf = true\n");
    let off = |tool: &str, repo: &Path, args: &[&str]| {
        let program = match tool {
            "git" => "git".into(),
            _ => std::path::PathBuf::from(env!("CARGO_BIN_EXE_alt")),
        };
        let run = Command::new(program)
            .current_dir(repo)
            .env("HOME", home)
            .env("GIT_CONFIG_SYSTEM", home.join("system"))
            .env("GIT_CONFIG_NOSYSTEM", "true")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("ALT_NO_DAEMON", "1")
            .env("GIT_AUTHOR_NAME", "tester")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "tester")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .args(args)
            .output();
        ok(run.unwrap())
    };
    off("git", src, &["init", "-q", "-b", "main", "."]);
    write(src, "a.txt", "one\ntwo\n");
    off("git", src, &["add", "."]);
    off("git", src, &["commit", "-qm", "base"]);
    std::fs::remove_file(src.join("a.txt")).unwrap();
    off("git", src, &["checkout", "-q", "--", "."]);
    off("alt", src, &["import", dst.to_str().unwrap()]);
    assert_eq!(std::fs::read(src.join("a.txt")).unwrap(), b"one\ntwo\n");
    assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"one\ntwo\n");
}

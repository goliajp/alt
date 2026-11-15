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
            let mut alt = crate::alt_command();
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

/// `alt` in `repo` through the repository's daemon, which this starts
/// when none runs and which ends by itself soon after its last command.
fn through_daemon(home: &Path, repo: &Path, args: &[&str]) -> Output {
    crate::alt_command()
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_SYSTEM", home.join("system"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_CONFIG_NOSYSTEM")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("ALT_NO_DAEMON")
        .env("ALT_DAEMON_IDLE_MS", "1500")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_command_through_the_daemon_reads_the_configuration_of_its_caller() {
    let dirs = [0; 3].map(|_| alt_os::temp::tempdir().unwrap());
    let (first, second, repo) = (dirs[0].path(), dirs[1].path(), dirs[2].path());
    let driver = |pattern: &str| format!("[diff \"custom\"]\n\txfuncname = \"{pattern}\"\n");
    write(first, ".gitconfig", &driver("^(section .*)$"));
    write(second, ".gitconfig", &driver("^ *(section .*)$"));
    let alt = |args: &[&str]| ok(at_home("alt", first, repo, args));
    alt(&["init", "."]);
    write(repo, ".gitattributes", "*.x diff=custom\n");
    write(repo, "notes.x", &notes(""));
    alt(&["add", "."]);
    alt(&["commit", "-m", "base"]);
    write(repo, "notes.x", &notes("!"));

    let socket = repo.join(".alt/daemon.sock");
    for (home, section) in [(first, "section one"), (second, "section two")] {
        let served = ok(through_daemon(home, repo, &["diff"]));
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "no daemon served the command"
        );
        let head = served.lines().rfind(|l| l.starts_with("@@")).unwrap();
        assert!(head.ends_with(section), "{head}");
        assert_eq!(served, ok(at_home("alt", home, repo, &["diff"])));
    }
    // the daemon this started is gone before the test is
    let begun = std::time::Instant::now();
    while socket.exists() {
        assert!(begun.elapsed().as_secs() < 20, "the daemon stays");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn the_alt_the_tests_run_does_not_read_the_configuration_of_the_machine() {
    let dirs = [0; 2].map(|_| alt_os::temp::tempdir().unwrap());
    let (home, repo) = (dirs[0].path(), dirs[1].path());
    // a machine whose user and system both name a driver for these files
    let driver = "[diff \"custom\"]\n\txfuncname = \"^ *(section two)$\"\n";
    write(home, ".gitconfig", driver);
    write(home, "system", driver);
    let alt = |args: &[&str]| {
        let mut alt = crate::alt_command();
        alt.current_dir(repo)
            .env("HOME", home)
            .env("ALT_NO_DAEMON", "1")
            .env_remove("XDG_CONFIG_HOME")
            .env("GIT_AUTHOR_NAME", "tester")
            .env("GIT_AUTHOR_EMAIL", "t@e");
        ok(alt.args(args).output().unwrap())
    };
    alt(&["init", "."]);
    write(repo, ".gitattributes", "*.x diff=custom\n");
    write(repo, "notes.x", &notes(""));
    alt(&["add", "."]);
    alt(&["commit", "-m", "base"]);
    write(repo, "notes.x", &notes("!"));
    let shown = alt(&["diff"]);
    let head = shown.lines().rfind(|l| l.starts_with("@@")).unwrap();
    // the usual rule, a line that begins with a letter: not the driver
    assert!(head.ends_with("section one"), "{head}");
}

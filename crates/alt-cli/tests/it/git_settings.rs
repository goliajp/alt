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

/// A git repository and its import into alt, each with `notes.x`
/// committed and then changed, and a home directory for the two tools.
struct Twin {
    dirs: [alt_os::temp::TempDir; 3],
}

/// What a variable is for one command: set to this, or not set.
type Env<'a> = &'a [(&'a str, Option<&'a str>)];

const SECTION_TWO: &str = "[diff \"custom\"]\n\txfuncname = \"^ *(section two)$\"\n";

impl Twin {
    /// With `in_tree`, the files in the tree name the driver `custom`
    /// for `notes.x`.
    fn new(in_tree: bool) -> Twin {
        let twin = Twin {
            dirs: [0; 3].map(|_| alt_os::temp::tempdir().unwrap()),
        };
        ok(twin.run("git", &["init", "-q", "-b", "main", "."], &[]));
        if in_tree {
            write(twin.root("git"), ".gitattributes", "*.x diff=custom\n");
        }
        write(twin.root("git"), "notes.x", &notes(""));
        ok(twin.run("git", &["add", "."], &[]));
        ok(twin.run("git", &["commit", "-qm", "base"], &[]));
        let target = twin.root("alt").to_str().unwrap().to_owned();
        ok(at_home(
            "alt",
            twin.home(),
            twin.root("git"),
            &["import", &target],
        ));
        for tool in ["git", "alt"] {
            write(twin.root(tool), "notes.x", &notes(" changed"));
        }
        twin
    }

    fn home(&self) -> &Path {
        self.dirs[0].path()
    }

    fn root(&self, tool: &str) -> &Path {
        self.dirs[if tool == "git" { 1 } else { 2 }].path()
    }

    fn run(&self, tool: &str, args: &[&str], env: Env) -> Output {
        let mut command = match tool {
            "git" => Command::new("git"),
            _ => crate::alt_command(),
        };
        command
            .current_dir(self.root(tool))
            .env("HOME", self.home())
            .env("GIT_CONFIG_SYSTEM", self.home().join("system"))
            .env("ALT_NO_DAEMON", "1")
            .env("GIT_AUTHOR_NAME", "tester")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "tester")
            .env("GIT_COMMITTER_EMAIL", "t@e");
        for name in [
            "XDG_CONFIG_HOME",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
        ] {
            command.env_remove(name);
        }
        for (name, value) in env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command.args(args).output().unwrap()
    }

    /// What follows the last `@@` of the patch both tools print, which
    /// they must print alike.
    fn section(&self, args: &[&str], env: Env) -> String {
        let last = |tool: &str| {
            let patch = ok(self.run(tool, args, env));
            let head = patch.lines().rfind(|l| l.starts_with("@@")).unwrap();
            head.rsplit("@@").next().unwrap().trim().to_owned()
        };
        let (ours, theirs) = (last("alt"), last("git"));
        assert_eq!(ours, theirs, "{args:?} with {env:?}");
        ours
    }

    /// The error both tools end `args` with; the message of alt.
    fn refused(&self, args: &[&str], env: Env) -> String {
        let theirs = self.run("git", args, env);
        let ours = self.run("alt", args, env);
        assert!(!theirs.status.success(), "git runs {args:?} with {env:?}");
        assert!(!ours.status.success(), "alt runs {args:?} with {env:?}");
        String::from_utf8_lossy(&ours.stderr).into_owned()
    }
}

#[test]
fn a_configuration_file_the_user_may_not_read_is_passed_over_and_a_broken_one_named() {
    use std::os::unix::fs::PermissionsExt;
    let twin = Twin::new(true);
    let file = twin.home().join(".gitconfig");
    write(twin.home(), ".gitconfig", SECTION_TWO);
    assert_eq!(twin.section(&["diff"], &[]), "section two");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let section = twin.section(&["diff"], &[]);
    // the superuser reads any file
    if std::fs::read(&file).is_err() {
        assert_eq!(section, "section one");
    }
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    write(
        twin.home(),
        ".gitconfig",
        "[core]\n\tquotepath = false\n[diff\n",
    );
    let message = twin.refused(&["diff"], &[]);
    let named = format!("bad config line 3 in file {}", file.display());
    assert!(message.contains(&named), "{message}");
    let theirs = twin.run("git", &["diff"], &[]);
    assert!(String::from_utf8_lossy(&theirs.stderr).contains(&named));
}

#[test]
fn a_variable_set_to_nothing_names_no_configuration_file() {
    let twin = Twin::new(true);
    write(twin.home(), ".gitconfig", SECTION_TWO);
    assert_eq!(twin.section(&["diff"], &[]), "section two");
    let none = &[("GIT_CONFIG_GLOBAL", Some(""))];
    assert_eq!(twin.section(&["diff"], none), "section one");
    std::fs::remove_file(twin.home().join(".gitconfig")).unwrap();
    write(twin.home(), "system", SECTION_TWO);
    assert_eq!(twin.section(&["diff"], &[]), "section two");
    let none = &[("GIT_CONFIG_SYSTEM", Some(""))];
    assert_eq!(twin.section(&["diff"], none), "section one");
}

#[test]
fn the_file_in_the_home_directory_is_read_after_the_one_in_the_config_directory() {
    let twin = Twin::new(true);
    let item = "[diff \"custom\"]\n\txfuncname = \"^ *(item 3)$\"\n";
    write(twin.home(), "xdg/git/config", SECTION_TWO);
    let xdg = twin.home().join("xdg");
    let env = &[("XDG_CONFIG_HOME", xdg.to_str())];
    assert_eq!(twin.section(&["diff"], env), "section two");
    write(twin.home(), ".gitconfig", item);
    assert_eq!(twin.section(&["diff"], env), "item 3");
    // and the usual place of the first is beneath the home directory
    std::fs::remove_file(twin.home().join(".gitconfig")).unwrap();
    write(twin.home(), ".config/git/config", item);
    assert_eq!(twin.section(&["diff"], &[]), "item 3");
    assert_eq!(twin.section(&["diff"], env), "section two");
}

#[test]
fn without_a_home_directory_no_file_is_found_there_and_a_path_into_it_is_an_error() {
    let twin = Twin::new(true);
    write(twin.home(), ".gitconfig", SECTION_TWO);
    write(twin.home(), ".config/git/config", SECTION_TWO);
    let homeless = &[("HOME", None)];
    assert_eq!(twin.section(&["diff"], homeless), "section one");
    for tool in ["git", "alt"] {
        ok(twin.run(tool, &["config", "core.attributesFile", "~/attrs"], &[]));
    }
    assert_eq!(twin.section(&["diff"], &[]), "section two");
    let message = twin.refused(&["diff"], homeless);
    let said = "failed to expand user dir in: '~/attrs'";
    assert!(message.contains(said), "{message}");

    let twin = Twin::new(true);
    for tool in ["git", "alt"] {
        ok(twin.run(tool, &["config", "include.path", "~/more"], &[]));
    }
    write(twin.home(), "more", SECTION_TWO);
    assert_eq!(twin.section(&["diff"], &[]), "section two");
    let message = twin.refused(&["diff"], homeless);
    let said = "could not expand include path '~/more'";
    assert!(message.contains(said), "{message}");
}

#[test]
fn the_users_attributes_file_is_read_through_a_symlink() {
    let twin = Twin::new(false);
    let config = format!("{SECTION_TWO}[core]\n\tattributesFile = ~/attrs\n");
    write(twin.home(), ".gitconfig", &config);
    assert_eq!(twin.section(&["diff"], &[]), "section one");
    write(twin.home(), "kept/attributes", "*.x diff=custom\n");
    std::os::unix::fs::symlink("kept/attributes", twin.home().join("attrs")).unwrap();
    assert_eq!(twin.section(&["diff"], &[]), "section two");
}

#[test]
fn a_file_included_on_a_branch_applies_to_the_patches_of_a_log() {
    let twin = Twin::new(true);
    let config = "[includeIf \"onbranch:main\"]\n\tpath = main.inc\n\
                  [includeIf \"onbranch:other\"]\n\tpath = other.inc\n";
    write(twin.home(), ".gitconfig", config);
    write(twin.home(), "main.inc", SECTION_TWO);
    write(
        twin.home(),
        "other.inc",
        "[diff \"custom\"]\n\txfuncname = \"^ *(item 3)$\"\n",
    );
    ok(twin.run("git", &["commit", "-qam", "changed"], &[]));
    ok(twin.run("alt", &["add", "."], &[]));
    ok(twin.run("alt", &["commit", "-m", "changed"], &[]));
    assert_eq!(twin.section(&["log", "-p", "-n", "1"], &[]), "section two");
    assert_eq!(twin.section(&["show"], &[]), "section two");
}

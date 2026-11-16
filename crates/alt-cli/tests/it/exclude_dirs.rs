//! A directory where an ignore file is looked for. git reads none of a
//! `.gitignore` that is one, and ends every command that sets up its
//! ignore rules where the user's file or the repository's own is one:
//! `status` whatever it shows of untracked files, `diff --cached`, `add`,
//! `commit` and `switch` to a branch, not `diff`, `log`, `show`, `branch`
//! or `switch -c`. Each command here is run by git, by `alt` and by its
//! daemon, which answer alike.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::scan_daemon::{Stop, daemon_pid, write};

/// `program` in `at`, a directory of the tree `root`, with the settings
/// of its user in a directory beside the trees, the same for them all.
fn command(program: &str, root: &Path, at: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(at)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("XDG_CONFIG_HOME", root.join("../config"))
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args);
    command
}

fn git(root: &Path, at: &Path, args: &[&str]) -> Output {
    command("git", root, at, args).output().unwrap()
}

fn alt(root: &Path, at: &Path, args: &[&str], daemon: bool) -> Output {
    let mut command = command(env!("CARGO_BIN_EXE_alt"), root, at, args);
    match daemon {
        true => command
            .env_remove("ALT_NO_DAEMON")
            .env("ALT_DAEMON_IDLE_MS", "60000"),
        false => command.env("ALT_NO_DAEMON", "1"),
    };
    command.output().unwrap()
}

/// Whether a command succeeded and, where it did not, its exit status and
/// the line it ended with, the tree and its control directory left out.
fn said(output: &Output, root: &Path, control: &str) -> (bool, Option<i32>, String) {
    let root = root.canonicalize().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).replace(root.to_str().unwrap(), "<root>");
    let fatal = stderr.lines().find(|line| line.starts_with("fatal:"));
    let fatal = fatal.unwrap_or_default().replace(control, "<control>");
    let ok = output.status.success();
    (ok, output.status.code().filter(|_| !ok), fatal)
}

/// The trees of `alt` run directly, of `alt` by its daemon and of git,
/// each with two commits on `main`, one more on `other`, a changed file,
/// a new one and a staged one.
fn trees(dir: &Path) -> ([PathBuf; 3], Stop) {
    let trees = ["direct", "served", "git"].map(|tree| dir.join(tree));
    for (root, tool) in trees.iter().zip(["alt", "alt", "git"]) {
        let step = |args: &[&str]| {
            let done = match tool {
                "alt" => alt(root, root, args, false),
                _ => git(root, root, args),
            };
            assert!(done.status.success(), "{tool} {args:?}");
        };
        write(root, "a.txt", "a\n");
        write(root, "sub/s.txt", "s\n");
        match tool {
            "alt" => step(&["init"]),
            _ => step(&["init", "-q", "-b", "main"]),
        }
        step(&["add", "a.txt", "sub/s.txt"]);
        step(&["commit", "-m", "base"]);
        step(&["branch", "other"]);
        write(root, "b.txt", "b\n");
        step(&["add", "b.txt"]);
        step(&["commit", "-m", "second"]);
        write(root, "a.txt", "changed\n");
        write(root, "new.txt", "new\n");
        write(root, "staged.txt", "staged\n");
        step(&["add", "staged.txt"]);
    }
    assert!(
        alt(&trees[1], &trees[1], &["status"], true)
            .status
            .success()
    );
    let stop = Stop(daemon_pid(&trees[1]));
    (trees, stop)
}

/// The commands, each in the directory it is run in. Those that change
/// something come last, in an order in which each finds what it needs
/// whether or not the ones before it were ended.
const COMMANDS: [(&str, &[&str]); 16] = [
    ("", &["status"]),
    ("", &["status", "-uno"]),
    ("", &["status", "-uall"]),
    ("sub", &["status"]),
    ("", &["diff"]),
    ("", &["diff", "--cached"]),
    ("sub", &["diff", "--cached"]),
    ("", &["log", "-n", "1"]),
    ("", &["show", "HEAD"]),
    ("", &["branch", "side"]),
    ("", &["add", "new.txt"]),
    ("", &["add", "."]),
    ("", &["commit", "-m", "third"]),
    ("", &["commit", "--amend", "-m", "third again"]),
    ("", &["switch", "-c", "fresh"]),
    ("", &["switch", "other"]),
];

/// Runs every command with `broken` done to each tree, a function of the
/// tree and its control directory, and returns what `alt` said to each.
fn through(broken: impl Fn(&Path, &str)) -> Vec<(bool, Option<i32>, String)> {
    let dir = alt_os::temp::tempdir().unwrap();
    let (trees, _stop) = trees(dir.path());
    let controls = [".alt/git-import", ".alt/git-import", ".git"];
    for (root, control) in trees.iter().zip(controls) {
        broken(root, control);
    }
    let mut all = Vec::new();
    for (sub, args) in COMMANDS {
        let [direct, served, with_git] = &trees;
        let direct = said(
            &alt(direct, &direct.join(sub), args, false),
            direct,
            controls[0],
        );
        let by_daemon = said(
            &alt(served, &served.join(sub), args, true),
            served,
            controls[1],
        );
        let git = said(
            &git(with_git, &with_git.join(sub), args),
            with_git,
            controls[2],
        );
        assert_eq!(by_daemon, direct, "{args:?} in {sub:?}");
        assert_eq!(direct, git, "{args:?} in {sub:?}");
        all.push(direct);
    }
    all
}

fn configure(root: &Path, control: &str, excludes: &str) {
    let config = root.join(control).join("config");
    let mut text = std::fs::read_to_string(&config).unwrap_or_default();
    text.push_str(&format!("[core]\n\texcludesFile = {excludes}\n"));
    write(root, &format!("{control}/config"), &text);
}

/// Which of the commands were ended, all of them with `fatal`.
fn ended(all: &[(bool, Option<i32>, String)], fatal: &str) -> Vec<String> {
    let mut ended = Vec::new();
    for ((sub, args), (ok, code, line)) in COMMANDS.iter().zip(all) {
        if !ok {
            assert_eq!((*code, line.as_str()), (Some(128), fatal), "{args:?}");
            ended.push(format!("{sub}:{}", args.join(" ")));
        }
    }
    ended
}

const ENDED: [&str; 11] = [
    ":status",
    ":status -uno",
    ":status -uall",
    "sub:status",
    ":diff --cached",
    "sub:diff --cached",
    ":add new.txt",
    ":add .",
    ":commit -m third",
    ":commit --amend -m third again",
    ":switch other",
];

#[test]
fn a_directory_named_gitignore_ends_no_command() {
    for at in [".gitignore", "sub/.gitignore"] {
        let all = through(|root, _| std::fs::create_dir(root.join(at)).unwrap());
        assert!(all.iter().all(|(ok, _, _)| *ok), "{at}: {all:?}");
    }
}

#[test]
fn a_directory_in_place_of_the_ignore_file_of_the_repository_ends_what_git_ends() {
    let all = through(|root, control| {
        let exclude = root.join(control).join("info/exclude");
        let _ = std::fs::remove_file(&exclude);
        std::fs::create_dir_all(exclude).unwrap();
    });
    let fatal = "fatal: cannot use <control>/info/exclude as an exclude file";
    assert_eq!(ended(&all, fatal), ENDED);
}

#[test]
fn a_directory_in_place_of_the_ignore_file_of_the_user_ends_what_git_ends() {
    // the file `core.excludesFile` names, by its full path and from the tree
    let all = through(|root, control| {
        std::fs::create_dir(root.join("../outside")).unwrap_or_default();
        let outside = root.canonicalize().unwrap().join("../outside");
        configure(root, control, outside.to_str().unwrap());
    });
    let fatal = "fatal: cannot use <root>/../outside as an exclude file";
    assert_eq!(ended(&all, fatal), ENDED);
    let all = through(|root, control| {
        std::fs::create_dir(root.join("sub/rules")).unwrap();
        configure(root, control, "sub/rules");
    });
    assert_eq!(
        ended(&all, "fatal: cannot use sub/rules as an exclude file"),
        ENDED
    );
    // the file it is without that setting, named before the repository's own
    let all = through(|root, control| {
        std::fs::create_dir_all(root.join("../config/git/ignore")).unwrap();
        let exclude = root.join(control).join("info/exclude");
        let _ = std::fs::remove_file(&exclude);
        std::fs::create_dir_all(exclude).unwrap();
    });
    let fatal = all[0].2.clone();
    assert!(
        fatal.ends_with("/config/git/ignore as an exclude file"),
        "{fatal}"
    );
    assert_eq!(ended(&all, &fatal), ENDED);
}

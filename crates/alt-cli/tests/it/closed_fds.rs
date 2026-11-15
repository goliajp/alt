//! A command started with a standard descriptor closed must not let one of
//! the repository's own files take that number: whatever the command then
//! prints would land in the file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

/// Runs `alt <args>` through a shell that first applies `redirect`
/// (`2>&-` closes standard error, and so on).
fn alt_with(repo: &Path, redirect: &str, args: &str) -> Output {
    shell(repo, redirect, args).output().unwrap()
}

fn shell(repo: &Path, redirect: &str, args: &str) -> Command {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(format!("exec \"$ALT\" {args} {redirect}"))
        .env("ALT", env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e");
    command
}

/// Every file that is the truth of the repository, with its bytes; the
/// caches a reader may rewrite (index runs, the ref snapshot) are left out.
fn truth(repo: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if !name.ends_with(".idx") && name != "refs" {
                    walk(&path, out);
                }
            } else if !name.ends_with(".lock") && !name.ends_with("durable") {
                out.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&repo.join(".alt"), &mut out);
    out
}

#[test]
fn closed_standard_descriptors_never_reach_repository_files() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path();
    assert!(alt(repo, &["init"]).status.success());
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    assert!(alt(repo, &["add", "a.txt"]).status.success());
    assert!(alt(repo, &["commit", "-m", "first"]).status.success());
    let before = truth(repo);
    assert!(before.keys().any(|path| path.ends_with("oplog/log")));

    // commands that print to the descriptor that is closed: output on
    // standard output, failures and warnings on standard error
    let cases = [
        ("2>&-", "log"),
        (
            "2>&-",
            "cat-file -p 0000000000000000000000000000000000000000",
        ),
        ("2>&-", "switch no-such-branch"),
        ("2>&-", "show no-such-rev"),
        (">&-", "log"),
        (">&-", "status"),
        (">&-", "branch"),
        (">&- 2>&-", "log"),
        (">&- 2>&-", "switch no-such-branch"),
        ("<&- >&- 2>&-", "status"),
        ("<&-", "log"),
    ];
    for (redirect, args) in cases {
        alt_with(repo, redirect, args);
        let after = truth(repo);
        for (path, bytes) in &before {
            assert!(
                after.get(path) == Some(bytes),
                "`alt {args} {redirect}` changed {}",
                path.display()
            );
        }
    }
    let fsck = alt(repo, &["fsck"]);
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

/// A program alt starts with its own standard descriptors (here the rebase
/// todo editor) reports what each of them is.
#[test]
fn a_closed_standard_descriptor_is_dev_null_for_the_command_and_its_children() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    assert!(alt(&repo, &["init"]).status.success());
    for n in 0..3 {
        fs::write(repo.join("a.txt"), format!("{n}\n")).unwrap();
        assert!(alt(&repo, &["add", "a.txt"]).status.success());
        assert!(
            alt(&repo, &["commit", "-m", &format!("c{n}")])
                .status
                .success()
        );
    }
    let probe = tmp.path().join("probe");
    let editor = tmp.path().join("editor.sh");
    fs::write(
        &editor,
        "#!/bin/sh\nexec 3>\"$PROBE\"\nfor fd in 0 1 2; do\n  \
         if [ /dev/fd/$fd -ef /dev/null ]; then echo \"$fd null\" >&3; \
         else echo \"$fd other\" >&3; fi\ndone\necho to-stdout\necho to-stderr >&2\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();

    let before = truth(&repo);
    for (redirect, closed) in [
        ("2>&-", vec![2]),
        (">&-", vec![1]),
        ("<&-", vec![0]),
        ("<&- >&- 2>&-", vec![0, 1, 2]),
    ] {
        let _ = fs::remove_file(&probe);
        shell(&repo, redirect, "rebase -i HEAD~2")
            .env("GIT_SEQUENCE_EDITOR", &editor)
            .env("PROBE", &probe)
            .output()
            .unwrap();
        let seen = fs::read_to_string(&probe).expect("the editor ran");
        for fd in closed {
            assert!(
                seen.contains(&format!("{fd} null")),
                "with `{redirect}` descriptor {fd} is not /dev/null in the child: {seen}"
            );
        }
        // a rebase may append to the logs; nothing already there may change
        for (path, bytes) in &before {
            let now = fs::read(path).unwrap_or_default();
            let name = path.file_name().unwrap().to_string_lossy();
            let append_only = ["log", "map.alt", "blobmap", "tier1"].contains(&&*name)
                || name.ends_with(".altpack");
            assert!(
                !append_only || now.starts_with(bytes),
                "`rebase -i {redirect}` rewrote {}",
                path.display()
            );
        }
    }
    let fsck = alt(&repo, &["fsck"]);
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

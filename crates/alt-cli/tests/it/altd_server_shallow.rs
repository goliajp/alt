//! Shallow fetches and fetches from a clone that has history already,
//! by `git` itself: `altd-server` gives each what `git upload-pack` gives
//! it from a mirror of the same history.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_untrusted::repository;

fn git_at(dir: &Path, time: u64, args: &[&str]) -> Output {
    let date = format!("{time} +0000");
    Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .args(["-c", "user.name=tester", "-c", "user.email=t@e"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> Output {
    git_at(dir, 1_700_000_000, args)
}

fn done(out: Output) -> String {
    let told = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{told}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// What a clone holds: its commits, the commits it holds without their
/// parents, and that it is whole as far as it goes.
fn held(clone: &Path) -> (Vec<String>, Vec<String>) {
    done(git(clone, &["fsck", "--no-progress", "--no-dangling"]));
    let lines = |text: String| {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines.sort();
        lines
    };
    let commits = lines(done(git(clone, &["rev-list", "--all"])));
    let shallow = fs::read_to_string(clone.join(".git/shallow")).unwrap_or_default();
    (commits, lines(shallow))
}

/// A history of twelve commits a thousand seconds apart from
/// 1 700 001 000 on: four, the tag `old`, a branch `side` of three, two
/// more, the merge of `side`, two more, the annotated tag `v1`.
fn history(src: &Path) {
    fs::create_dir(src).unwrap();
    done(git(src, &["init", "-q", "-b", "work", "."]));
    let time = std::cell::Cell::new(1_700_000_000);
    let commit = |name: &str| {
        time.set(time.get() + 1000);
        fs::write(src.join(name), name).unwrap();
        done(git(src, &["add", "."]));
        done(git_at(src, time.get(), &["commit", "-q", "-m", name]));
    };
    (1..=4).for_each(|n| commit(&format!("f{n}")));
    done(git(src, &["tag", "old"]));
    done(git(src, &["checkout", "-q", "-b", "side"]));
    (1..=3).for_each(|n| commit(&format!("s{n}")));
    done(git(src, &["checkout", "-q", "work"]));
    (5..=6).for_each(|n| commit(&format!("f{n}")));
    let merge = ["merge", "-q", "--no-ff", "-m", "merge", "side"];
    done(git_at(src, 1_700_009_500, &merge));
    done(git(src, &["branch", "-q", "-D", "side"]));
    time.set(time.get() + 1000);
    (7..=8).for_each(|n| commit(&format!("f{n}")));
    done(git(src, &["tag", "-a", "-m", "annotated", "v1"]));
}

struct Sides {
    root: PathBuf,
    /// `altd-server` and `git upload-pack`, in that order
    urls: [String; 2],
}

impl Sides {
    /// A clone of `work` from each side with `clone`, then a fetch with
    /// each of `fetches`. What the two clones hold after each step is the
    /// same, and is what is returned for the last.
    fn alike(&self, name: &str, clone: &[&str], fetches: &[&[&str]]) -> (Vec<String>, Vec<String>) {
        let into = |side: usize| self.root.join(format!("{name}-{side}"));
        let both = |step: &str| {
            let (ours, theirs) = (held(&into(0)), held(&into(1)));
            assert_eq!(ours, theirs, "{name}: {step}");
            ours
        };
        for (side, url) in self.urls.iter().enumerate() {
            let mut args = vec!["clone", "-q", "--single-branch", "-b", "work"];
            args.extend(clone);
            let to = into(side);
            args.extend([url.as_str(), to.to_str().unwrap()]);
            done(git(&self.root, &args));
        }
        let mut last = both("clone");
        for fetch in fetches {
            for side in 0..2 {
                let mut args = vec!["fetch", "-q"];
                args.extend(*fetch);
                args.push("origin");
                done(git(&into(side), &args));
            }
            last = both(&fetch.join(" "));
        }
        last
    }
}

#[test]
fn a_shallow_fetch_and_a_fetch_onto_history_are_given_what_git_gives_them() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);
    let src = tmp.path().join("src");
    history(&src);
    done(git(&src, &["push", "-q", &url, "work", "old", "v1"]));
    done(git(
        tmp.path(),
        &["clone", "-q", "--bare", "src", "mirror.git"],
    ));
    let mirror = format!("file://{}", tmp.path().join("mirror.git").display());
    let sides = Sides {
        root: tmp.path().to_owned(),
        urls: [url.clone(), mirror],
    };

    let (commits, shallow) = sides.alike("one", &["--depth", "1"], &[]);
    assert_eq!((commits.len(), shallow.len()), (1, 1));
    assert_eq!(commits, shallow);
    // three down from the tip is under the merge on one side only; two
    // further down is both sides of it
    let (commits, shallow) = sides.alike("deeper", &["--depth", "3"], &[&["--deepen", "2"]]);
    assert_eq!((commits.len(), shallow.len()), (7, 2));
    let (commits, shallow) = sides.alike("whole", &["--depth", "2"], &[&["--unshallow"]]);
    assert_eq!((commits.len(), shallow.len()), (12, 0));
    let (commits, shallow) = sides.alike("far", &["--depth", "100"], &[]);
    assert_eq!((commits.len(), shallow.len()), (12, 0));
    // a history made shorter: nothing new is sent, and the pack is a pack
    sides.alike("shorter", &["--depth", "5"], &[&["--depth", "2"]]);
    let (commits, _) = sides.alike(
        "since",
        &["--shallow-since=1700006500"],
        &[&["--deepen", "1"]],
    );
    assert_eq!(commits.len(), 8);
    let (commits, shallow) = sides.alike("not", &["--shallow-exclude=old"], &[]);
    assert_eq!((commits.len(), shallow.len()), (8, 2));

    // onto what a clone has: the server says which of its commits it has
    let before = sides.alike("onto", &[], &[]);
    let lone = sides.alike("lone", &["--depth", "2"], &[]);
    fs::write(src.join("later"), "later").unwrap();
    done(git(&src, &["add", "."]));
    done(git_at(
        &src,
        1_700_020_000,
        &["commit", "-q", "-m", "later"],
    ));
    done(git(&src, &["push", "-q", &url, "work"]));
    let update = ["fetch", "-q", "../src", "work:work"];
    done(git(&tmp.path().join("mirror.git"), &update));
    for (name, was) in [("onto", before), ("lone", lone)] {
        for side in 0..2 {
            let clone = tmp.path().join(format!("{name}-{side}"));
            done(git(&clone, &["fetch", "-q", "origin"]));
        }
        let ours = held(&tmp.path().join(format!("{name}-0")));
        assert_eq!(ours, held(&tmp.path().join(format!("{name}-1"))), "{name}");
        assert_eq!(ours.0.len(), was.0.len() + 1, "{name}");
        assert_eq!(ours.1, was.1, "{name}");
    }
    // a repository that has nothing of this history says so of every
    // commit it has, and is given all of it
    let other = tmp.path().join("other");
    fs::create_dir(&other).unwrap();
    done(git(&other, &["init", "-q", "."]));
    fs::write(other.join("own"), "own").unwrap();
    done(git(&other, &["add", "."]));
    done(git(&other, &["commit", "-q", "-m", "own"]));
    done(git(&other, &["fetch", "-q", &url, "work:theirs"]));
    assert_eq!(done(git(&other, &["rev-list", "--count", "theirs"])), "13");

    for (how, why) in [
        (
            "--shallow-since=2100000000",
            "no commits selected for shallow requests",
        ),
        ("--shallow-exclude=nowhere", "ambiguous deepen-not"),
    ] {
        let refused = git(
            tmp.path(),
            &["clone", "-q", "-b", "work", how, &url, "none"],
        );
        let told = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(!refused.status.success() && told.contains(why), "{told}");
    }
}

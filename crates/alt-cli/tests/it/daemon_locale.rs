//! A locale that only the caller's `LOCPATH` leads to, named to a
//! command the repository's daemon serves: the texts are matched as the
//! caller's C library would read them, whichever path the daemon has.

use std::path::Path;
use std::process::{Command, Output};

const LOCALE: &str = "xx_YY.UTF-8";

/// `alt diff` in `repo` in the locale [`LOCALE`], looked for in
/// `locales` when one is given; through the daemon or not.
fn diff(repo: &Path, locales: Option<&Path>, daemon: bool) -> Output {
    let mut alt = crate::alt_command();
    alt.current_dir(repo)
        .env_remove("LC_CTYPE")
        .env_remove("LANG")
        .env_remove("LOCPATH")
        .env("LC_ALL", LOCALE)
        .env("ALT_DAEMON_IDLE_MS", "1500")
        .arg("diff");
    match daemon {
        true => alt.env_remove("ALT_NO_DAEMON"),
        false => alt.env("ALT_NO_DAEMON", "1"),
    };
    if let Some(locales) = locales {
        alt.env("LOCPATH", locales);
    }
    alt.output().unwrap()
}

fn hunk_headers(patch: Output) -> Vec<String> {
    assert!(patch.status.success(), "{patch:?}");
    let lines = patch.stdout.split(|&b| b == b'\n');
    let heads = lines.filter(|l| l.starts_with(b"@@"));
    heads.map(|l| l.escape_ascii().to_string()).collect()
}

fn functions(mark: &str) -> Vec<u8> {
    // the second head is one only where lines are bytes
    let heads: [&[u8]; 2] = [b"def first(self):", b"def caf\xe9(self):"];
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
fn a_locale_on_the_callers_path_is_found_whichever_path_the_daemon_has() {
    let dirs = [0; 2].map(|_| alt_os::temp::tempdir().unwrap());
    let (repo, locales) = (dirs[0].path(), dirs[1].path());
    let made = Command::new("localedef")
        .args(["-i", "en_US", "-f", "UTF-8"])
        .arg(locales.join(LOCALE))
        .output()
        .expect("localedef is installed");
    let written = locales.join(LOCALE).join("LC_CTYPE").exists();
    assert!(written, "{}", String::from_utf8_lossy(&made.stderr));

    let alt = |args: &[&str]| {
        let mut alt = crate::alt_command();
        alt.current_dir(repo).env("ALT_NO_DAEMON", "1").args(args);
        let done = alt.output().unwrap();
        assert!(done.status.success(), "{done:?}");
    };
    alt(&["init", "."]);
    std::fs::write(repo.join(".gitattributes"), "*.py diff=python\n").unwrap();
    std::fs::write(repo.join("f.py"), functions("")).unwrap();
    alt(&["add", "."]);
    alt(&["commit", "-m", "base"]);
    std::fs::write(repo.join("f.py"), functions(" changed")).unwrap();

    // without the path the locale is not found, and lines are bytes
    let found = hunk_headers(diff(repo, Some(locales), false));
    let missed = hunk_headers(diff(repo, None, false));
    assert!(found[1].ends_with("def first(self):"), "{found:?}");
    assert!(!missed[1].ends_with("def first(self):"), "{missed:?}");

    let socket = repo.join(".alt/daemon.sock");
    let listens = || std::os::unix::net::UnixStream::connect(&socket).is_ok();
    // a daemon with each of the two paths, then a caller with each
    for theirs in [None, Some(locales)] {
        let begun = std::time::Instant::now();
        while socket.exists() {
            assert!(begun.elapsed().as_secs() < 20, "the daemon stays");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let first = hunk_headers(diff(repo, theirs, true));
        assert!(listens(), "no daemon served the command");
        let with = hunk_headers(diff(repo, Some(locales), true));
        let without = hunk_headers(diff(repo, None, true));
        assert!(listens(), "the daemon is gone");
        let expected = if theirs.is_some() { &found } else { &missed };
        assert_eq!(&first, expected);
        assert_eq!((&with, &without), (&found, &missed), "{theirs:?}");
    }
}

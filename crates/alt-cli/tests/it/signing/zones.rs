//! A date given to `GIT_AUTHOR_DATE` or `GIT_COMMITTER_DATE` in the forms
//! `date` and git print, beside what git writes for it; one without a
//! zone is in the zone of whoever runs the command, which is not the
//! daemon's.

use super::*;

/// The caller's `TZ` and a date.
const DATED: &[(&str, &str)] = &[
    ("Asia/Tokyo", "2005-04-07 22:13:13"),
    ("America/New_York", "2005-01-07 22:13:13"),
    ("America/New_York", "2005-07-07 22:13:13"),
    ("UTC", "2005-04-07 22:13:13"),
    ("JST-9", "Thu Apr 7 22:13:13 2005"),
    ("EST5EDT,M3.2.0,M11.1.0", "2005-07-07 22:13:13"),
    ("EST5EDT,M3.2.0,M11.1.0", "1105135993"),
    (":Asia/Tokyo", "2005-04-07T22:13:13"),
    ("Asia/Tokyo", "April 7, 2005 10:13:13 PM"),
    // `date`, `date -u`, `date -R` and `date -Iseconds`
    ("America/New_York", "Mon Nov 23 03:47:02 JST 2026"),
    ("Asia/Tokyo", "Sun Nov 22 18:47:02 UTC 2026"),
    ("UTC", "Mon, 23 Nov 2026 03:47:02 +0900"),
    ("UTC", "2026-11-23T03:47:02+09:00"),
    // a zone git has no name for is passed over
    ("America/New_York", "Thu Apr  7 22:13:13 KST 2005"),
    // `git log --date=` default, rfc, iso, iso-strict, raw and unix
    ("Asia/Tokyo", "Thu Apr 7 22:13:13 2005 +0200"),
    ("Asia/Tokyo", "Thu, 7 Apr 2005 22:13:13 +0200"),
    ("Asia/Tokyo", "2005-04-07 22:13:13 +0200"),
    ("Asia/Tokyo", "2005-04-07T22:13:13+02:00"),
    ("Asia/Tokyo", "1112904793 +0200"),
    ("Asia/Tokyo", "1112904793"),
    ("Asia/Tokyo", "Thu, 07 Apr 2005 22:13:13 GMT"),
    // the end of a day, and a zone that is none beside one that is
    ("Asia/Tokyo", "2005-04-07 24:00:00"),
    ("Asia/Tokyo", "2005-04-07 22:13:13 +0200 (UTC+2)"),
    ("Asia/Tokyo", "2005-04-07 22:13:13 JST-9"),
    // what is no file of a zone is UTC, and is not read to an end it
    // does not have
    ("/dev/zero", "2005-04-07 22:13:13"),
    (":/dev/zero", "2005-04-07 22:13:13"),
    ("/dev/null", "2005-04-07 22:13:13"),
    ("/usr/share/zoneinfo", "2005-04-07 22:13:13"),
];

/// A zone no date above is in, for the daemon.
const ELSEWHERE: &str = "Pacific/Kiritimati";

fn daemon_of(root: &Path) -> String {
    let meta = std::fs::read_to_string(root.join(".alt/daemon.meta")).unwrap();
    meta.lines().next().unwrap().to_owned()
}

#[test]
fn a_date_is_read_as_git_reads_it_in_the_zone_of_the_caller() {
    let dir = alt_os::temp::tempdir().unwrap();
    let theirs = dir.path().join("git");
    std::fs::create_dir(&theirs).unwrap();
    ok(git(&theirs, &[], &["init", "-q", "-b", "main", "."]));
    let (direct, served) = (dir.path().join("direct"), dir.path().join("served"));
    for root in [&direct, &served] {
        std::fs::create_dir(root).unwrap();
        init(root);
    }
    // the daemon is started in another zone than any caller's
    let idle = [("TZ", ELSEWHERE), ("ALT_DAEMON_IDLE_MS", "20000")];
    ok(alt(&served, true, &idle, &["status"]));
    let daemon = daemon_of(&served);
    let _stop = crate::scan_daemon::Stop(daemon.parse().unwrap(), false);

    for (tz, date) in DATED {
        let mut env = LATER[..4].to_vec();
        env.extend([
            ("GIT_AUTHOR_DATE", *date),
            ("GIT_COMMITTER_DATE", *date),
            ("TZ", *tz),
        ]);
        let made = ["commit", "-q", "--allow-empty", "-m", "made"];
        ok(git(&theirs, &env, &made));
        let expected = git_lines(&theirs, "commit", "HEAD");
        assert_eq!(expected.len(), 2, "{date} in {tz}");
        ok(commit(&direct, false, &env));
        assert_eq!(alt_lines(&direct, "HEAD"), expected, "{date} in {tz}");
        ok(commit(&served, true, &env));
        assert_eq!(alt_lines(&served, "HEAD"), expected, "{date} in {tz}");
    }
    assert_eq!(daemon_of(&served), daemon, "another daemon served");
}

#[test]
fn a_date_that_is_none_is_refused_as_git_refuses_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let theirs = dir.path().join("git");
    std::fs::create_dir(&theirs).unwrap();
    ok(git(&theirs, &[], &["init", "-q", "-b", "main", "."]));
    let ours = dir.path().join("alt");
    std::fs::create_dir(&ours).unwrap();
    init(&ours);
    for date in ["2005-04-07", "yesterday", "2 weeks ago", "12345 +0100", " "] {
        let mut env = LATER[..4].to_vec();
        env.push(("GIT_AUTHOR_DATE", date));
        let made = ["commit", "-q", "--allow-empty", "-m", "made"];
        let refusal = format!("fatal: invalid date format: {date}");
        let by_git = git(&theirs, &env, &made);
        let said = String::from_utf8_lossy(&by_git.stderr);
        assert!(said.contains(&refusal), "{said}");
        for daemon in [false, true] {
            let before = state(&ours);
            let by_alt = commit(&ours, daemon, &env);
            let said = String::from_utf8_lossy(&by_alt.stderr);
            assert_eq!(by_alt.status.code(), Some(128), "{said}");
            assert!(said.contains(&refusal), "{said}");
            assert_eq!(state(&ours), before, "{date}");
        }
    }
}

/// The zone, the last field, of each line of who made something.
fn zones_of(lines: &[String]) -> Vec<&str> {
    let zones = lines.iter().map(|line| line.rsplit(' ').next());
    zones.map(Option::unwrap).collect()
}

fn git_in(root: &Path, tz: Option<&str>, args: &[&str]) -> Output {
    let mut git = Command::new("git");
    git.current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", root)
        .envs(LATER[..4].iter().copied())
        .env_remove("GIT_AUTHOR_DATE")
        .env_remove("GIT_COMMITTER_DATE");
    match tz {
        Some(tz) => git.env("TZ", tz),
        None => git.env_remove("TZ"),
    };
    git.args(args).output().unwrap()
}

/// What is given no date is made now, in the zone the caller is in now:
/// the one its `TZ` names, the system's where it has none, and never the
/// daemon's.
#[test]
fn a_commit_made_now_is_in_the_zone_of_the_caller_as_git_writes_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let theirs = dir.path().join("git");
    std::fs::create_dir(&theirs).unwrap();
    ok(git(&theirs, &[], &["init", "-q", "-b", "main", "."]));
    let (direct, served) = (dir.path().join("direct"), dir.path().join("served"));
    for root in [&direct, &served] {
        std::fs::create_dir(root).unwrap();
        init(root);
    }
    let idle = [("TZ", ELSEWHERE), ("ALT_DAEMON_IDLE_MS", "20000")];
    ok(alt(&served, true, &idle, &["status"]));
    let daemon = daemon_of(&served);
    let _stop = crate::scan_daemon::Stop(daemon.parse().unwrap(), false);

    for (number, tz) in [
        Some("Asia/Tokyo"),
        Some("America/New_York"),
        Some("UTC"),
        Some("Asia/Kathmandu"),
        Some("/dev/zero"),
        Some(":/dev/zero"),
        Some("/dev/null"),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let made = ["commit", "-q", "--allow-empty", "-m", "made"];
        ok(git_in(&theirs, tz, &made));
        let lines = git_lines(&theirs, "commit", "HEAD");
        let expected = zones_of(&lines);
        assert_eq!(expected.len(), 2, "{tz:?}");
        let mut env = LATER[..4].to_vec();
        env.extend(tz.map(|tz| ("TZ", tz)));
        for (root, daemon) in [(&direct, false), (&served, true)] {
            let told = format!("{tz:?}, daemon {daemon}");
            ok(commit(root, daemon, &env));
            assert_eq!(zones_of(&alt_lines(root, "HEAD")), expected, "{told}");
            // the notes commit and a tag are the committer's
            let notes = alt_lines(root, "refs/notes/alt/meta");
            assert_eq!(zones_of(&notes), expected, "{told}");
            let name = format!("at-{number}-{daemon}");
            ok(alt(
                root,
                daemon,
                &env,
                &["tag", "-a", &name, "-m", "marked"],
            ));
            let tagger = alt_lines(root, &name);
            assert_eq!(zones_of(&tagger), expected[1..], "{told}");
            ok(alt(
                root,
                daemon,
                &env,
                &["commit", "--amend", "-m", "again"],
            ));
            let amended = alt_lines(root, "HEAD");
            assert_eq!(zones_of(&amended)[1], expected[1], "{told}");
        }
    }
    assert_eq!(daemon_of(&served), daemon, "another daemon served");
}

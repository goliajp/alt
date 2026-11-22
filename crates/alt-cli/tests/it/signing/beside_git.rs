//! The author and the committer of what alt writes beside those of
//! what git writes, given the same environment and configuration.

use super::*;

/// What names the two, in the environment, in the repository and outside it.
struct Case {
    env: &'static [(&'static str, &'static str)],
    own: &'static [(&'static str, &'static str)],
    outside: &'static str,
}

const USER: &[(&str, &str)] = &[("user.name", "U"), ("user.email", "u@x")];

const CASES: &[Case] = &[
    Case {
        env: &[],
        own: USER,
        outside: "",
    },
    Case {
        env: &[],
        own: &[],
        outside: "[user]\n\tname = G\n\temail = g@x\n",
    },
    // the repository's stands before what is outside it
    Case {
        env: &[],
        own: &[("user.email", "own@x")],
        outside: "[user]\n\tname = G\n\temail = g@x\n",
    },
    // the environment stands before both, each variable for itself
    Case {
        env: &[("GIT_AUTHOR_NAME", "EA"), ("GIT_COMMITTER_EMAIL", "ec@x")],
        own: USER,
        outside: "",
    },
    Case {
        env: &[
            ("GIT_AUTHOR_NAME", "EA"),
            ("GIT_AUTHOR_EMAIL", "ea@x"),
            ("GIT_COMMITTER_NAME", "EC"),
            ("GIT_COMMITTER_EMAIL", "ec@x"),
        ],
        own: &[],
        outside: "",
    },
    // the author's and the committer's own stand before the user's
    Case {
        env: &[],
        own: &[
            ("user.name", "U"),
            ("user.email", "u@x"),
            ("author.name", "AN"),
            ("author.email", "an@x"),
            ("committer.name", "CN"),
            ("committer.email", "cn@x"),
        ],
        outside: "",
    },
    Case {
        env: &[],
        own: &[("author.email", "an@x"), ("committer.name", "CN")],
        outside: "[user]\n\tname = G\n\temail = g@x\n",
    },
    Case {
        env: &[("GIT_AUTHOR_EMAIL", "ea@x")],
        own: &[
            ("author.email", "an@x"),
            ("user.name", "U"),
            ("user.email", "u@x"),
        ],
        outside: "",
    },
    // EMAIL is the last an address is taken from
    Case {
        env: &[("EMAIL", "e@x")],
        own: &[("user.name", "U")],
        outside: "",
    },
    Case {
        env: &[("EMAIL", "e@x")],
        own: USER,
        outside: "",
    },
    Case {
        env: &[("EMAIL", "e@x"), ("GIT_AUTHOR_EMAIL", "ea@x")],
        own: &[("user.name", "U")],
        outside: "",
    },
    // what neither a name nor an address may carry is taken out
    Case {
        env: &[
            ("GIT_AUTHOR_NAME", " <A <b> c>\nd>. "),
            ("GIT_AUTHOR_EMAIL", " <a@\nx>> "),
            ("GIT_COMMITTER_NAME", "\"C:D,"),
            ("GIT_COMMITTER_EMAIL", ".c@x;"),
        ],
        own: &[],
        outside: "",
    },
    Case {
        env: &[],
        own: &[],
        outside: "[user]\n\tname = \" U <V> \"\n\temail = \"  <u@x>  \"\n",
    },
];

#[test]
fn both_are_named_as_git_names_them() {
    for (number, case) in CASES.iter().enumerate() {
        let dir = alt_os::temp::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, case.outside).unwrap();
        let mut env = vec![("GIT_CONFIG_GLOBAL", outside.to_str().unwrap())];
        env.extend(DATES);
        env.extend(case.env);

        let theirs = dir.path().join("git");
        std::fs::create_dir(&theirs).unwrap();
        ok(git(&theirs, &[], &["init", "-q", "-b", "main", "."]));
        for (key, value) in case.own {
            ok(git(&theirs, &[], &["config", key, value]));
        }
        ok(git(
            &theirs,
            &env,
            &["commit", "-q", "--allow-empty", "-m", "made"],
        ));
        let expected = git_lines(&theirs, "commit", "HEAD");
        assert_eq!(expected.len(), 2, "case {number}");

        for daemon in [false, true] {
            let ours = dir.path().join(format!("alt-{daemon}"));
            std::fs::create_dir(&ours).unwrap();
            init(&ours);
            for (key, value) in case.own {
                set(&ours, daemon, key, value);
            }
            ok(commit(&ours, daemon, &env));
            let told = format!("case {number}, daemon {daemon}");
            assert_eq!(alt_lines(&ours, "HEAD"), expected, "{told}");
        }
    }
}

/// The commands that keep an author or write a new one, each with what
/// git is asked for the same.
const STEPS: &[(&[&str], &[&str], &str, &str)] = &[
    (
        &["cherry-pick", "topic"],
        &["cherry-pick", "topic"],
        "commit",
        "HEAD",
    ),
    (
        &["commit", "--amend", "-m", "again"],
        &["commit", "-q", "--amend", "-m", "again"],
        "commit",
        "HEAD",
    ),
    (
        &["revert", "HEAD"],
        &["revert", "--no-edit", "HEAD"],
        "commit",
        "HEAD",
    ),
    (
        &["tag", "-a", "marked", "-m", "marked"],
        &["tag", "-a", "marked", "-m", "marked"],
        "tag",
        "marked",
    ),
    (
        &["switch", "side"],
        &["switch", "-q", "side"],
        "commit",
        "HEAD",
    ),
    (
        &["rebase", "main"],
        &["rebase", "-q", "main"],
        "commit",
        "HEAD",
    ),
];

#[test]
fn an_author_is_kept_and_a_committer_written_as_git_does() {
    let dir = alt_os::temp::tempdir().unwrap();
    let theirs = dir.path().join("git");
    std::fs::create_dir(&theirs).unwrap();
    ok(git(&theirs, &[], &["init", "-q", "-b", "main", "."]));
    diverged(&theirs, &|env, args| {
        let quiet = matches!(args[0], "switch" | "commit");
        let args: Vec<&str> = args.iter().copied().chain(quiet.then_some("-q")).collect();
        ok(git(&theirs, env, &args));
    });
    for daemon in [false, true] {
        let ours = dir.path().join(format!("alt-{daemon}"));
        std::fs::create_dir(&ours).unwrap();
        init(&ours);
        diverged(&ours, &|env, args| drop(ok(alt(&ours, daemon, env, args))));
    }
    for (asked, of_git, kind, rev) in STEPS {
        ok(git(&theirs, &LATER, of_git));
        let expected = git_lines(&theirs, kind, rev);
        assert!(!expected.is_empty(), "{asked:?}");
        for daemon in [false, true] {
            let ours = dir.path().join(format!("alt-{daemon}"));
            ok(alt(&ours, daemon, &LATER, asked));
            assert_eq!(
                alt_lines(&ours, rev),
                expected,
                "{asked:?}, daemon {daemon}"
            );
        }
    }
}

#[test]
fn a_merge_commit_is_by_both_as_git_writes_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let theirs = dir.path().join("git");
    std::fs::create_dir(&theirs).unwrap();
    ok(git(&theirs, &[], &["init", "-q", "-b", "main", "."]));
    diverged(&theirs, &|env, args| {
        let quiet = matches!(args[0], "switch" | "commit");
        let args: Vec<&str> = args.iter().copied().chain(quiet.then_some("-q")).collect();
        ok(git(&theirs, env, &args));
    });
    ok(git(&theirs, &LATER, &["merge", "-q", "--no-edit", "topic"]));
    let expected = git_lines(&theirs, "commit", "HEAD");
    assert_eq!(expected.len(), 2);
    for daemon in [false, true] {
        let ours = dir.path().join(format!("alt-{daemon}"));
        std::fs::create_dir(&ours).unwrap();
        init(&ours);
        diverged(&ours, &|env, args| drop(ok(alt(&ours, daemon, env, args))));
        ok(alt(&ours, daemon, &LATER, &["merge", "topic"]));
        assert_eq!(alt_lines(&ours, "HEAD"), expected, "daemon {daemon}");
    }
}

//! The path gate of a policy holds for what is staged and for what is
//! committed, and a pattern of it for the directory it names.

use std::path::Path;

use crate::capability_gate::{alt, alt_as, fails_with, fixture, ok, write_policy};

/// `src/**` allows what lies beneath `src` and nothing whose name only
/// begins with it.
#[test]
fn path_allow_holds_a_path_to_the_directory_it_names() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    for file in ["src/deep/lib.rs", "srcx/y", "srcfile2"] {
        std::fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
        std::fs::write(repo.join(file), "x\n").unwrap();
    }
    write_policy(repo, "agent:* -> path=src/**\n");
    for beside in ["srcx/y", "srcfile2", "srcx", "."] {
        fails_with(alt_as(repo, &["add", beside], "bot"), "capability denied");
    }
    ok(alt_as(repo, &["add", "src"], "bot"));
    ok(alt_as(repo, &["commit", "-m", "src"], "bot"));
}

/// What another principal staged outside the allowed paths is not
/// committed by the restricted one: a new file, a change, a removal. The
/// index stays as it was, and the operator commits it.
#[test]
fn path_allow_denies_a_commit_of_what_another_staged_out_of_tree() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    std::fs::write(repo.join("scripts/old.sh"), "old\n").unwrap();
    std::fs::write(repo.join("scripts/gone.sh"), "gone\n").unwrap();
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "scripts"]));
    write_policy(repo, "agent:* -> path=src/**\n");
    let head = ok(alt(repo, &["rev-parse", "main"]));

    type Staged = fn(&Path);
    let staged: [Staged; 3] = [
        |repo| std::fs::write(repo.join("scripts/run.sh"), "#!/bin/sh\n").unwrap(),
        |repo| std::fs::write(repo.join("scripts/old.sh"), "changed\n").unwrap(),
        |repo| std::fs::remove_file(repo.join("scripts/gone.sh")).unwrap(),
    ];
    for stage in staged {
        stage(repo);
        std::fs::write(repo.join("src/lib.rs"), "// src\n").unwrap();
        ok(alt(repo, &["add", "."]));
        for args in [
            &["commit", "-m", "by bot"][..],
            &["commit", "--amend", "-m", "by bot"],
        ] {
            fails_with(alt_as(repo, args, "bot"), "capability denied");
        }
        assert_eq!(ok(alt(repo, &["rev-parse", "main"])), head);
    }
    // the baseline another principal committed is no obstacle
    ok(alt(repo, &["commit", "-m", "by the operator"]));
    std::fs::write(repo.join("src/lib.rs"), "// more\n").unwrap();
    ok(alt_as(repo, &["add", "src/lib.rs"], "bot"));
    ok(alt_as(repo, &["commit", "-m", "src only"], "bot"));
}

/// A removal outside the allowed paths is not staged by the restricted
/// principal, however the `add` comes by it, and the index stays as it was.
#[test]
fn path_allow_denies_staging_a_removal_out_of_tree() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    let lone = repo.join("scripts/lone");
    std::fs::create_dir_all(&lone).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(lone.join("gone.sh"), "gone\n").unwrap();
    std::fs::write(repo.join("src/gone.rs"), "gone\n").unwrap();
    ok(alt(repo, &["add", "."]));
    ok(alt(repo, &["commit", "-m", "scripts"]));
    write_policy(repo, "agent:* -> path=src/**\n");
    std::fs::remove_file(lone.join("gone.sh")).unwrap();
    std::fs::remove_file(repo.join("src/gone.rs")).unwrap();
    let index = std::fs::read(repo.join(".alt/index")).unwrap();

    for args in [
        &["add", "scripts/lone/gone.sh"][..],
        &["add", "-u", "scripts/lone/gone.sh"],
        &["add", "-A", "scripts/lone"],
        &["add", "scripts"],
    ] {
        fails_with(alt_as(repo, args, "bot"), "capability denied");
    }
    // the removal is the one tracked path the directory has
    for args in [&["add", "."][..], &["add", "-u"], &["add", "-A"]] {
        fails_with(alt_as(&lone, args, "bot"), "capability denied");
    }
    assert_eq!(std::fs::read(repo.join(".alt/index")).unwrap(), index);
    ok(alt_as(repo, &["add", "src/gone.rs"], "bot"));
    ok(alt_as(repo, &["commit", "-m", "src only"], "bot"));
}

/// A rebase the restricted principal may not carry out is refused before
/// it begins: over a path the new base changes, and over one a replayed
/// commit changes that the last of them leaves as the base has it. The
/// branch, the index and the working tree stay as they were.
#[test]
fn path_allow_denies_a_rebase_and_leaves_no_part_of_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    let write = |file: &str, body: &str| {
        std::fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
        std::fs::write(repo.join(file), body).unwrap();
    };
    let commit = |message: &str| {
        ok(alt(repo, &["add", "."]));
        ok(alt(repo, &["commit", "-m", message]));
    };
    write("scripts/run.sh", "run\n");
    commit("scripts");
    ok(alt(repo, &["switch", "-c", "beside"]));
    write("src/o.rs", "// o\n");
    commit("beside: src only");
    ok(alt(repo, &["switch", "-c", "across"]));
    write("scripts/x.sh", "x\n");
    commit("across: scripts");
    ok(alt(repo, &["switch", "main"]));
    write("scripts/t.sh", "t\n");
    write("src/lib.rs", "// lib\n");
    commit("main: a script comes");
    std::fs::remove_file(repo.join("scripts/t.sh")).unwrap();
    commit("main: and goes");
    write_policy(repo, "agent:* -> path=src/**\n");

    let state = || {
        let said = ["status", "branch"].map(|c| ok(alt(repo, &[c])));
        let refs = ["main", "beside", "across"].map(|r| ok(alt(repo, &["rev-parse", r])));
        (said, refs, std::fs::read(repo.join(".alt/index")).unwrap())
    };
    let before = state();
    for upstream in ["across", "beside"] {
        fails_with(alt_as(repo, &["rebase", upstream], "bot"), "capability denied");
        assert_eq!(state(), before, "after rebase {upstream}");
        assert!(!repo.join("src/o.rs").exists());
    }
    ok(alt(repo, &["rebase", "beside"]));
}

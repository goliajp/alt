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

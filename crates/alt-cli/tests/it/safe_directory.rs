//! A repository that belongs to another user is not opened, as by git,
//! unless the user's configuration names it in `safe.directory`.

use std::path::Path;
use std::process::{Command, Output};

use crate::like_git::{Pair, fails, ok};

fn run(program: &str, repo: &Path, global: &Path, args: &[&str]) -> Output {
    let mut command = match program {
        "alt" => crate::alt_command(),
        other => Command::new(other),
    };
    command
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .env_remove("SUDO_UID")
        .args(args);
    command.output().unwrap()
}

#[test]
#[ignore = "needs root: gives a repository to another user"]
fn a_repository_of_another_user_is_opened_only_where_it_is_named_safe() {
    let pair = Pair::new();
    pair.write("f", "f\n");
    pair.commit_all("one");
    // a filter the repository names, which a command that reads would run
    let ran = pair.alt.join("ran");
    let filter = format!("touch '{}'; cat", ran.display());
    ok(crate::like_git::alt(
        &pair.alt,
        &["config", "filter.x.clean", &filter],
    ));
    std::fs::write(pair.alt.join(".gitattributes"), "* filter=x\n").unwrap();
    let global = pair.alt.parent().unwrap().join("global");
    std::fs::write(&global, "").unwrap();
    for (program, repo) in [("alt", &pair.alt), ("git", &pair.git)] {
        ok(run(program, repo, &global, &["status"]));
    }
    let _ = std::fs::remove_file(&ran);
    for repo in [&pair.alt, &pair.git] {
        std::os::unix::fs::chown(repo, Some(65534), None).unwrap();
    }
    for (program, repo) in [("alt", &pair.alt), ("git", &pair.git)] {
        let at = repo.canonicalize().unwrap();
        let wanted = format!(
            "fatal: detected dubious ownership in repository at '{0}'\n\
             To add an exception for this directory, call:\n\n\
             \tgit config --global --add safe.directory {0}\n",
            at.display()
        );
        for args in [&["status"][..], &["log", "-n", "1"], &["add", "f"]] {
            let said = fails(run(program, repo, &global, args), 128);
            assert!(said.starts_with(&wanted), "{program} {args:?}: {said}");
        }
    }
    assert!(!ran.exists(), "the filter of the repository was run");
    // and no store is made of a git repository that is another user's
    let to = pair.alt.parent().unwrap().join("imported");
    let import = ["import", to.to_str().unwrap()];
    let said = fails(run("alt", &pair.git, &global, &import), 128);
    assert!(said.contains("detected dubious ownership"), "{said}");
    assert!(!to.exists());
    for (program, repo) in [("alt", &pair.alt), ("git", &pair.git)] {
        let at = repo.canonicalize().unwrap();
        for named in [at.to_str().unwrap().to_owned(), "*".to_owned()] {
            std::fs::write(&global, format!("[safe]\n\tdirectory = {named}\n")).unwrap();
            ok(run(program, repo, &global, &["status"]));
        }
        // named, and taken back
        let both = "[safe]\n\tdirectory = *\n\tdirectory =\n";
        std::fs::write(&global, both).unwrap();
        fails(run(program, repo, &global, &["status"]), 128);
    }
}

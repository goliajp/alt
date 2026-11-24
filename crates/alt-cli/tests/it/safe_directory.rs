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
        .env("HOME", repo.parent().unwrap())
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
    // nor is one made for tasks
    let (from, tasks) = (pair.git.to_str().unwrap(), to.with_file_name("tasks"));
    let intake = [
        "intake",
        from,
        tasks.to_str().unwrap(),
        "--git",
        "/usr/bin/git",
    ];
    let said = fails(run("alt", &pair.alt, &global, &intake), 128);
    assert!(said.contains("detected dubious ownership"), "{said}");
    assert!(!tasks.exists());
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
        // named from the home of the user, as the directory the command
        // runs in, and by a path that leads through a symbolic link
        let above = at.parent().unwrap();
        let via = above.join("via");
        if via.symlink_metadata().is_err() {
            std::os::unix::fs::symlink(above, &via).unwrap();
        }
        let name = at.file_name().unwrap().to_str().unwrap();
        let through = via.join(name);
        for named in [
            format!("~/{name}"),
            ".".to_owned(),
            through.to_str().unwrap().to_owned(),
            format!("{}/*", via.display()),
        ] {
            std::fs::write(&global, format!("[safe]\n\tdirectory = {named}\n")).unwrap();
            let opened = run(program, repo, &global, &["status"]);
            assert!(opened.status.success(), "{program} {named}");
        }
        // a path that begins nowhere names nothing
        std::fs::write(&global, format!("[safe]\n\tdirectory = {name}\n")).unwrap();
        fails(run(program, repo, &global, &["status"]), 128);
    }
}

/// What `git ls-remote` of `url` ends with and says.
fn asked(url: &str) -> Output {
    Command::new("git")
        .args(["ls-remote", url])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap()
}

#[test]
#[ignore = "needs root: gives a repository to another user"]
fn a_repository_of_another_user_is_served_only_where_it_is_named_safe() {
    let pair = Pair::new();
    pair.write("f", "f\n");
    pair.commit_all("one");
    let above = pair.alt.parent().unwrap();
    let global = above.join("global");
    std::fs::write(&global, "").unwrap();
    let env = |root: &'static str, at: &Path| {
        [
            (root, at.to_str().unwrap().to_owned()),
            ("GIT_CONFIG_NOSYSTEM", "1".to_owned()),
            ("GIT_CONFIG_GLOBAL", global.to_str().unwrap().to_owned()),
        ]
    };
    let serve = |root: &'static str, at: &Path| {
        let env = env(root, at);
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        crate::altd_server::spawn_server_with_env(&env)
    };
    let many = serve("ALT_SERVER_ROOT", above);
    let url = format!("http://{}/{}/", many.addr, "alt");
    assert_eq!(pair.alt.file_name().unwrap(), "alt");
    ok(asked(&url));
    // git looks at the directory it serves, which is the one inside
    for repo in [&pair.alt, &pair.git, &pair.git.join(".git")] {
        std::os::unix::fs::chown(repo, Some(65534), None).unwrap();
    }
    // git serves it to no one: not to a fetch, not to a push
    let dubious = "fatal: detected dubious ownership in repository at";
    for service in ["upload-pack", "receive-pack"] {
        let git = Command::new("git")
            .args([service, "--advertise-refs", pair.git.to_str().unwrap()])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &global)
            .env_remove("SUDO_UID")
            .output()
            .unwrap();
        assert!(fails(git, 128).starts_with(dubious), "git {service}");
    }
    // nor does a server that was serving it, and it tells the one who
    // asks nothing of where the repository is
    let refused = asked(&url);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(!said.contains(above.to_str().unwrap()), "{said}");
    let mut many = many;
    let logged = many.drain_stderr();
    assert!(logged.contains(dubious), "{logged}");
    // one started on it alone does not start
    let alone = Command::new(env!("CARGO_BIN_EXE_altd-server"))
        .args(["--bind", "127.0.0.1:0"])
        .envs(env("ALT_SERVER_REPO", &pair.alt))
        .env_remove("SUDO_UID")
        .output()
        .unwrap();
    assert!(!alone.status.success());
    let said = String::from_utf8_lossy(&alone.stderr).into_owned();
    assert!(said.starts_with(dubious), "{said}");
    // and no daemon holds its store
    let daemon = Command::new(env!("CARGO_BIN_EXE_altd"))
        .arg(pair.alt.join(".alt"))
        .env("ALT_DAEMON_IDLE_MS", "100")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env_remove("SUDO_UID")
        .output()
        .unwrap();
    assert!(!daemon.status.success());
    let said = String::from_utf8_lossy(&daemon.stderr).into_owned();
    assert!(said.starts_with(dubious), "{said}");
    assert!(!pair.alt.join(".alt/daemon.sock").exists());
    // named safe, it is served again
    let at = pair.alt.canonicalize().unwrap();
    std::fs::write(&global, format!("[safe]\n\tdirectory = {}\n", at.display())).unwrap();
    let many = serve("ALT_SERVER_ROOT", above);
    ok(asked(&format!("http://{}/alt/", many.addr)));
    let alone = serve("ALT_SERVER_REPO", &pair.alt);
    ok(asked(&format!("http://{}/", alone.addr)));
}

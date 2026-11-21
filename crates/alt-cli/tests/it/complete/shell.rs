//! Completion and the prompt in an interactive zsh, typed into through a
//! pseudo-terminal.

use std::os::unix::fs::PermissionsExt;

use super::{alt, repository};
use crate::shell::{fixture, no_zsh, zsh};

#[test]
fn an_interactive_zsh_completes_alt_and_redraws_its_prompt() {
    if no_zsh("an_interactive_zsh_completes_alt_and_redraws_its_prompt") {
        return;
    }
    let dir = repository();
    let repo = dir.path();
    let others = alt_os::temp::tempdir().unwrap();
    let (slow, quick) = (others.path().join("s"), others.path().join("q"));
    for (place, branch) in [(&slow, "slow"), (&quick, "quick")] {
        std::fs::create_dir(place).unwrap();
        alt(place, &["init", "."]);
        std::fs::write(place.join("f"), "f\n").unwrap();
        alt(place, &["add", "f"]);
        alt(place, &["commit", "-m", "first"]);
        alt(place, &["switch", "-c", branch]);
    }
    // an `alt` that takes its time in the slow repository
    let real = env!("CARGO_BIN_EXE_alt");
    let wrapped = others.path().join("bin");
    std::fs::create_dir(&wrapped).unwrap();
    let wrapper = format!(
        "#!/bin/sh\ncase $PWD in {}*) sleep 1 ;; esac\nexec {real} \"$@\"\n",
        slow.display()
    );
    std::fs::write(wrapped.join("alt"), wrapper).unwrap();
    let executable = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(wrapped.join("alt"), executable).unwrap();

    let mut driver = zsh(repo);
    let path = std::env::var("PATH").unwrap();
    driver
        .env("PATH", format!("{}:{path}", wrapped.display()))
        .arg(fixture("interactive.zsh"))
        .arg(fixture("interactive-setup.zsh"))
        .args([repo, slow.as_path(), quick.as_path()]);
    let done = driver.output().unwrap();
    let said = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{said}\n{stderr}");
    assert_eq!(
        said,
        "subcommand=[alt switch ]\n\
         branch=[alt switch feature/login ]\n\
         option=[alt commit --amend ]\n\
         branches=[feature/login hotfix/urgent main release/1.0 topic]\n\
         paths=[a.txt new.txt fresh/]\n\
         refspecs=[main:dev main:main]\n\
         prompt=[<main *%>]\n\
         late=[<slow>]\n\
         kept=[quick]\n"
    );
}

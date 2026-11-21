//! The prompt functions of the zsh plugin, run in a zsh.

use std::path::Path;

use super::{alt, commit, committed, diverged, head, run, set_ref};
use crate::shell::{no_zsh, plugin_dir, zsh_says};

/// What `alt_prompt_info` shows in a prompt in `repo`, after the zsh
/// commands `styles`.
fn shown(repo: &Path, styles: &str) -> String {
    let script = format!(
        "eval \"$(alt shell zsh)\"; zstyle ':alt:prompt' color no; {styles}\n\
         cd $1; alt_prompt_update now; print -rP -- \"[$(alt_prompt_info)]\""
    );
    zsh_says(repo, &script, &[repo.to_str().unwrap()])
}

#[test]
fn the_prompt_shows_the_state_as_a_git_prompt_would() {
    if no_zsh("the_prompt_shows_the_state_as_a_git_prompt_would") {
        return;
    }
    let outside = alt_os::temp::tempdir().unwrap();
    assert_eq!(shown(outside.path(), ""), "[]\n");

    let dir = committed();
    let repo = dir.path();
    assert_eq!(shown(repo, ""), "[main]\n");
    std::fs::write(repo.join("staged.txt"), "s\n").unwrap();
    alt(repo, &["add", "staged.txt"]);
    std::fs::write(repo.join("f.txt"), "changed\n").unwrap();
    std::fs::write(repo.join("new"), "n\n").unwrap();
    assert_eq!(shown(repo, ""), "[main *+%]\n");
    assert_eq!(
        shown(repo, "zstyle ':alt:prompt' untracked no"),
        "[main *+]\n"
    );
    let styles = "zstyle ':alt:prompt:symbols' staged S; zstyle ':alt:prompt' format '%f on %b'";
    assert_eq!(shown(repo, styles), "[ *S% on main]\n");
    let coloured =
        "eval \"$(alt shell zsh)\"; alt_prompt_update now; print -r -- \"$(alt_prompt_info)\"";
    assert_eq!(
        zsh_says(repo, coloured, &[]),
        "%F{green}main%f%F{red} *+%%%f\n"
    );

    let dir = diverged();
    let repo = dir.path();
    assert_eq!(run(repo, &["merge", "feature/side"]).status.code(), Some(1));
    assert_eq!(shown(repo, ""), "[main|MERGING !1]\n");

    let dir = diverged();
    let repo = dir.path();
    alt(repo, &["switch", "feature/side"]);
    std::fs::write(repo.join("other.txt"), "o\n").unwrap();
    alt(repo, &["add", "other.txt"]);
    alt(repo, &["commit", "-m", "second on side"]);
    assert_eq!(run(repo, &["rebase", "main"]).status.code(), Some(1));
    assert_eq!(shown(repo, ""), "[feature/side|REBASE 1/2 !1]\n");
    alt(repo, &["rebase", "--abort"]);
    alt(repo, &["bisect", "start"]);
    let tip = head(repo);
    assert_eq!(shown(repo, ""), format!("[({})|BISECTING]\n", &tip[..8]));

    let broken = alt_os::temp::tempdir().unwrap();
    std::fs::create_dir(broken.path().join(".alt")).unwrap();
    assert_eq!(shown(broken.path(), ""), "[ ⚠]\n");
}

#[test]
fn the_prompt_shows_the_workspace_and_the_distance_to_the_remotes() {
    if no_zsh("the_prompt_shows_the_workspace_and_the_distance_to_the_remotes") {
        return;
    }
    let dir = committed();
    let repo = dir.path();
    alt(
        repo,
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    );
    alt(
        repo,
        &["remote", "add", "backup", "https://example.invalid/b.git"],
    );
    alt(repo, &["branch", "topic"]);
    set_ref(repo, "refs/remotes/origin/main", &head(repo));
    commit(repo, "one\n", "one");
    set_ref(repo, "refs/remotes/backup/main", &head(repo));
    commit(repo, "two\n", "two");
    // the first remote by name, unless another is named
    assert_eq!(shown(repo, ""), "[main ↑1]\n");
    assert_eq!(
        shown(repo, "zstyle ':alt:prompt' remote origin"),
        "[main ↑2]\n"
    );
    assert_eq!(
        shown(repo, "zstyle ':alt:prompt' remote all"),
        "[main ↑1 ↑2]\n"
    );
    assert_eq!(
        shown(repo, "zstyle ':alt:prompt' ahead-behind no"),
        "[main]\n"
    );

    let other = alt_os::temp::tempdir().unwrap();
    let place = other.path().join("w");
    alt(
        repo,
        &["workspace", "add", "side", place.to_str().unwrap(), "topic"],
    );
    set_ref(repo, "refs/remotes/origin/topic", &head(repo));
    assert_eq!(
        shown(&place, "zstyle ':alt:prompt' remote origin"),
        "[topic ↓2 [side]]\n"
    );
}

#[test]
fn vcs_info_knows_alt_however_the_plugin_is_loaded() {
    if no_zsh("vcs_info_knows_alt_however_the_plugin_is_loaded") {
        return;
    }
    let dir = diverged();
    let repo = dir.path();
    std::fs::write(repo.join("staged.txt"), "s\n").unwrap();
    alt(repo, &["add", "staged.txt"]);
    std::fs::write(repo.join("f.txt"), "changed\n").unwrap();
    let styles = "zstyle ':vcs_info:*' formats '%s|%b|%c%u|%m'\n\
                  zstyle ':vcs_info:*' actionformats '%s|%b|%a|%c%u'\n\
                  zstyle ':vcs_info:*' check-for-changes true\n\
                  vcs_info; print -r -- \"$vcs_info_msg_0_\"";
    let printed = format!("autoload -Uz vcs_info; eval \"$(alt shell zsh)\"; {styles}");
    assert_eq!(zsh_says(repo, &printed, &[]), "alt|main|SU|\n");
    let sourced = format!("autoload -Uz vcs_info; source $1/alt.plugin.zsh; {styles}");
    let plugin = plugin_dir();
    let plugin = plugin.to_str().unwrap();
    assert_eq!(zsh_says(repo, &sourced, &[plugin]), "alt|main|SU|\n");

    alt(repo, &["commit", "-m", "staged"]);
    std::fs::write(repo.join("f.txt"), "main\n").unwrap();
    assert_eq!(run(repo, &["merge", "feature/side"]).status.code(), Some(1));
    assert_eq!(zsh_says(repo, &printed, &[]), "alt|main|merge|U\n");
    let outside = alt_os::temp::tempdir().unwrap();
    assert_eq!(zsh_says(outside.path(), &printed, &[]), "\n");
}

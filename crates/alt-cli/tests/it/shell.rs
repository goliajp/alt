//! `alt shell zsh` and the zsh plugin it prints. The tests that run zsh
//! say so and pass on a machine that has none.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether there is no zsh to test with; says so when there is none.
pub fn no_zsh(test: &str) -> bool {
    let found = Command::new("zsh").arg("--version").output();
    let missing = !found.is_ok_and(|done| done.status.success());
    if missing {
        eprintln!("skipped {test}: zsh is not installed on this machine");
    }
    missing
}

/// The plugin as the repository holds it.
pub fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shell/zsh")
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/zsh")
        .join(name)
}

/// A zsh that reads no startup file and finds this build's `alt` first.
pub fn zsh(cwd: &Path) -> Command {
    let alt = Path::new(env!("CARGO_BIN_EXE_alt"));
    let path = std::env::var("PATH").unwrap();
    let mut zsh = Command::new("zsh");
    zsh.arg("-f")
        .current_dir(cwd)
        .env(
            "PATH",
            format!("{}:{path}", alt.parent().unwrap().display()),
        )
        .env("ALT_NO_DAEMON", "1")
        .envs(crate::TEST_SIGNING)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    zsh
}

/// What the zsh `script` prints when run in `cwd` with `args`.
pub fn zsh_says(cwd: &Path, script: &str, args: &[&str]) -> String {
    let done = zsh(cwd)
        .arg("-c")
        .arg(script)
        .arg("zsh")
        .args(args)
        .output();
    let done = done.unwrap();
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success() && stderr.is_empty(), "{stderr}");
    String::from_utf8(done.stdout).unwrap()
}

fn printed(args: &[&str]) -> String {
    let dir = alt_os::temp::tempdir().unwrap();
    let done = crate::alt_command()
        .current_dir(dir.path())
        .args(args)
        .output()
        .unwrap();
    assert!(done.status.success() && done.stderr.is_empty(), "{done:?}");
    String::from_utf8(done.stdout).unwrap()
}

#[test]
fn the_plugin_is_printed_whole_or_as_the_completion_file() {
    let whole = printed(&["shell", "zsh"]);
    assert!(whole.starts_with("_alt() {\n#compdef alt\n"), "{whole}");
    assert!(whole.ends_with("\n}\nalt_prompt_setup\n"));
    for function in std::fs::read_dir(plugin_dir().join("functions")).unwrap() {
        let function = function.unwrap();
        let name = function.file_name().into_string().unwrap();
        let body = std::fs::read_to_string(function.path()).unwrap();
        let defined = format!("{name}() {{\n{}\n}}\n", body.trim_end());
        assert!(
            whole.contains(&defined),
            "{name} is not printed as its file has it"
        );
    }
    let completion = printed(&["shell", "zsh", "--completion"]);
    assert_eq!(
        completion,
        std::fs::read_to_string(plugin_dir().join("_alt")).unwrap()
    );
}

#[test]
fn what_is_printed_and_every_file_of_the_plugin_is_zsh() {
    if no_zsh("what_is_printed_and_every_file_of_the_plugin_is_zsh") {
        return;
    }
    let dir = alt_os::temp::tempdir().unwrap();
    let whole = dir.path().join("whole.zsh");
    std::fs::write(&whole, printed(&["shell", "zsh"])).unwrap();
    let mut files = vec![whole, plugin_dir().join("_alt")];
    files.push(plugin_dir().join("alt.plugin.zsh"));
    let functions = std::fs::read_dir(plugin_dir().join("functions")).unwrap();
    files.extend(functions.map(|function| function.unwrap().path()));
    assert_eq!(files.len(), 13);
    for file in files {
        let checked = Command::new("zsh").arg("-n").arg(&file).output().unwrap();
        let said = String::from_utf8_lossy(&checked.stderr);
        assert!(checked.status.success(), "{}: {said}", file.display());
    }
}

#[test]
fn loading_the_plugin_says_nothing_either_way_it_is_loaded() {
    if no_zsh("loading_the_plugin_says_nothing_either_way_it_is_loaded") {
        return;
    }
    let dir = alt_os::temp::tempdir().unwrap();
    let defined = "print -r -- $+functions[alt_prompt_info]$+functions[_alt] ${precmd_functions[(I)alt_prompt_update]}";
    let printed = format!("eval \"$(alt shell zsh)\"; {defined}");
    assert_eq!(zsh_says(dir.path(), &printed, &[]), "11 1\n");
    let sourced = format!("source $1/alt.plugin.zsh; {defined}");
    let plugin = plugin_dir();
    assert_eq!(
        zsh_says(dir.path(), &sourced, &[plugin.to_str().unwrap()]),
        "11 1\n"
    );
}

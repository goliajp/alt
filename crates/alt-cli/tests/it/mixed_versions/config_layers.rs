//! A store an older program imported keeps one configuration file, what
//! the git repository had and what was set since in one: it is the
//! store's own from then on.

use super::{new_bin, ok, old_bin, run};

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_one_file_of_an_older_import_is_the_store_s_own() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, imported) = (dir.path().join("git"), dir.path().join("alt"));
    std::fs::create_dir(&source).unwrap();
    let git = |args: &[&str]| {
        let run = std::process::Command::new("git")
            .current_dir(&source)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap();
        ok(run)
    };
    git(&["init", "-q", "-b", "main", "."]);
    git(&["config", "user.name", "Source User"]);
    git(&["config", "user.email", "source@user.invalid"]);
    git(&["commit", "-q", "--allow-empty", "-m", "one"]);
    let target = imported.to_str().unwrap();
    ok(run(&old, &source, &["import", target]));
    ok(run(&old, &imported, &["config", "user.name", "Old Set"]));
    let own = imported.join(".alt/git-import/config");
    let kept = imported.join(".alt/git-import/source/config");
    assert!(!kept.exists());

    // the new program reads and writes the one file as the older does
    let value = |bin: &std::path::Path, key: &str| ok(run(bin, &imported, &["config", key]));
    assert_eq!(value(&new, "user.name"), "Old Set\n");
    assert_eq!(value(&new, "user.email"), "source@user.invalid\n");
    ok(run(&new, &imported, &["config", "core.quotepath", "false"]));
    assert_eq!(value(&old, "core.quotepath"), "false\n");
    let exported = dir.path().join("exported");
    ok(run(
        &new,
        &imported,
        &["export", exported.to_str().unwrap()],
    ));
    assert_eq!(
        std::fs::read(exported.join(".git/config")).unwrap(),
        std::fs::read(&own).unwrap()
    );

    // its next import keeps a copy beside the file and leaves the file
    git(&["config", "user.email", "two@user.invalid"]);
    git(&["config", "alias.st", "status"]);
    git(&["commit", "-q", "--allow-empty", "-m", "two"]);
    let before = std::fs::read(&own).unwrap();
    ok(run(&new, &source, &["import", target]));
    assert_eq!(std::fs::read(&own).unwrap(), before);
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        std::fs::read(source.join(".git/config")).unwrap()
    );
    // nothing tells what of the one file was set in the store: all of
    // it stands over the copy
    assert_eq!(value(&new, "user.name"), "Old Set\n");
    assert_eq!(value(&new, "user.email"), "source@user.invalid\n");
    assert_eq!(value(&new, "alias.st"), "status\n");
    // the older program goes on reading the one file it knows
    assert_eq!(value(&old, "user.name"), "Old Set\n");
    assert_eq!(value(&old, "core.quotepath"), "false\n");
}

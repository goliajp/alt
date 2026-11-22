//! `core.fsync` set in a file the configuration includes for one branch
//! (`includeIf "onbranch:…"`) has `alt add` sync on that branch and on no
//! other, as it has `git add`.

use std::fs;
use std::path::Path;

use super::scan_daemon::{Stop, daemon_pid, run};
use super::staged_durability::{repo, synced_to_the_ends};

fn add(dir: &Path, name: &str, daemon: bool) -> bool {
    fs::write(dir.join(name), name).unwrap();
    run(dir, &["add", name], daemon);
    synced_to_the_ends(dir)
}

#[test]
fn an_add_syncs_on_the_branch_core_fsync_is_included_for_and_on_no_other() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let own = dir.join(".alt/git-import");
    fs::write(own.join("on-topic"), "[core]\n\tfsync = added\n").unwrap();
    let mut config = fs::read_to_string(own.join("config")).unwrap();
    config.push_str("[includeIf \"onbranch:topic\"]\n\tpath = on-topic\n");
    fs::write(own.join("config"), config).unwrap();

    assert!(!add(dir, "b.txt", false), "main, run here");
    run(dir, &["commit", "-m", "second"], false);
    run(dir, &["switch", "-c", "topic"], false);
    assert!(add(dir, "c.txt", false), "topic, run here");
    assert!(add(dir, "d.txt", true), "topic, through the daemon");
    let _stop = Stop(daemon_pid(dir), false);
    run(dir, &["commit", "-m", "third"], true);
    run(dir, &["switch", "main"], true);
    assert!(!add(dir, "e.txt", true), "main, through the daemon");
}

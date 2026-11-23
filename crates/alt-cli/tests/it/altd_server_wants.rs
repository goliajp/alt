//! What a fetch may ask `altd-server` for: what the refs of the repository
//! reach, by whatever id, and nothing else that is in its store.

use std::fs;
use std::path::Path;
use std::process::Output;

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_pack_limits::{git, push, source};
use crate::altd_server_untrusted::repository;

fn done(out: Output) -> String {
    let told = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{told}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A commit in `src` that adds `file`. Its id.
fn commit(src: &Path, file: &str) -> String {
    fs::write(src.join(file), format!("{file}\n")).unwrap();
    done(git(src, &["add", "."]));
    done(git(src, &["commit", "-q", "-m", file]));
    done(git(src, &["rev-parse", "HEAD"]))
}

#[test]
fn a_fetch_is_given_what_the_refs_reach_and_nothing_else_of_the_store() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);
    let src = source(tmp.path(), "src");

    // a branch that is pushed and then deleted: its commit stays in the store
    let gone = commit(&src, "gone.txt");
    done(push(&src, &url, "gone"));
    done(git(&src, &["push", &url, ":refs/heads/gone"]));
    // a branch that stays, with a commit under the one it points at
    done(git(&src, &["reset", "-q", "--hard", "HEAD~1"]));
    let under = commit(&src, "under.txt");
    commit(&src, "kept.txt");
    done(push(&src, &url, "kept"));

    let fetch = |want: &str| {
        let into = tmp.path().join(format!("into-{want}"));
        fs::create_dir(&into).unwrap();
        done(git(&into, &["init", "-q", "."]));
        git(&into, &["-c", "protocol.version=2", "fetch", &url, want])
    };
    let refused = fetch(&gone);
    let told = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(!refused.status.success(), "{told}");
    let why = format!("remote error: upload-pack: not our ref {gone}");
    assert!(told.contains(&why), "{told}");
    // reached by a ref, though none points at it
    done(fetch(&under));
}

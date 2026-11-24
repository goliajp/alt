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

    // a partial clone asks for the files it left out by their ids
    let clone = [
        "clone",
        "-q",
        "--filter=blob:none",
        "-b",
        "kept",
        &url,
        "partial",
    ];
    done(git(tmp.path(), &clone));
    let partial = tmp.path().join("partial");
    assert_eq!(
        fs::read_to_string(partial.join("kept.txt")).unwrap(),
        "kept.txt\n"
    );
    assert_eq!(
        fs::read_to_string(partial.join("under.txt")).unwrap(),
        "under.txt\n"
    );
}

/// The status and the body of a `fetch` command of `lines`.
pub(crate) fn fetched(url: &str, dir: &Path, lines: &[u8]) -> (String, Vec<u8>) {
    let pkt = |line: &str| format!("{:04x}{line}", line.len() + 4).into_bytes();
    let mut body = [pkt("command=fetch\n"), b"0001".to_vec(), lines.to_vec()].concat();
    body.extend([pkt("done\n"), b"0000".to_vec()].concat());
    fs::write(dir.join("request"), body).unwrap();
    let out = std::process::Command::new("curl")
        .current_dir(dir)
        .args([
            "-sS",
            "--max-time",
            "120",
            "-w",
            "%{http_code}",
            "-o",
            "answer",
        ])
        .args(["-H", "Git-Protocol: version=2", "--data-binary", "@request"])
        .arg(format!("{url}git-upload-pack"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let status = String::from_utf8(out.stdout).unwrap();
    (status, fs::read(dir.join("answer")).unwrap())
}

/// A want sent a hundred thousand times is the want it is once, and a
/// request of more haves than any client sends is not looked at.
#[test]
fn the_lines_of_a_fetch_are_taken_once_each_and_within_a_number() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);
    let src = source(tmp.path(), "src");
    let tip = commit(&src, "one.txt");
    done(push(&src, &url, "one"));

    let once = fetched(&url, tmp.path(), format!("0032want {tip}\n").as_bytes());
    assert_eq!(once.0, "200");
    let often = format!("0032want {tip}\n").repeat(100_000);
    assert_eq!(fetched(&url, tmp.path(), often.as_bytes()), once);

    let haves = format!("0032have {tip}\n").repeat(alt_wire::fetch::MAX_HAVES + 1);
    let (status, why) = fetched(&url, tmp.path(), haves.as_bytes());
    assert_eq!(
        (status.as_str(), &why[..]),
        ("400", &b"too many have lines"[..])
    );
}

/// A commit the sender says it has is left out of what it is sent, with
/// all that is under it, where a ref reaches it; one that lies in the
/// store under no ref changes nothing of the answer, like one the store
/// never had.
#[test]
fn a_have_that_no_ref_reaches_is_one_the_server_does_not_know() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);
    let src = source(tmp.path(), "src");
    let gone = commit(&src, "gone.txt");
    done(push(&src, &url, "gone"));
    done(git(&src, &["push", &url, ":refs/heads/gone"]));
    done(git(&src, &["reset", "-q", "--hard", "HEAD~1"]));
    let under = commit(&src, "under.txt");
    let tip = commit(&src, "kept.txt");
    done(push(&src, &url, "kept"));

    let want = format!("0032want {tip}\n");
    let having = |have: &str| {
        let lines = format!("{want}0032have {have}\n");
        fetched(&url, tmp.path(), lines.as_bytes())
    };
    let all = fetched(&url, tmp.path(), want.as_bytes());
    assert_eq!(all.0, "200");
    let never = "1".repeat(40);
    assert_eq!(having(&never), all);
    assert_eq!(having(&gone), all);
    let less = having(&under);
    assert_eq!(less.0, "200");
    assert!(
        less.1.len() < all.1.len(),
        "{} {}",
        less.1.len(),
        all.1.len()
    );
}

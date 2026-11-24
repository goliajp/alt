//! Shallow fetches written line by line, as a client that is not `git` may
//! send them: `altd-server` answers each as `git upload-pack` answers it from
//! a mirror of the same history, where that answers.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_shallow::{done, git, history};
use crate::altd_server_untrusted::repository;
use crate::altd_server_wants::fetched;

fn pkt(line: &str) -> Vec<u8> {
    format!("{:04x}{line}", line.len() + 4).into_bytes()
}

/// What `git upload-pack` writes for a `fetch` command of `lines` read from
/// the repository at `mirror`, the way it is run behind HTTP.
fn theirs(mirror: &Path, lines: &[u8]) -> Vec<u8> {
    let mut body = [pkt("command=fetch\n"), b"0001".to_vec(), lines.to_vec()].concat();
    body.extend([pkt("done\n"), b"0000".to_vec()].concat());
    let mut git = Command::new("git")
        .args(["upload-pack", "--stateless-rpc"])
        .arg(mirror)
        .env("GIT_PROTOCOL", "version=2")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // it may be done with the request before all of it is written
    let _ = git.stdin.take().unwrap().write_all(&body);
    git.wait_with_output().unwrap().stdout
}

/// The lines of an answer before its pack, the ends of the history first and
/// sorted: those of its sections, with the marks between them, and the refusal
/// where it is one.
fn told(answer: &[u8]) -> Vec<String> {
    let (mut lines, mut rest) = (Vec::new(), answer);
    while rest.len() >= 4 {
        let size = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
        if size < 4 {
            lines.push(format!("{size:04x}"));
            rest = &rest[4..];
            continue;
        }
        let line = String::from_utf8_lossy(&rest[4..size])
            .trim_end()
            .to_owned();
        if line == "packfile" {
            break;
        }
        lines.push(line);
        rest = &rest[size..];
    }
    // the ends of a history are told in no order that means anything
    lines.sort_by_key(|line| !line.contains("shallow "));
    let ends = lines
        .iter()
        .take_while(|line| line.contains("shallow "))
        .count();
    lines[..ends].sort();
    lines
}

#[test]
fn shallow_fetches_written_by_hand_are_answered_as_git_answers_them() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);
    let src = tmp.path().join("src");
    history(&src);
    done(git(&src, &["push", "-q", &url, "work", "old", "v1"]));
    let bare = ["clone", "-q", "--bare", "src", "mirror.git"];
    done(git(tmp.path(), &bare));
    let mirror = tmp.path().join("mirror.git");
    let tip = done(git(&src, &["rev-parse", "work"]));
    let blob = done(git(&src, &["rev-parse", "work:f1"]));
    let nowhere = "1234567890123456789012345678901234567890";

    let want = |oid: &str| String::from_utf8(pkt(&format!("want {oid}\n"))).unwrap();
    let line = |line: &str| String::from_utf8(pkt(&format!("{line}\n"))).unwrap();
    // each as both answer it: the lines before the pack are the same
    let alike = |name: &str, lines: String| {
        let (status, ours) = fetched(&url, tmp.path(), lines.as_bytes());
        let ours = told(&ours);
        assert_eq!(status, "200", "{name}: {ours:?}");
        assert_eq!(ours, told(&theirs(&mirror, lines.as_bytes())), "{name}");
        ours
    };
    let one = alike("a depth of one", want(&tip) + &line("deepen 1"));
    assert_eq!(
        one,
        [
            format!("shallow {tip}"),
            "shallow-info".to_owned(),
            "0001".to_owned()
        ]
    );
    // a want no ref reaches is refused though the fetch asks for little of it
    let refused = alike("a want of nowhere", want(nowhere) + &line("deepen 1"));
    assert_eq!(refused, [format!("ERR upload-pack: not our ref {nowhere}")]);
    // a commit the sender says it holds without its parents, and has not
    let held = line(&format!("shallow {nowhere}"));
    alike("held of nowhere", want(&tip) + &held + &line("deepen 2"));
    // A depth and a ref together: git writes the head of its answer and ends
    // there, with no pack; here the fetch is told why it is given none.
    let both = want(&tip) + &line("deepen 1") + &line("deepen-not old");
    let ended = theirs(&mirror, both.as_bytes());
    assert_eq!(told(&ended), ["shallow-info"]);
    assert!(!ended.windows(8).any(|at| at == b"packfile"));
    let (status, ours) = fetched(&url, tmp.path(), both.as_bytes());
    let why = "ERR upload-pack: deepen and deepen-since (or deepen-not) cannot be used together";
    assert_eq!(
        (status.as_str(), told(&ours)),
        ("200", vec![why.to_owned()])
    );
    // as many refs left out as a request may name, and one more
    let nots = |count: usize| want(&tip) + &line("deepen-not old").repeat(count);
    let many = alike(
        "a ref left out many times",
        nots(alt_wire::fetch::MAX_DEEPEN_NOTS),
    );
    assert_eq!(many, alike("a ref left out", nots(1)));
    assert!(
        many.len() > 2 && many.contains(&"shallow-info".to_owned()),
        "{many:?}"
    );
    let over = nots(alt_wire::fetch::MAX_DEEPEN_NOTS + 1);
    let (status, _) = fetched(&url, tmp.path(), over.as_bytes());
    assert_eq!(status, "400");

    // What is held without parents and is no commit: git ends without an
    // answer; here the line says nothing of the history and the fetch is given.
    let held = line(&format!("shallow {blob}"));
    let lines = want(&tip) + &held + &line("deepen 1");
    assert!(told(&theirs(&mirror, lines.as_bytes())).is_empty());
    let (status, ours) = fetched(&url, tmp.path(), lines.as_bytes());
    assert_eq!((status.as_str(), told(&ours)), ("200", one));
}

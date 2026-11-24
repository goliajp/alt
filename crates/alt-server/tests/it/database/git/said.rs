//! What a client says it has is taken for held only where a ref leads to it.

use super::*;

/// Posts a fetch of `want` to the repository by a client that says it has
/// `have`, done or not; the body of the answer.
fn fetch(url: &str, token: &str, want: &str, have: Option<&str>, done: bool) -> Vec<u8> {
    use std::io::Read;
    let mut body = Vec::new();
    alt_wire::pkt::write_data(&mut body, b"command=fetch\n").unwrap();
    alt_wire::pkt::write_delim(&mut body).unwrap();
    alt_wire::pkt::write_data(&mut body, format!("want {want}\n").as_bytes()).unwrap();
    if let Some(have) = have {
        alt_wire::pkt::write_data(&mut body, format!("have {have}\n").as_bytes()).unwrap();
    }
    if done {
        alt_wire::pkt::write_data(&mut body, b"done\n").unwrap();
    }
    alt_wire::pkt::write_flush(&mut body).unwrap();
    let response = ureq::post(&format!("{url}/git-upload-pack"))
        .timeout(Duration::from_secs(30))
        .set("Content-Type", "application/x-git-upload-pack-request")
        .set("Git-Protocol", "version=2")
        .set("Authorization", &format!("Bearer {token}"))
        .send_bytes(&body)
        .unwrap();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).unwrap();
    bytes
}

/// A branch is pushed and deleted again: its commit stays in the segments, and no
/// ref leads to it. A client that names it is told nothing of it, and is sent what
/// one that named nothing is sent; a commit below the branch that is left is
/// acknowledged, and what it holds is left out.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_commit_no_ref_leads_to_is_not_acknowledged_and_leaves_nothing_out() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(work, &f.token);
    let commit = |name: &str| {
        std::fs::write(work.join(name), format!("{name}\n").repeat(400)).unwrap();
        ok(work, &f.token, &["add", "."]);
        ok(work, &f.token, &["commit", "-qm", name]);
        text(ok(work, &f.token, &["rev-parse", "HEAD"]))
    };
    let below = commit("below");
    let main = commit("main");
    ok(work, &f.token, &["checkout", "-qb", "gone"]);
    let gone = commit("gone");
    ok(work, &f.token, &["push", "-q", &url, "main", "gone"]);
    ok(work, &f.token, &["push", "-q", &url, ":gone"]);

    let said = |have: Option<&str>, done: bool| fetch(&url, &f.token, &main, have, done);
    let line = |text: &str| {
        let mut line = Vec::new();
        alt_wire::pkt::write_data(&mut line, text.as_bytes()).unwrap();
        line
    };
    let within = |answer: &[u8], line: &[u8]| answer.windows(line.len()).any(|at| at == line);
    let answer = said(Some(&gone), false);
    assert!(within(&answer, &line("NAK\n")), "{answer:?}");
    assert!(!within(&answer, gone.as_bytes()), "{answer:?}");
    let answer = said(Some(&below), false);
    assert!(
        within(&answer, &line(&format!("ACK {below}\n"))),
        "{answer:?}"
    );

    let all = said(None, true);
    assert_eq!(said(Some(&gone), true), all);
    assert!(said(Some(&below), true).len() < all.len());
}

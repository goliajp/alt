//! A fetch whose request runs out of time while its objects are chosen.

use super::*;
use std::io::{Read, Write};
use std::time::Instant;

/// A history for `git fast-import`: a directory of `files` files, and `commits`
/// commits that each give one of them other content.
fn history(files: usize, commits: usize) -> Vec<u8> {
    let mut stream = b"blob\nmark :1\ndata 2\nx\n\n".to_vec();
    let who = "committer Original Actor <original@example.test>";
    let commit = |stream: &mut Vec<u8>, at: usize| {
        write!(
            stream,
            "commit refs/heads/main\n{who} {} +0000\n",
            1_700_000_000 + at
        )
        .unwrap();
        stream.extend_from_slice(b"data 2\nc\n");
    };
    commit(&mut stream, 0);
    for file in 0..files {
        writeln!(stream, "M 100644 :1 d/f{file:05}").unwrap();
    }
    for at in 1..commits {
        commit(&mut stream, at);
        let content = format!("{at}\n");
        write!(
            stream,
            "M 100644 inline d/f{:05}\ndata {}\n{content}",
            at % files,
            content.len()
        )
        .unwrap();
    }
    stream
}

/// What a fetch of `want` by a client that has nothing is answered with: the
/// status, and how many bytes came.
fn fetched(url: &str, token: &str, want: &str) -> (u16, usize) {
    let mut body = Vec::new();
    alt_wire::pkt::write_data(&mut body, b"command=fetch\n").unwrap();
    alt_wire::pkt::write_delim(&mut body).unwrap();
    alt_wire::pkt::write_data(&mut body, format!("want {want}\n").as_bytes()).unwrap();
    alt_wire::pkt::write_data(&mut body, b"done\n").unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    let asked = ureq::post(&format!("{url}/git-upload-pack"))
        .timeout(Duration::from_secs(600))
        .set("Content-Type", "application/x-git-upload-pack-request")
        .set("Git-Protocol", "version=2")
        .set("Authorization", &format!("Bearer {token}"));
    let response = match asked.send_bytes(&body) {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).unwrap();
    (status, bytes.len())
}

fn healthy(server: &Running) -> bool {
    let asked = ureq::get(&format!("{}/healthz", server.url));
    matches!(asked.timeout(Duration::from_secs(30)).call(), Ok(response) if response.status() == 200)
}

/// The service has one worker. A fetch of most of a history of large trees takes
/// it for a while: that while is measured where requests have time enough. Where
/// they have an eighth of it, the fetch is told to come again when its time is
/// over, and the worker is there for another request long before the objects
/// would have been chosen.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_fetch_whose_time_is_over_chooses_no_further_and_gives_its_worker_back() {
    let f = Fixture::new();
    let parts = |time: Duration| {
        let (mut factory, mut options) = Running::parts(&f, None, vec![]);
        factory.limits.request_bytes = 1 << 30;
        factory.limits.graph_objects = 1 << 20;
        factory.limits.graph_edges = 1 << 30;
        factory.limits.structured_bytes = 1 << 31;
        factory.limits.tree_paths = 1 << 20;
        factory.limits.walk_steps = 1 << 30;
        factory.limits.total_bytes = 1 << 34;
        factory.statement_timeout = Duration::from_secs(300);
        // its bounds are held against those of requests, which are far from
        // what an instance has
        options.native.portable = None;
        options.workers = 1;
        options.work_timeout = time;
        options.body_timeout = Duration::from_secs(600);
        (factory, options)
    };
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    let mut importing = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(work)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stream = importing.stdin.take().unwrap();
    stream.write_all(&history(8000, 3000)).unwrap();
    drop(stream);
    assert!(importing.wait().unwrap().success());
    // a branch beside the one that is asked for: the fetch is not of all there is
    ok(work, &f.token, &["branch", "older", "main~1"]);
    let older = text(ok(work, &f.token, &["rev-parse", "older"]));

    let long = Duration::from_secs(600);
    let (factory, options) = parts(long);
    let server = Running::serve(factory, options, None, false);
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["push", "-q", &url, "main", "older"]);
    let began = Instant::now();
    let (status, bytes) = fetched(&url, &f.token, &older);
    let whole = began.elapsed();
    assert_eq!(status, 200);
    assert!(bytes > 1000, "{bytes} bytes");
    assert!(
        whole >= Duration::from_secs(2),
        "too short to tell: {whole:?}"
    );
    drop(server);

    let time = whole / 8;
    let (factory, options) = parts(time);
    let server = Running::serve(factory, options, None, false);
    let url = format!("{}/git/repository", server.url);
    let began = Instant::now();
    let (status, _) = fetched(&url, &f.token, &older);
    let answered = began.elapsed();
    assert_eq!(status, 503);
    assert!(
        answered >= time && answered < time + whole / 4,
        "{answered:?} of {whole:?}"
    );
    while !healthy(&server) {
        let since = began.elapsed() - answered;
        assert!(
            since < whole / 4,
            "the worker is kept for {since:?} of {whole:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A history of three hundred commits of one tree. What a fetch of most of it sends is
/// chosen in a walk that reads that tree and goes through every commit: the
/// commits are steps of the walk as the trees are, and a walk that may take
/// fewer steps than there are commits is refused.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn the_commits_a_fetch_goes_through_are_steps_of_its_walk() {
    let f = Fixture::new();
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    let mut stream = b"blob\nmark :1\ndata 2\nx\n\n".to_vec();
    let who = "commit refs/heads/main\ncommitter Original Actor <original@example.test>";
    for at in 0..300 {
        write!(stream, "{who} {at} +0000\ndata 2\nc\n").unwrap();
        if at == 0 {
            stream.extend_from_slice(b"M 100644 :1 file\n");
        }
    }
    let mut importing = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(work)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    importing.stdin.take().unwrap().write_all(&stream).unwrap();
    assert!(importing.wait().unwrap().success());
    // a branch beside the one that is asked for: a fetch of every tip is sent with no walk
    ok(work, &f.token, &["branch", "older", "main~1"]);
    let tip = text(ok(work, &f.token, &["rev-parse", "older"]));
    let server = |steps: usize| {
        let (mut factory, options) = Running::parts(&f, None, vec![]);
        factory.limits.walk_steps = steps;
        Running::serve(factory, options, None, false)
    };
    let all = server(1 << 30);
    let url = format!("{}/git/repository", all.url);
    ok(work, &f.token, &["push", "-q", &url, "main", "older"]);
    assert_eq!(fetched(&url, &f.token, &tip).0, 200);
    drop(all);
    let few = server(100);
    let url = format!("{}/git/repository", few.url);
    assert_ne!(fetched(&url, &f.token, &tip).0, 200);
}

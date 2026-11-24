//! A push under which the segments move each time it comes to be published.
use super::*;
use std::sync::mpsc::{Receiver, channel};

const WAITING: &str = "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'";

/// Waits until `count` transactions wait for a lock.
fn waiting(f: &Fixture, count: i64) {
    let until = std::time::Instant::now() + Duration::from_secs(120);
    while f.client().query_one(WAITING, &[]).unwrap().get::<_, i64>(0) != count {
        assert!(std::time::Instant::now() < until, "not {count} that wait");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A push with as long as these take to be let through, one after another.
fn receive(url: &str, token: &str, body: &[u8]) -> (u16, String) {
    let request = ureq::post(&format!("{url}/git-receive-pack"))
        .timeout(Duration::from_secs(300))
        .set("Content-Type", "application/x-git-receive-pack-request")
        .set("Authorization", &format!("Bearer {token}"));
    let response = match request.send_bytes(body) {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("push request failed: {error}"),
    };
    (response.status(), response.into_string().unwrap())
}

/// What moves the live segments of the repository between two looks at
/// them: two of them change places. Nothing is taken out of them, which
/// would have the server look for what to remove, and wait for the row too.
const MOVE: &str = "UPDATE alt_pack_segments SET sequence=(SELECT sum(sequence) FROM alt_pack_segments WHERE repository='repository' AND segment_id=ANY($1))-sequence WHERE repository='repository' AND segment_id=ANY($1)";

/// The live segments move every time a large push comes to its publication:
/// a transaction that holds the row of the repository while the push waits
/// for it changes them, and commits. Three times the push finds them other
/// than it was told behind, and is told anew. The fourth time it takes the
/// row before it looks at them, and what was done to them while it waited is
/// what it is told behind: it is published. Its pack is as it was pushed,
/// and read by a clone.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_the_segments_move_under_each_time_is_published_the_fourth_time_it_comes_to_it() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let url = format!("{}/git/repository", server.url);
    let none = id(ObjectKind::Tree, b"");
    // the segments that are moved: of branches the large push has nothing of
    for name in ["one", "other"] {
        let small = creating(name, &none, &[entry(2, b"", b"")]);
        let (status, report) = receive(&url, &f.token, &small);
        assert!(
            status == 200 && report.contains("ok refs/heads/"),
            "{report}"
        );
    }
    let rows = (f.client())
        .query(
            "SELECT segment_id FROM alt_pack_segments WHERE repository='repository'",
            &[],
        )
        .unwrap();
    let moved: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
    assert_eq!(moved.len(), 2);
    let large: Vec<u8> = (0..200_000u32)
        .flat_map(|n| (n.wrapping_mul(2654435761)).to_le_bytes())
        .collect();
    let file = id(ObjectKind::Blob, &large);
    let tree = [&b"100644 file\0"[..], file.as_bytes()].concat();
    let entries = [entry(2, b"", &tree), entry(3, b"", &large)];
    let big = creating("large", &id(ObjectKind::Tree, &tree), &entries);
    // one that holds the row of the repository until it is told to let go, and
    // then moves the segments, or not, before it does
    let holder = || -> (Receiver<()>, std::sync::mpsc::Sender<bool>) {
        let (held, is_held) = channel();
        let (let_go, told) = channel::<bool>();
        let (mut client, moved) = (f.client(), moved.clone());
        std::thread::spawn(move || {
            let mut holding = client.transaction().unwrap();
            holding.query_one(HELD, &[]).unwrap();
            held.send(()).unwrap();
            if told.recv() == Ok(true) {
                assert_eq!(holding.execute(MOVE, &[&moved]).unwrap(), 2);
            }
            holding.commit().unwrap();
        });
        (is_held, let_go)
    };
    let rounds = std::thread::scope(|scope| {
        let (is_held, mut let_go) = holder();
        is_held.recv().unwrap();
        // the large push waits for the row, verified behind the segments as they are
        let (done, large) = channel();
        let (to, token, body) = (&url, &f.token, &big);
        scope.spawn(move || done.send(receive(to, token, body)));
        waiting(&f, 1);
        let mut rounds = 0;
        let report = loop {
            // behind it the next that holds the row
            let (is_held, next) = holder();
            waiting(&f, 2);
            // the segments move, and the large push comes to them
            let_go.send(true).unwrap();
            rounds += 1;
            is_held.recv().unwrap();
            let_go = next;
            // it is either published, or waits for the row again
            let until = std::time::Instant::now() + Duration::from_secs(120);
            let ended = loop {
                if let Ok(report) = large.try_recv() {
                    break Some(report);
                }
                let waits: i64 = f.client().query_one(WAITING, &[]).unwrap().get(0);
                if waits == 1 {
                    // it may have been published in this very moment
                    break large.recv_timeout(Duration::from_millis(500)).ok();
                }
                assert!(
                    std::time::Instant::now() < until,
                    "neither published nor waiting"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            if let Some(report) = ended {
                break report;
            }
            assert!(rounds < 12, "the large push is sent back without end");
        };
        let_go.send(false).unwrap();
        assert!(
            report.0 == 200 && report.1.contains("ok refs/heads/large"),
            "{report:?}"
        );
        rounds
    });
    // three times sent back, and published the fourth time whatever was done before it
    assert_eq!(rounds, 4);
    let temp = alt_os::temp::tempdir().unwrap();
    let checked = [
        "-c",
        "transfer.fsckObjects=true",
        "clone",
        "-q",
        "--branch",
        "large",
    ];
    ok(
        temp.path(),
        &f.token,
        &[&checked[..], &[&url, "all"]].concat(),
    );
    let all = temp.path().join("all");
    ok(&all, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(std::fs::read(all.join("file")).unwrap(), large);
}

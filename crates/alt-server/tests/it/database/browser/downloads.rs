//! What is written out to be downloaded takes of the room the responses to
//! fetches have on disk.
use super::*;
use crate::database::git::{identity, ok, text};
use alt_server::shares::Shares;
use std::io::Read;

const ROOM: u64 = 24 << 20;
const FILE: usize = 9_500_000;

/// The room is twenty-four megabytes and the file a browser downloads nine and
/// a half. An address that has another taker in the room is held to its quarter
/// and told to come again; another address is given the file, and the room is
/// given back when it is sent.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by browser acceptance and closing gate"]
fn a_file_written_out_for_a_browser_takes_of_the_room_and_an_address_a_quarter_of_it() {
    let mut f = Fixture::new();
    let room = Shares::passed_by_one_alone(ROOM);
    let server = crate::database::sign_in_deadline::start(&mut f, |_, options| {
        options.sending = room.clone()
    });
    let (store, admin) = (&mut f.store, &f.credential);
    store
        .set_grants(admin, "repository", "reader", true, false, &[])
        .unwrap();
    let (cookie, _) = login(&server, "reader");

    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(work, &f.token);
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let noise: Vec<u8> = (0..FILE)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect();
    std::fs::write(work.join("noise"), &noise).unwrap();
    ok(work, &f.token, &["add", "."]);
    ok(work, &f.token, &["commit", "-qm", "noise"]);
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["push", "-q", &url, "main"]);
    let blob = text(ok(work, &f.token, &["rev-parse", "HEAD:noise"]));

    let download = |from: &str| {
        let path = format!("/web/repositories/repository/objects/{blob}/download");
        let asked = ureq::get(&format!("{}{path}", server.url))
            .timeout(Duration::from_secs(60))
            .set("Cookie", &cookie)
            .set("X-Forwarded-For", from);
        let response = match asked.call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("{error}"),
        };
        let again = response.header("Retry-After").map(str::to_owned);
        let named = response.header("Content-Disposition").is_some();
        let status = response.status();
        let mut body = Vec::new();
        response.into_reader().read_to_end(&mut body).unwrap();
        (status, again, named, body)
    };
    let (one, other) = ("203.0.113.9", "203.0.113.10");
    let mut mine = room.share(Some(one.parse().unwrap()));
    mine.take(1).unwrap();
    let (status, again, ..) = download(one);
    assert_eq!((status, again.as_deref()), (503, Some("60")));
    let (status, again, named, body) = download(other);
    assert_eq!((status, again, named), (200, None, true));
    assert!(body == noise);
    let began = std::time::Instant::now();
    while room.held() > 1 {
        assert!(began.elapsed() < Duration::from_secs(30), "{}", room.held());
        std::thread::sleep(Duration::from_millis(20));
    }
}

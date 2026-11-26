//! The archive of a migration and each file of it are written out before they
//! are sent, and take of the room the responses to fetches have on disk.
use super::migration::{migration_payload_of, migration_send};
use super::*;
use alt_server::shares::Shares;
use std::io::Read;

const ROOM: u64 = 48 << 20;
const FILE: usize = 9_000_000;

/// The room is forty-eight megabytes and the one file of the repository nine,
/// within the quarter of an address. An address whose other takers have its
/// quarter is held to it, for the archive and for the file alone, and told to
/// come again; another address is given each, and the room is given back when
/// they are sent.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn the_archive_of_a_migration_and_a_file_of_it_take_of_the_room_and_an_address_a_quarter() {
    let mut f = Fixture::new();
    user(&mut f, "browser", true);
    let (store, admin) = (&mut f.store, &f.credential);
    store
        .set_grants(admin, "repository", "browser", true, true, &grants())
        .unwrap();
    let room = Shares::passed_by_one_alone(ROOM, 1);
    let server = crate::database::sign_in_deadline::start(&mut f, |_, options| {
        options.sending = room.clone()
    });
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let noise = |_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    let noise: Vec<u8> = (0..FILE).map(noise).collect();
    let payload = migration_payload_of(noise.clone());
    let native = crate::database::portable::client(&server, &f.token);
    crate::database::portable::publish(&native, &payload);
    let (cookie, session) = login(&server, "browser");
    let csrf = session["csrf"].as_str().unwrap();
    let export = "/web/repositories/repository/epochs/1/portable/exports/room-export";
    let made = migration_send(
        &server,
        "POST",
        export,
        Some(&cookie),
        Some(ORIGIN),
        Some(csrf),
        b"",
    );
    assert_eq!(made.status(), 200);
    let digest = alt_hash::blake3::hash(&noise).to_hex().to_string();

    let download = |path: &str, from: &str| {
        let asked = ureq::get(&format!("{}{path}", server.url))
            .timeout(Duration::from_secs(60))
            .set("Cookie", &cookie)
            .set("X-Forwarded-For", from);
        let response = match asked.call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("{error}"),
        };
        let again = response.header("Retry-After").map(str::to_owned);
        let status = response.status();
        let mut body = Vec::new();
        response.into_reader().read_to_end(&mut body).unwrap();
        (status, again, body)
    };
    let (one, other) = ("203.0.113.9", "203.0.113.10");
    let mut mine = room.share(Some(one.parse().unwrap()));
    mine.take(ROOM / 4).unwrap();
    let given_back = || {
        let began = std::time::Instant::now();
        while room.held() > ROOM / 4 {
            assert!(began.elapsed() < Duration::from_secs(30), "{}", room.held());
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let archive = format!("{export}/archive");
    let (status, again, _) = download(&archive, one);
    assert_eq!((status, again.as_deref()), (503, Some("60")));
    let (status, again, body) = download(&archive, other);
    assert_eq!((status, again), (200, None));
    assert!(body.len() > FILE);
    given_back();
    let file = format!("{export}/content/{digest}");
    let (status, again, _) = download(&file, one);
    assert_eq!((status, again.as_deref()), (503, Some("60")));
    let (status, again, body) = download(&file, other);
    assert_eq!((status, again), (200, None));
    assert!(body == noise);
    given_back();
}

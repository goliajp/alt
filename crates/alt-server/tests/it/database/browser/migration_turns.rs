//! A download of a migration that waits for its turn to be large holds neither
//! the worker nor a connection of the database meanwhile.
use super::migration::{migration_payload_of, migration_send};
use super::*;
use alt_server::shares::Shares;
use std::collections::BTreeSet;
use std::io::Read;
use std::time::Instant;

/// a quarter of it is less than the file, half of it more
const ROOM: u64 = 24 << 20;
const FILE: usize = 9_000_000;

/// the connections the database has, other than that of `client`
fn backends(client: &mut alt_pg::Client) -> BTreeSet<i32> {
    let asked = "SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid()";
    let rows = client.query(asked, &[]).unwrap();
    rows.iter().map(|row| row.get(0)).collect()
}

fn until_waiting(room: &Shares, count: usize) {
    let by = Instant::now() + Duration::from_secs(60);
    while room.waiting() != count {
        assert!(Instant::now() < by, "{} wait, not {count}", room.waiting());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// until what was sent before has given its room back
fn until_given_back(room: &Shares) {
    let by = Instant::now() + Duration::from_secs(60);
    while room.held() != 0 {
        assert!(Instant::now() < by, "{} held", room.held());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// One worker; the turn to be large is taken by the test for an address. A
/// download of the archive of an export from another address, through the
/// browser and through the native protocol, and of its one file, waits for
/// the turn: meanwhile a request that takes a worker and a store is answered,
/// on a connection of the database that was there before, and the download
/// is answered whole once the turn is given back.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn a_download_of_a_migration_that_waits_for_its_turn_holds_no_worker_and_no_connection() {
    let mut f = Fixture::new();
    user(&mut f, "browser", true);
    let (store, admin) = (&mut f.store, &f.credential);
    store
        .set_grants(admin, "repository", "browser", true, true, &grants())
        .unwrap();
    let room = Shares::passed_by_one_alone(ROOM, 1);
    let server = crate::database::sign_in_deadline::start(&mut f, |_, options| {
        options.sending = room.clone();
        options.workers = 1;
        options.work_timeout = Duration::from_secs(120);
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
    let export = "repositories/repository/epochs/1/portable/exports/turn-export";
    let made = migration_send(
        &server,
        "POST",
        &format!("/web/{export}"),
        Some(&cookie),
        Some(ORIGIN),
        Some(csrf),
        b"",
    );
    assert_eq!(made.status(), 200);
    let digest = alt_hash::blake3::hash(&noise).to_hex().to_string();

    let (one, other) = ("203.0.113.9", "203.0.113.10");
    let mut client = f.client();
    let download = |path: String, who: (&'static str, String)| {
        let url = format!("{}{path}", server.url);
        std::thread::spawn(move || {
            let asked = ureq::get(&url)
                .timeout(Duration::from_secs(120))
                .set(who.0, &who.1)
                .set("X-Forwarded-For", other);
            let response = match asked.call() {
                Ok(response) | Err(ureq::Error::Status(_, response)) => response,
                Err(error) => panic!("{error}"),
            };
            let status = response.status();
            let mut body = Vec::new();
            response.into_reader().read_to_end(&mut body).unwrap();
            (status, body)
        })
    };
    let browser = ("Cookie", cookie.clone());
    let bearer = ("Authorization", format!("Bearer {}", f.token));
    let asked = [
        (format!("/web/{export}/archive"), browser.clone()),
        (format!("/v1/{export}/archive"), bearer),
        (format!("/web/{export}/content/{digest}"), browser),
    ];
    for (path, who) in asked {
        until_given_back(&room);
        let mut kept = room.share(Some(one.parse().unwrap()));
        kept.take(ROOM / 4 + 1).unwrap();
        let waiting = download(path.clone(), who);
        until_waiting(&room, 1);
        let before = backends(&mut client);
        let metrics = server.send("GET", "/v1/instance/metrics", Some(&f.token), b"");
        assert_eq!(metrics.status(), 200, "{path}");
        let after = backends(&mut client);
        assert!(
            after.is_subset(&before),
            "{path}: {:?} were not there before",
            after.difference(&before).collect::<Vec<_>>()
        );
        assert!(!waiting.is_finished(), "{path}");
        drop(kept);
        let (status, body) = waiting.join().unwrap();
        assert_eq!(
            status,
            200,
            "{path}: {:.200}",
            String::from_utf8_lossy(&body)
        );
        match path.contains("/content/") {
            true => assert!(body == noise, "{path}"),
            false => assert!(body.len() > FILE, "{path}: {}", body.len()),
        }
    }
}

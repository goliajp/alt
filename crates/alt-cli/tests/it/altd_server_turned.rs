//! A large response to a fetch that was put off to wait for its turn is built
//! again once it has it, in a time of its own: what it took to be built
//! first and to wait leaves it no less.

use std::io::Read;
use std::time::{Duration, Instant};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_fetch_time::of_many_files;
use crate::altd_server_sending::{answer, asked_for};

/// the bytes of an answer after its head
fn body(answer: &[u8]) -> &[u8] {
    let head = answer
        .windows(4)
        .position(|end| end == b"\r\n\r\n")
        .unwrap();
    &answer[head + 4..]
}

/// Two addresses fetch a history whose pack takes a while to be built, and
/// the response is large: the first keeps its turn, unread, and the second is
/// put off once its pack is built. It is let have its turn where less is left
/// of the time a fetch may take than building the pack takes, and still is
/// answered, whole. A fetch may take three times what one alone takes.
#[test]
fn a_fetch_put_off_for_its_turn_is_built_again_in_a_time_of_its_own() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_many_files(&origin);
    let repo = ("ALT_SERVER_REPO", origin.to_str().unwrap());
    let server = spawn_server_with_env(&[repo]);
    let began = Instant::now();
    let whole = answer(asked_for(&server.addr, &head, Some("203.0.113.8")));
    let alone = began.elapsed();
    assert!(
        whole.starts_with(b"HTTP/1.1 200"),
        "{:.300}",
        String::from_utf8_lossy(&whole)
    );
    assert!(
        alone >= Duration::from_secs(1),
        "too short to tell: {alone:?}"
    );
    drop(server);

    let time = alone * 3;
    // a quarter of the room is half the response: it is large, and the room has it alone
    let (most, fetch) = ((whole.len() * 2).to_string(), time.as_millis().to_string());
    let server = spawn_server_with_env(&[
        repo,
        ("ALT_SERVER_WORKERS", "2"),
        ("ALT_SERVER_MAX_SENDING_BYTES", &most),
        ("ALT_SERVER_MAX_FETCH_MS", &fetch),
    ]);
    let mut kept = asked_for(&server.addr, &head, Some("203.0.113.9"));
    let mut begun = [0u8; 12];
    kept.read_exact(&mut begun).unwrap();
    assert_eq!(&begun, b"HTTP/1.1 200");
    let began = Instant::now();
    let waiter = {
        let (address, head) = (server.addr.clone(), head.clone());
        std::thread::spawn(move || answer(asked_for(&address, &head, Some("203.0.113.10"))))
    };
    // its pack is built by then, and less is left of its time than that took
    std::thread::sleep(time - alone / 2);
    let let_in = began.elapsed();
    drop(kept);
    let answered = waiter.join().unwrap();
    let ended = began.elapsed();
    assert!(
        answered.starts_with(b"HTTP/1.1 200") && body(&answered).len() == body(&whole).len(),
        "let in after {let_in:?}, answered after {ended:?} of {time:?}, alone {alone:?}: {:.300}",
        String::from_utf8_lossy(&answered)
    );
}

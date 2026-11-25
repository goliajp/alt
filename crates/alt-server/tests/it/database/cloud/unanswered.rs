//! A platform that gives no answer is asked by one request, not by each.
use super::*;
use std::time::Instant;

/// the wait after the first asking that got no answer
const FIRST: Duration = Duration::from_secs(1);

/// The lease of an account nears its end and the platform is out, saying so
/// only half a second after it is asked. Of the requests that come with that
/// account while one asks, and in the wait after it got no answer, none asks:
/// they go on with what the lease still allows. Whether a request came in that
/// wait is read from the clock, not taken from how soon it was sent: one that
/// a slow machine brings past the wait may ask, and is held to have asked
/// past it.
#[test]
#[ignore = "requires isolated PostgreSQL and actual loopback HTTP platform fixture"]
fn a_platform_that_gives_no_answer_is_asked_by_one_request_and_then_left_alone() {
    let mut f = Fixture::new();
    let platform = Platform::start(&f);
    let Instance { server, token, .. } = instance(&mut f, &platform, |_| ());
    let (mode, calls) = (&platform.mode, &platform.calls);
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    let asked = calls.load(Ordering::SeqCst);
    // within the ten seconds of its end in which it is renewed, with most of them left
    let near = (time() + 9_500).to_string();
    f.client().execute("UPDATE alt_cloud_credentials SET claims=jsonb_set(claims,'{lease,expires_at_unix_ms}',to_jsonb($1::text))", &[&near]).unwrap();
    mode.store(4, Ordering::SeqCst);
    // every asking after one without an answer came past the wait that one began
    let left_alone = || {
        let told = platform.told.lock().unwrap();
        for (nth, pair) in told[asked..].windows(2).enumerate() {
            let waited = pair[1].0.duration_since(pair[0].1);
            assert!(
                waited >= FIRST * (1 << nth),
                "asked {waited:?} after no answer"
            );
        }
        told[asked].1
    };
    // one request is under way, and others come while it waits for the platform
    let statuses: Vec<u16> = std::thread::scope(|scope| {
        let first = scope.spawn(|| server.command("GET", "/v1/capabilities", &token).0);
        let mut statuses: Vec<u16> = (0..4)
            .map(|_| server.command("GET", "/v1/capabilities", &token).0)
            .collect();
        statuses.push(first.join().unwrap());
        statuses
    });
    assert_eq!(statuses, [200; 5]);
    // right after no answer nobody asks
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    let (answered, now) = (left_alone(), Instant::now());
    let asked_since = calls.load(Ordering::SeqCst) - asked;
    assert!(asked_since == 1 || now.duration_since(answered) >= FIRST);
    // and once the platform is back and the wait is over, one does
    mode.store(0, Ordering::SeqCst);
    let (began, before) = (Instant::now(), calls.load(Ordering::SeqCst));
    while calls.load(Ordering::SeqCst) == before {
        assert!(began.elapsed() < Duration::from_secs(30), "not asked again");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    }
    platform.end();
}

//! A platform that gives no answer is asked by one request, not by each.
use super::*;

/// The lease of an account nears its end and the platform is out, saying so
/// only half a second after it is asked. Of the requests that come with that
/// account while one asks, and right after it got no answer, none asks: they
/// go on with what the lease still allows.
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
    assert_eq!(calls.load(Ordering::SeqCst), asked + 1);
    // right after no answer nobody asks
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    assert_eq!(calls.load(Ordering::SeqCst), asked + 1);
    // and once the platform is back and the wait is over, one does
    mode.store(0, Ordering::SeqCst);
    let began = std::time::Instant::now();
    while calls.load(Ordering::SeqCst) == asked + 1 {
        assert!(began.elapsed() < Duration::from_secs(30), "not asked again");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    }
    platform.end();
}

//! What the refs lead to, looked for on behalf of a request nobody waits for
//! any more.
use super::*;
use crate::database::native_led::{client, sent, without_content};
use crate::database::sign_in_deadline::{served, start};
use std::sync::atomic::AtomicBool;

/// A repository that had a branch deleted gives a Git object by the digest of
/// its content where the refs lead to it, which takes reading every tree they
/// reach. A request that is waited for no longer does not have that read: the
/// worker it took is free again without it. Told here by what is kept of such
/// a reading for the next that asks: after a request that ran out of time
/// before its worker came to it, nothing is kept; after one that was answered,
/// it is. The same of the page of a browser and of the native route.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn what_the_refs_lead_to_is_not_read_for_a_request_that_is_waited_for_no_longer() {
    let mut f = Fixture::new();
    let kept = Payload::new("the-branch-that-stays", None);
    let cookie = {
        let server = start(&mut f, |_, _| ());
        let client = client(&server, &f.token);
        let gone = Payload::new("the-branch-that-goes", None);
        assert!(sent(&client, &gone));
        let mut deletion = without_content(Payload::new("remove-it", Some(gone.commit)));
        deletion.publication.refs[0].new = None;
        assert!(sent(&client, &deletion));
        assert!(sent(&client, &kept));
        let (store, admin) = (&mut f.store, &f.credential);
        store
            .set_grants(admin, "repository", "reader", true, false, &[])
            .unwrap();
        login(&server, "reader").0
    };
    let digest = &kept.manifest.entries[1].blake3;
    // one worker, and a second and a half for an answer
    let server = served(&f, |_, options| {
        options.workers = 1;
        options.work_timeout = Duration::from_millis(1500);
    });
    let pages = [
        (
            format!("/web/repositories/repository/content/{digest}/download"),
            Some(cookie.as_str()),
        ),
        (
            format!("/v1/repositories/repository/epochs/1/content/{digest}"),
            None,
        ),
    ];
    let token = f.token.clone();
    let asked = |(path, cookie): &(String, Option<&str>)| match cookie {
        Some(cookie) => send(&server, "GET", path, Some(cookie), None, None, None).status(),
        None => server.send("GET", path, Some(&token), b"").status(),
    };
    let steps = f.store.limits().walk_steps;
    let kept_for_the_next = |f: &mut Fixture| {
        let view = f.store.pack_view(&f.credential, "repository").unwrap();
        match view.led(steps, Some(&AtomicBool::new(true))) {
            Ok(_) => true,
            Err(alt_server_store::Error::Busy(_)) => false,
            Err(error) => panic!("{error}"),
        }
    };
    for page in &pages {
        // the worker waits for the refs, which are held from it past the time of the request
        let mut holding = f.client();
        holding
            .batch_execute("BEGIN; LOCK TABLE alt_refs IN ACCESS EXCLUSIVE MODE")
            .unwrap();
        assert_eq!(asked(page), 503, "{}", page.0);
        holding.batch_execute("ROLLBACK").unwrap();
        // the one worker is free once it answers another
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while server.command("GET", "/v1/capabilities", &token).0 != 200 {
            assert!(std::time::Instant::now() < until, "the worker is not free");
        }
        assert!(!kept_for_the_next(&mut f), "{}", page.0);
    }
    // waited for, it is read and kept
    assert_eq!(asked(&pages[1]), 200);
    assert!(kept_for_the_next(&mut f));
    assert_eq!(asked(&pages[0]), 200);
}

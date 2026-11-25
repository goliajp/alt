//! A publication that finds the lease of the service over, as its client is
//! told of it.
use super::*;

const ENDS: &str = "UPDATE alt_cloud_binding SET service=jsonb_set(service,'{lease,expires_at_unix_ms}',to_jsonb($1::text))";

/// The lease of the service ends between a request being made ready and its
/// publication, and the platform is out, so nothing renews it. That is no
/// fault of the request: its client is told 409 and to come again with the
/// same request, the request stays ready, and once the platform is back the
/// same request is published.
#[test]
#[ignore = "requires isolated PostgreSQL and actual loopback HTTP platform fixture"]
fn a_publication_that_finds_the_lease_of_the_service_over_is_told_to_come_again_as_it_is() {
    let mut f = Fixture::new();
    let platform = Platform::start(&f);
    platform.writes.store(true, Ordering::SeqCst);
    let Instance { server, token, .. } = instance(&mut f, &platform, |_| ());
    assert_eq!(server.command("GET", "/v1/capabilities", &token).0, 200);
    let credential = f.store.authenticate(&token).unwrap();
    let store = &mut f.store;
    store
        .create_repository(&credential, "repository", 100_000_000)
        .unwrap();
    store
        .set_grants(&credential, "repository", "owner", true, true, &grants())
        .unwrap();

    let payload = Payload::new("lease-over", None);
    let base = "/v1/repositories/repository/epochs/1/requests";
    let request = format!("{base}/owner/lease-over");
    let begin = alt_server::BeginRequest {
        publication: payload.publication.clone(),
        manifest: payload.manifest.clone(),
    };
    let (status, state) = server.json("POST", base, &token, &begin);
    assert_eq!(status, 200, "{state}");
    for (entry, bytes) in payload.manifest.entries.iter().zip(&payload.contents) {
        let path = format!("{request}/content/{}", entry.blake3);
        assert_eq!(server.send("PUT", &path, Some(&token), bytes).status(), 200);
    }
    let (status, state) = server.command("POST", &format!("{request}/prepare"), &token);
    assert_eq!((status, &state["result"]["state"]), (200, &"ready".into()));

    platform.mode.store(1, Ordering::SeqCst);
    let over = (time() - 1000).to_string();
    f.client().execute(ENDS, &[&over]).unwrap();
    let (status, refused) = server.command("POST", &format!("{request}/publish"), &token);
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["retry"], "same_request", "{refused}");
    assert_eq!(f.counts().0, 0);
    let ready: String = f
        .client()
        .query_one("SELECT state FROM alt_requests", &[])
        .unwrap()
        .get(0);
    assert_eq!(ready, "ready");

    platform.mode.store(0, Ordering::SeqCst);
    let (status, committed) = server.command("POST", &format!("{request}/publish"), &token);
    assert_eq!(status, 200, "{committed}");
    assert_eq!(committed["result"]["state"], "committed");
    assert_eq!(f.counts().0, 1);
    drop(server);
    platform.end();
}

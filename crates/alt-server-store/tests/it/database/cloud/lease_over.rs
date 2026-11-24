//! A publication and the lease of the service: one that is over leaves the request
//! as it was, whether it ended before the publication began or while it ran.

use super::*;

#[test]
#[ignore = "requires isolated PostgreSQL; publication commit must recheck after actual delayed SQL"]
fn cloud_lease_expiring_inside_publication_rolls_back_all_visible_state() {
    let mut f = cloud();
    repository(&mut f);
    let payload = Payload::new("expires-during-commit", None);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    f.client().batch_execute("CREATE FUNCTION delay_cloud_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(2); RETURN NEW; END $$; CREATE TRIGGER delay_cloud_commit BEFORE INSERT ON alt_refs FOR EACH ROW EXECUTE FUNCTION delay_cloud_commit()").unwrap();
    // The lease is to be good when the publication begins and over when its refs
    // are written, two seconds later: so it is given last, with nothing but its
    // installation before the publication.
    let mut short = service(3, 1, true);
    short.lease.expires_at_unix_ms = Counter(short.lease.issued_at_unix_ms.0 + 1500);
    f.store.refresh_cloud_service(&short).unwrap();
    // refused as what may come again as it is: 409 and `same_request` over HTTP
    let refused = f.store.publish(&f.credential, &work, &graph);
    assert!(
        matches!(refused, Err(alt_server_store::Error::Precondition(why)) if why.contains("not currently authorized"))
    );
    assert_eq!(f.counts(), (0, 0, 0));
    assert!(
        matches!(f.store.query(&f.credential,&payload.publication.key).unwrap(),RequestObservation::Recorded{result,..} if matches!(*result,RequestState::Ready{..}))
    );
    f.client()
        .batch_execute("DROP TRIGGER delay_cloud_commit ON alt_refs")
        .unwrap();
    f.store.refresh_cloud_service(&service(4, 1, true)).unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
}

/// The lease is over before the publication begins. That is nothing of the
/// request's: it is refused for now and stays ready, as it does where the lease
/// ends while it is published, and is published under the next lease.
#[test]
#[ignore = "requires isolated PostgreSQL; a lease that is over ends no request"]
fn cloud_lease_over_before_publication_leaves_the_request_ready_for_the_next_lease() {
    let mut f = cloud();
    repository(&mut f);
    let payload = Payload::new("lease-over-before", None);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    let mut short = service(3, 1, true);
    short.lease.expires_at_unix_ms = Counter(short.lease.issued_at_unix_ms.0 + 1000);
    f.store.refresh_cloud_service(&short).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // refused as what may come again as it is: 409 and `same_request` over HTTP
    let refused = f.store.publish(&f.credential, &work, &graph);
    assert!(
        matches!(refused, Err(alt_server_store::Error::Precondition(why)) if why.contains("not currently authorized"))
    );
    assert_eq!(f.counts(), (0, 0, 0));
    assert!(
        matches!(f.store.query(&f.credential,&payload.publication.key).unwrap(),RequestObservation::Recorded{result,..} if matches!(*result,RequestState::Ready{..}))
    );
    f.store.refresh_cloud_service(&service(4, 1, true)).unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
}

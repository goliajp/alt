//! The segments of a Cloud instance read, and one that does not give its objects
//! back replaced.
use super::*;
use crate::database::pack_scrub::{memory, planted, states, turned};

/// A Cloud instance holds every transaction to the clock and the authority it
/// has recorded. Its segments are read and one of them is replaced as on any
/// other instance, while its service allows writes.
#[test]
#[ignore = "requires isolated PostgreSQL; scrub and its replacement under Cloud authority"]
fn cloud_segment_that_does_not_give_its_objects_back_is_indexed_anew() {
    let mut f = cloud();
    repository(&mut f);
    Payload::new("first", None).publish_new(&mut f.store, &f.credential);
    let row = f
        .client()
        .query_one(
            "SELECT segment_id,sequence,inline FROM alt_pack_segments WHERE state='live'",
            &[],
        )
        .unwrap();
    let (first, sequence): (String, i64) = (row.get(0), row.get(1));
    let written: Vec<u8> = (row.get::<_, Option<Vec<u8>>>(2)).unwrap_or_else(|| {
        std::fs::read(f.store.pack_path("repository", &first).unwrap()).unwrap()
    });
    let astray = "a".repeat(32);
    let mut bytes = written.clone();
    turned(&mut bytes);
    planted(&f, &astray, sequence + 1, &bytes);

    let found = f
        .store
        .scrub_packs(&f.credential, "repository", None, &mut |_, _, _| ());
    let found = found.unwrap();
    assert_eq!((found.segments, found.objects), (2, 4));
    assert_eq!(found.unread.len(), 1);
    assert_eq!(found.unread[0].segment, astray);
    let mended = f.store.scrub_packs(
        &f.credential,
        "repository",
        Some(memory()),
        &mut |_, _, _| (),
    );
    let anew = mended.unwrap().unread[0].replaced_by.clone().unwrap();
    let live = "live".to_owned();
    assert_eq!(
        states(&f),
        [
            (first, live.clone()),
            (anew, live),
            (astray, "retired".to_owned())
        ]
    );
    let found = f.store.scrub_packs(
        &f.credential,
        "repository",
        Some(memory()),
        &mut |_, _, _| (),
    );
    assert!(found.unwrap().unread.is_empty());
    assert_eq!(
        f.store.repositories_to_scrub(&f.credential).unwrap(),
        ["repository"]
    );
}

//! Reading every object of a repository's live segments, and replacing a segment
//! that does not give its objects back by its pack indexed anew.
use super::*;

/// memory for the pack of a segment to be indexed anew in
pub(super) fn memory() -> &'static alt_pack_segment::Memory {
    Box::leak(Box::new(alt_pack_segment::Memory::new(1 << 30)))
}

/// Has the first record of the bytes of a segment tell of the entry of the second.
pub(super) fn turned(bytes: &mut [u8]) {
    let end = bytes.len() - 64;
    let pack = u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) as usize;
    let records = pack + 1024;
    // where the entry is, then its checksum and its length
    for field in [52..60, 68..76] {
        let other = bytes[records + 88..][field.clone()].to_vec();
        bytes[records..][field].copy_from_slice(&other);
    }
    let sum = alt_hash::blake3::hash(&bytes[pack..end]);
    bytes[end + 24..end + 56].copy_from_slice(sum.as_bytes());
}

/// A live segment of `bytes` behind those the repository has.
pub(super) fn planted(f: &Fixture, id: &str, sequence: i64, bytes: &[u8]) {
    std::fs::write(f.store.pack_path("repository", id).unwrap(), bytes).unwrap();
    f.client()
        .execute(
            "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state) VALUES('repository',$1,$2,2,$3,0,'live')",
            &[&id, &sequence, &(bytes.len() as i64)],
        )
        .unwrap();
}

pub(super) fn states(f: &Fixture) -> Vec<(String, String)> {
    f.client()
        .query(
            "SELECT segment_id,state FROM alt_pack_segments ORDER BY sequence,state,segment_id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_segment_that_does_not_give_its_objects_back_is_told_of_and_indexed_anew() {
    let mut f = Fixture::new();
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
    let found = f
        .store
        .scrub_packs(&f.credential, "repository", None, &mut |_, _, _| ());
    let found = found.unwrap();
    assert_eq!((found.segments, found.objects), (1, 2));
    assert!(found.unread.is_empty());

    // the same pack once more, its index leading the read of one object to the other
    let astray = "a".repeat(32);
    let mut bytes = written.clone();
    turned(&mut bytes);
    planted(&f, &astray, sequence + 1, &bytes);
    let mut told = Vec::new();
    let mut each = |repository: &str, read, all| told.push((repository.to_owned(), read, all));
    let found = f
        .store
        .scrub_packs(&f.credential, "repository", None, &mut each);
    let found = found.unwrap();
    assert_eq!((found.segments, found.objects), (2, 4));
    // told of as each object was read, the one that is not read among them
    let each_object = (1..=4).map(|read| ("repository".to_owned(), read, 4));
    assert_eq!(told, each_object.collect::<Vec<_>>());
    let mut told = Vec::new();
    let mut each = |repository: &str, read, all| told.push((repository.to_owned(), read, all));
    assert_eq!(f.store.unread_packs(&mut each).unwrap().len(), 1);
    assert_eq!(told.last(), Some(&("repository".to_owned(), 4, 4)));
    assert_eq!(told.len(), 4);
    assert_eq!(found.unread.len(), 1);
    let unread = &found.unread[0];
    assert_eq!(
        (unread.segment.as_str(), unread.objects),
        (astray.as_str(), 1)
    );
    assert_eq!(
        unread.named[0].1,
        "segment corrupt: object differs from its id"
    );
    assert!(unread.replaced_by.is_none() && unread.kept_because.is_none());
    let live = "live".to_owned();
    assert_eq!(
        states(&f),
        [
            (first.clone(), live.clone()),
            (astray.clone(), live.clone())
        ]
    );

    // a name that is no repository has no segments, and is not told of as sound
    let none = f
        .store
        .scrub_packs(&f.credential, "repositry", None, &mut |_, _, _| ());
    let none = none.err().map(|error| error.to_string());
    let no_repository = format!("precondition failed: {}", alt_server_store::NO_REPOSITORY);
    assert_eq!(none, Some(no_repository));

    // in less memory than its pack is indexed in it stays, and that is why
    let little = Box::leak(Box::new(alt_pack_segment::Memory::new(1 << 10)));
    let kept = f
        .store
        .scrub_packs(&f.credential, "repository", Some(little), &mut |_, _, _| ());
    let kept = kept.unwrap();
    assert!(kept.unread[0].replaced_by.is_none());
    assert_eq!(
        kept.unread[0].kept_because.as_deref(),
        Some("storage limit exceeded: memory to index the pack of the segment anew")
    );

    // asked to, the server puts the pack indexed anew in its place
    let mended = f.store.scrub_packs(
        &f.credential,
        "repository",
        Some(memory()),
        &mut |_, _, _| (),
    );
    let mended = mended.unwrap();
    assert_eq!(mended.unread.len(), 1);
    let anew = mended.unread[0].replaced_by.clone().unwrap();
    assert_eq!(
        states(&f),
        [
            (first.clone(), live.clone()),
            (anew.clone(), live.clone()),
            (astray.clone(), "retired".to_owned())
        ]
    );
    let indexed = std::fs::read(f.store.pack_path("repository", &anew).unwrap()).unwrap();
    assert_eq!(indexed.len(), written.len());
    let found = f.store.scrub_packs(
        &f.credential,
        "repository",
        Some(memory()),
        &mut |_, _, _| (),
    );
    assert!(found.unwrap().unread.is_empty());

    // a pack that is no longer what was taken in cannot be: it stays, and is told of
    let changed = "c".repeat(32);
    let mut bytes = written.clone();
    let end = bytes.len() - 64;
    let pack = u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) as usize;
    bytes[pack - 21] ^= 1;
    planted(&f, &changed, sequence + 2, &bytes);
    let found = f.store.scrub_packs(
        &f.credential,
        "repository",
        Some(memory()),
        &mut |_, _, _| (),
    );
    let found = found.unwrap();
    assert_eq!(found.unread.len(), 1);
    let unread = &found.unread[0];
    assert_eq!(
        (unread.segment.as_str(), unread.objects),
        (changed.as_str(), 1)
    );
    assert_eq!(unread.named[0].1, "segment corrupt: entry checksum");
    assert!(unread.replaced_by.is_none());
    assert_eq!(
        unread.kept_because.as_deref(),
        Some("integrity failure: pack of the segment does not verify")
    );
    assert_eq!(states(&f).last().unwrap(), &(changed, live));
    assert_eq!(
        f.store.repositories_to_scrub(&f.credential).unwrap(),
        ["repository"]
    );
}

/// A segment that does not give its objects back is indexed anew while a merge
/// takes the segment before it into one of its own: the live segments no
/// longer begin with those it was indexed behind, and it takes no place. It
/// stays, and is told of; read again, it is put in its place.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_segment_indexed_anew_takes_no_place_where_the_segments_before_it_were_merged() {
    use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering::SeqCst};
    let mut f = Fixture::new();
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

    // once the segment indexed anew is written, the first is merged into one of its own
    let merged = "m".repeat(32);
    let this = f.root.path().file_name().unwrap().to_owned();
    let mut dsn: alt_pg::Config = f.admin_dsn.parse().unwrap();
    dsn.set_dbname(&f.database);
    let once = AtomicBool::new(false);
    let (first_merged, merged_id) = (first.clone(), merged.clone());
    let merged_path = f.store.pack_path("repository", &merged).unwrap();
    let written_merged = written.clone();
    *alt_server_store::MERGE_WRITTEN.lock().unwrap() = Some(Arc::new(move |path| {
        if !path.iter().any(|part| part == this) || once.swap(true, SeqCst) {
            return;
        }
        std::fs::write(&merged_path, &written_merged).unwrap();
        let mut client = alt_pg::Client::connect_with(&dsn).unwrap();
        client
            .execute(
                "UPDATE alt_pack_segments SET state='retired',retired_at=0 WHERE segment_id=$1",
                &[&first_merged],
            )
            .unwrap();
        client
            .execute(
                "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state) SELECT repository,$2,sequence,objects,bytes,flags,'live' FROM alt_pack_segments WHERE segment_id=$1",
                &[&first_merged, &merged_id],
            )
            .unwrap();
    }));
    let kept = f.store.scrub_packs(
        &f.credential,
        "repository",
        Some(memory()),
        &mut |_, _, _| (),
    );
    *alt_server_store::MERGE_WRITTEN.lock().unwrap() = None;
    let kept = kept.unwrap();
    assert_eq!(kept.unread.len(), 1);
    assert_eq!(kept.unread[0].replaced_by, None);
    assert_eq!(
        kept.unread[0].kept_because.as_deref(),
        Some("precondition failed: pack segment is no longer live")
    );
    let live: Vec<_> = (states(&f).into_iter())
        .filter(|(_, state)| state == "live")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(live, [merged.clone(), astray.clone()]);

    let mended = f.store.scrub_packs(
        &f.credential,
        "repository",
        Some(memory()),
        &mut |_, _, _| (),
    );
    let anew = mended.unwrap().unread[0].replaced_by.clone().unwrap();
    let live: Vec<_> = (states(&f).into_iter())
        .filter(|(_, state)| state == "live")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(live, [merged, anew]);
}

/// The live segments read besides those read already, as a backup reads those
/// published after it read the rest: a segment read already is not read
/// again, and is still read behind for those after it.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_segments_read_besides_those_read_already_are_those_published_since() {
    let mut f = Fixture::new();
    Payload::new("first", None).publish_new(&mut f.store, &f.credential);
    let first: String = f
        .client()
        .query_one("SELECT segment_id FROM alt_pack_segments", &[])
        .unwrap()
        .get(0);
    let mut none = |_: &str, _, _| ();
    let (unread, done) = f
        .store
        .unread_packs_besides(&Default::default(), &mut none)
        .unwrap();
    assert!(unread.is_empty());
    let known = ("repository".to_owned(), first.clone());
    assert_eq!(done, [known.clone()].into());
    // the same pack once more, its index leading the read of one object to the other
    let row = f
        .client()
        .query_one("SELECT sequence,inline FROM alt_pack_segments", &[])
        .unwrap();
    let written: Vec<u8> = (row.get::<_, Option<Vec<u8>>>(1)).unwrap_or_else(|| {
        std::fs::read(f.store.pack_path("repository", &first).unwrap()).unwrap()
    });
    let mut bytes = written;
    turned(&mut bytes);
    let astray = "a".repeat(32);
    planted(&f, &astray, row.get::<_, i64>(0) + 1, &bytes);
    let (unread, now) = f.store.unread_packs_besides(&done, &mut none).unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].1.segment, astray);
    let astray = ("repository".to_owned(), astray);
    assert_eq!(now, [known, astray.clone()].into());
    let (unread, _) = f.store.unread_packs_besides(&now, &mut none).unwrap();
    assert!(unread.is_empty(), "those read already are read again");
    let besides = [astray].into();
    let found = f.store.unread_packs_besides(&besides, &mut none).unwrap();
    assert!(found.0.is_empty(), "the one read already is read again");
}

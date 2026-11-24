//! Reading every object of a repository's live segments, and replacing a segment
//! that does not give its objects back by its pack indexed anew.
use super::*;

/// Has the first record of the bytes of a segment tell of the entry of the second.
fn turned(bytes: &mut [u8]) {
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
fn planted(f: &Fixture, id: &str, sequence: i64, bytes: &[u8]) {
    std::fs::write(f.store.pack_path("repository", id).unwrap(), bytes).unwrap();
    f.client()
        .execute(
            "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state) VALUES('repository',$1,$2,2,$3,0,'live')",
            &[&id, &sequence, &(bytes.len() as i64)],
        )
        .unwrap();
}

fn states(f: &Fixture) -> Vec<(String, String)> {
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
    let found = f.store.scrub_packs(&f.credential, "repository", false);
    let found = found.unwrap();
    assert_eq!((found.segments, found.objects), (1, 2));
    assert!(found.unread.is_empty());

    // the same pack once more, its index leading the read of one object to the other
    let astray = "a".repeat(32);
    let mut bytes = written.clone();
    turned(&mut bytes);
    planted(&f, &astray, sequence + 1, &bytes);
    let found = f.store.scrub_packs(&f.credential, "repository", false);
    let found = found.unwrap();
    assert_eq!((found.segments, found.objects), (2, 4));
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

    // asked to, the server puts the pack indexed anew in its place
    let mended = f.store.scrub_packs(&f.credential, "repository", true);
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
    let found = f.store.scrub_packs(&f.credential, "repository", true);
    assert!(found.unwrap().unread.is_empty());

    // a pack that is no longer what was taken in cannot be: it stays, and is told of
    let changed = "c".repeat(32);
    let mut bytes = written.clone();
    let end = bytes.len() - 64;
    let pack = u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) as usize;
    bytes[pack - 21] ^= 1;
    planted(&f, &changed, sequence + 2, &bytes);
    let found = f.store.scrub_packs(&f.credential, "repository", true);
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

//! Abbreviated ids against a store whose map is read through index runs.

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("abbrev object {n}\n").into_bytes();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
    (oid, data)
}

fn shared_digits(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

#[test]
fn an_id_gets_one_digit_more_than_it_shares_with_another() {
    // two objects whose ids agree in their first seven digits or more
    let mut ids: Vec<(String, usize)> = (0..80_000).map(|n| (object(n).0.to_string(), n)).collect();
    ids.sort();
    let pair = ids
        .windows(2)
        .find(|w| w[0].0[..7] == w[1].0[..7])
        .expect("80000 ids hold a pair sharing seven digits");
    let (near_a, near_b) = (pair[0].1, pair[1].1);
    let shared = shared_digits(&pair[0].0, &pair[1].0);

    let dir = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(dir.path()).unwrap();
    // enough for index runs, with one of the pair left in the unindexed tail
    for n in (0..600).chain([near_a]) {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
    let (oid, data) = object(near_b);
    odb.put(oid, ObjectKind::Blob, &data).unwrap();
    odb.flush().unwrap();
    drop(odb);

    let odb = NativeOdb::deferred(dir.path());
    let abbrev = odb.abbrev().unwrap();
    assert_eq!(abbrev.min_len(), 7);
    for n in [near_a, near_b] {
        let hex = object(n).0.to_string();
        assert_eq!(abbrev.of(&object(n).0).unwrap(), hex[..shared + 1]);
    }
    // every other id stays apart from the rest at the default length
    let all: Vec<String> = odb
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.git.to_string())
        .collect();
    for hex in all.iter().filter(|h| **h != pair[0].0 && **h != pair[1].0) {
        let most = all
            .iter()
            .filter(|o| *o != hex)
            .map(|o| shared_digits(o, hex))
            .max()
            .unwrap();
        let oid = ObjectId::from_hex(hex.as_bytes()).unwrap();
        assert_eq!(abbrev.of(&oid).unwrap(), hex[..(most + 1).max(7)]);
    }
    // an id that is not stored is kept apart from those that are
    let (absent, _) = object(80_001);
    assert_eq!(abbrev.of(&absent).unwrap().len(), 7);
}

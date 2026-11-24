//! Pushes whose packs were verified before the segments under them moved.
use super::*;
use alt_pack_segment::Segment;

fn id(kind: ObjectKind, data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, kind, data)
}

fn entry(kind: u8, before: &[u8], data: &[u8]) -> Vec<u8> {
    let mut size = data.len();
    let mut entry = vec![(kind << 4) | (size as u8 & 15)];
    size >>= 4;
    while size > 0 {
        *entry.last_mut().unwrap() |= 128;
        entry.push(size as u8 & 127);
        size >>= 7;
    }
    entry.extend_from_slice(before);
    entry.extend(alt_deflate::compress(data, 6, alt_deflate::Format::Zlib));
    entry
}

/// The entry of `target` as a delta of the object `base`, which has
/// `base_bytes`: nothing of the base is copied.
fn of(base: &ObjectId, base_bytes: usize, target: &[u8]) -> Vec<u8> {
    let mut delta = Vec::new();
    for mut size in [base_bytes, target.len()] {
        while size >= 128 {
            delta.push(size as u8 | 128);
            size >>= 7;
        }
        delta.push(size as u8);
    }
    for piece in target.chunks(127) {
        delta.push(piece.len() as u8);
        delta.extend_from_slice(piece);
    }
    entry(7, base.as_bytes(), &delta)
}

fn commit(tree: &ObjectId, message: &str) -> Vec<u8> {
    let who = "A <a@example.test> 1 +0000";
    format!("tree {tree}\nauthor {who}\ncommitter {who}\n\n{message}\n").into_bytes()
}

/// The body of a push that creates the branch `name` at a commit of `tree`,
/// with that commit and `entries` as its pack.
fn creating(name: &str, tree: &ObjectId, entries: &[Vec<u8>]) -> Vec<u8> {
    let commit = commit(tree, name);
    let count = entries.len() as u32 + 1;
    let mut pack = [&b"PACK"[..], &2u32.to_be_bytes(), &count.to_be_bytes()].concat();
    pack.extend(entry(1, b"", &commit));
    entries
        .iter()
        .for_each(|entry| pack.extend_from_slice(entry));
    let sum = alt_hash::sha1(&pack);
    pack.extend_from_slice(&sum);
    let (none, new) = ("0".repeat(40), id(ObjectKind::Commit, &commit));
    let mut body = Vec::new();
    let line = format!("{none} {new} refs/heads/{name}\0report-status\n");
    alt_wire::pkt::write_data(&mut body, line.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&pack);
    body
}

/// What holds a publication at the first row its transaction locks that a
/// request does not come to before: its pack is received and verified by then.
const HELD: &str = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";

/// The live segments of the repository, oldest first.
fn live(f: &Fixture) -> Vec<Segment> {
    let rows = f.client().query(
        "SELECT segment_id FROM alt_pack_segments WHERE repository='repository' AND state='live' ORDER BY sequence,segment_id",
        &[],
    );
    let open = |row: &alt_pg::Row| {
        let path = f
            .store
            .pack_path("repository", &row.get::<_, String>(0))
            .unwrap();
        Segment::open(&path).unwrap()
    };
    rows.unwrap().iter().map(open).collect()
}

/// Two pushes are verified behind the same segments and published one behind
/// the other, and a merge of the segments under them is published among them.
/// One brings an object made of another that two older segments hold, stored
/// in the newer and four deltas deep in the older, which is the one a merge
/// keeps; both bring a file neither knew the other had. However they fall,
/// the depth each record tells is the depth it is read at, and a clone that
/// checks what it is sent takes the repository.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn pushes_verified_before_the_segments_under_them_moved_are_published_as_they_then_stand() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let url = format!("{}/git/repository", server.url);
    let pushed = |body: &[u8]| {
        let (status, report) = raw_push::receive(&url, &f.token, body);
        assert_eq!(status, 200, "{report}");
        assert!(
            report.contains("unpack ok") && !report.contains("ng "),
            "{report}"
        );
    };
    let none = id(ObjectKind::Tree, b"");
    // a segment far larger than the ones behind it, which no merge of them takes in
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let large: Vec<u8> = (0..60_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    pushed(&creating(
        "s0",
        &none,
        &[entry(2, b"", b""), entry(3, b"", &large)],
    ));
    // the object, at the end of four deltas and then stored
    let step = |n: usize| format!("what the object was at step {n}\n").into_bytes();
    let mut chain = vec![of(&id(ObjectKind::Blob, &large), large.len(), &step(0))];
    for n in 1..4 {
        let base = step(n - 1);
        chain.push(of(&id(ObjectKind::Blob, &base), base.len(), &step(n)));
    }
    let (x, x_id) = (step(3), id(ObjectKind::Blob, &step(3)));
    pushed(&creating("s1", &none, &chain));
    pushed(&creating("s2", &none, &[entry(3, b"", &x)]));
    for n in 3..8 {
        pushed(&creating(&format!("s{n}"), &none, &[]));
    }
    assert_eq!(live(&f).len(), 8);

    // the two pushes, each held where its transaction asks for the repository
    let file = b"a file both pushes bring\n";
    let tree = [&b"100644 file\0"[..], id(ObjectKind::Blob, file).as_bytes()].concat();
    let shared = [entry(2, b"", &tree), entry(3, b"", file)];
    let z = b"what is made of the object\n";
    let with_delta = [&shared[..], &[of(&x_id, x.len(), z)]].concat();
    let tree = id(ObjectKind::Tree, &tree);
    let bodies = [
        creating("p", &tree, &shared),
        creating("t", &tree, &with_delta),
    ];
    let mut held = f.client();
    let mut holding = held.transaction().unwrap();
    holding.query_one(HELD, &[]).unwrap();
    let waiting = |f: &Fixture| -> i64 {
        let asked = "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'";
        f.client().query_one(asked, &[]).unwrap().get(0)
    };
    std::thread::scope(|scope| {
        for (n, body) in bodies.iter().enumerate() {
            scope.spawn(|| pushed(body));
            let until = std::time::Instant::now() + Duration::from_secs(60);
            while waiting(&f) <= n as i64 {
                assert!(std::time::Instant::now() < until, "push {n} does not wait");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        holding.rollback().unwrap();
    });
    drop(server);
    while f.store.compact_packs("repository").unwrap() {}

    let segments = live(&f);
    let raw = |oid: &ObjectId| -> [u8; 20] { oid.as_bytes().try_into().unwrap() };
    let z_id = raw(&id(ObjectKind::Blob, z));
    let (at, index) = (segments.iter().enumerate())
        .find_map(|(at, segment)| Some((at, segment.find(&z_id)?)))
        .unwrap();
    let told = segments[at].record(index).unwrap();
    let under = match told.delta {
        alt_pack_segment::Delta::Inside => segments[at].record(told.base as usize).unwrap(),
        _ => {
            let below = segments[..at].iter().rev();
            let found = below.map(|segment| Some(segment.record(segment.find(&raw(&x_id))?)));
            found.flatten().next().unwrap().unwrap()
        }
    };
    assert_eq!(
        told.depth,
        under.depth + 1,
        "of {} live segments",
        segments.len()
    );
    let scrubbed = f.store.scrub_packs(&f.credential, "repository", None);
    assert_eq!(scrubbed.unwrap().unread.len(), 0);

    let server = Running::start(&f);
    let url = format!("{}/git/repository", server.url);
    let temp = alt_os::temp::tempdir().unwrap();
    let checked = [
        "-c",
        "transfer.fsckObjects=true",
        "clone",
        "-q",
        "--branch",
        "t",
    ];
    ok(
        temp.path(),
        &f.token,
        &[&checked[..], &[&url, "all"]].concat(),
    );
    let all = temp.path().join("all");
    ok(&all, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(std::fs::read(all.join("file")).unwrap(), file);
}

/// The file of a segment is gone when its publication comes to its
/// transaction, as it would be had a sweep taken it for one nobody writes.
/// Nothing is published: a row without its file would be a repository that
/// cannot be read. The push is told that it failed, and the next one is taken.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_segment_whose_file_is_gone_is_not_published() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let url = format!("{}/git/repository", server.url);
    let none = id(ObjectKind::Tree, b"");
    // more than is kept with the row of a segment: the file is all there is of it
    let large: Vec<u8> = (0..200_000u32)
        .flat_map(|n| (n.wrapping_mul(2654435761)).to_le_bytes())
        .collect();
    let body = creating("gone", &none, &[entry(2, b"", b""), entry(3, b"", &large)]);
    let mut held = f.client();
    let mut holding = held.transaction().unwrap();
    holding.query_one(HELD, &[]).unwrap();
    let report = std::thread::scope(|scope| {
        let pushing = scope.spawn(|| raw_push::receive(&url, &f.token, &body));
        let asked = "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'";
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while f.client().query_one(asked, &[]).unwrap().get::<_, i64>(0) == 0 {
            assert!(std::time::Instant::now() < until, "the push does not wait");
            std::thread::sleep(Duration::from_millis(20));
        }
        let directory = f.store.pack_directory("repository").unwrap();
        let files: Vec<_> = std::fs::read_dir(directory).unwrap().collect();
        assert_eq!(files.len(), 1);
        std::fs::remove_file(files[0].as_ref().unwrap().path()).unwrap();
        holding.rollback().unwrap();
        pushing.join().unwrap()
    });
    assert!(report.1.contains("ng refs/heads/gone"), "{report:?}");
    assert!(live(&f).is_empty());
    let (status, report) = raw_push::receive(&url, &f.token, &body);
    assert!(
        status == 200 && report.contains("ok refs/heads/gone"),
        "{report}"
    );
    assert_eq!(live(&f).len(), 1);
}

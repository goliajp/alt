//! `index_pack` over packs as a client that is no Git could send them:
//! deltas named by the id of a base that comes later in the pack.

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_pack::{IndexLimits, IndexedPack, PackError, index_pack, index_pack_within};
use alt_os::temp::TempDir;

use crate::ingress::{entry_header, finish, ofs, pack_header, zlib};

pub(crate) fn blob(data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, data)
}

fn size(out: &mut Vec<u8>, mut value: usize) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

/// A delta that makes `target` of a base of `base` bytes: the first
/// `keep` bytes of the base, then the rest of the target as it is.
pub(crate) fn delta(base: usize, keep: usize, target: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    size(&mut out, base);
    size(&mut out, target.len());
    if keep > 0 {
        out.extend([0x80 | 0x10 | 0x20, keep as u8, (keep >> 8) as u8]);
    }
    for chunk in target[keep..].chunks(127) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out
}

pub(crate) fn plain_entry(data: &[u8]) -> Vec<u8> {
    let mut entry = entry_header(3, data.len() as u64);
    entry.extend(zlib(data));
    entry
}

pub(crate) fn ref_entry(base: &ObjectId, delta: &[u8]) -> Vec<u8> {
    let mut entry = entry_header(7, delta.len() as u64);
    entry.extend_from_slice(base.as_bytes());
    entry.extend(zlib(delta));
    entry
}

/// A delta of the entry `distance` bytes before the one this becomes.
pub(crate) fn ofs_entry(distance: usize, delta: &[u8]) -> Vec<u8> {
    let mut entry = entry_header(6, delta.len() as u64);
    entry.extend(ofs(distance));
    entry.extend(zlib(delta));
    entry
}

pub(crate) fn written(dir: &TempDir, entries: &[Vec<u8>]) -> std::path::PathBuf {
    let mut pack = pack_header(entries.len() as u32);
    for entry in entries {
        pack.extend_from_slice(entry);
    }
    let path = dir.path().join("incoming.pack");
    std::fs::write(&path, finish(pack)).unwrap();
    path
}

fn limits(base_bytes: usize) -> IndexLimits {
    IndexLimits {
        base_bytes,
        ..IndexLimits::default()
    }
}

pub(crate) fn object(n: usize) -> Vec<u8> {
    format!("the object number {n} of a chain\n").into_bytes()
}

/// Object 0 plain and last, each other object a delta of the one before
/// it by id, the deepest delta first: no entry's base is known before
/// every entry behind it is resolved.
fn chain_backwards(count: usize) -> Vec<Vec<u8>> {
    let mut entries: Vec<Vec<u8>> = (1..count)
        .rev()
        .map(|n| {
            let base = object(n - 1);
            ref_entry(&blob(&base), &delta(base.len(), 18, &object(n)))
        })
        .collect();
    entries.push(plain_entry(&object(0)));
    entries
}

fn reads_back(pack: &std::path::Path, objects: impl Iterator<Item = Vec<u8>>) {
    let indexed = IndexedPack::open(pack, HashAlgo::Sha1).unwrap();
    for data in objects {
        let read = indexed.read(&blob(&data)).unwrap().expect("in the pack");
        assert_eq!(*read.data, data);
    }
}

#[test]
fn deltas_named_before_their_bases_are_resolved_in_work_linear_in_their_number() {
    for count in [64, 128, 256] {
        let dir = TempDir::new().unwrap();
        let pack = written(&dir, &chain_backwards(count));
        let indexed = index_pack(&pack, HashAlgo::Sha1, false).unwrap();
        assert_eq!(indexed.objects as usize, count);
        // every object is hashed once, and every delta applied once
        assert_eq!(indexed.work.hashed, count as u64, "{count} objects");
        assert_eq!(indexed.work.applied, count as u64 - 1, "{count} objects");
        reads_back(&pack, (0..count).map(object));
    }
}

#[test]
fn a_delta_whose_base_the_pack_lacks_is_refused() {
    let dir = TempDir::new().unwrap();
    let absent = object(7);
    let entries = [
        plain_entry(&object(0)),
        ref_entry(&blob(&absent), &delta(absent.len(), 18, &object(8))),
    ];
    let refused = index_pack(&written(&dir, &entries), HashAlgo::Sha1, false);
    assert!(matches!(refused, Err(PackError::Format(_))), "{refused:?}");
}

/// The index `git index-pack` writes for the pack at `pack`.
pub(crate) fn by_git(pack: &std::path::Path) -> Vec<u8> {
    let dir = TempDir::new().unwrap();
    let copy = dir.path().join("copy.pack");
    std::fs::copy(pack, &copy).unwrap();
    let out = std::process::Command::new("git")
        .arg("index-pack")
        .arg(&copy)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(copy.with_extension("idx")).unwrap()
}

#[test]
fn the_index_of_deltas_before_their_bases_is_the_one_git_writes() {
    let dir = TempDir::new().unwrap();
    let pack = written(&dir, &chain_backwards(40));
    let indexed = index_pack(&pack, HashAlgo::Sha1, false).unwrap();
    assert_eq!(std::fs::read(&indexed.idx_path).unwrap(), by_git(&pack));
}

pub(crate) fn wide(n: usize) -> Vec<u8> {
    let mut data = format!("{n:04} ").into_bytes();
    data.resize(1000, b'a' + (n % 26) as u8);
    data
}

/// A comb: a chain of deltas by offset, each of whose objects has a
/// second delta, by id, that comes after the whole chain. Coming back
/// from the end of the chain, every object on it is needed once more.
fn comb(depth: usize) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut entries = vec![plain_entry(&wide(0))];
    let mut objects = vec![wide(0)];
    for n in 1..depth {
        let last = entries.last().unwrap().len();
        entries.push(ofs_entry(last, &delta(1000, 0, &wide(n))));
        objects.push(wide(n));
    }
    for n in 0..depth {
        let side = wide(depth + n);
        entries.push(ref_entry(&blob(&wide(n)), &delta(1000, 0, &side)));
        objects.push(side);
    }
    (entries, objects)
}

/// The comb with its teeth the other way: the chain is by id, and the
/// second delta of each of its objects is by offset. Which of two
/// deltas by id has more under it is not known before both are
/// resolved, so here the objects of the chain do have to wait.
fn comb_by_id(depth: usize) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut entries = vec![plain_entry(&wide(0))];
    let mut objects = vec![wide(0)];
    for n in 1..depth {
        entries.push(ref_entry(&blob(&wide(n - 1)), &delta(1000, 0, &wide(n))));
        objects.push(wide(n));
    }
    for n in 0..depth {
        let side = wide(depth + n);
        let distance = entries[n..].iter().map(Vec::len).sum();
        entries.push(ofs_entry(distance, &delta(1000, 0, &side)));
        objects.push(side);
    }
    (entries, objects)
}

/// However little room the bases have, a chain whose objects each have
/// one more delta takes one application for each delta: the object of a
/// step is let go of when the chain goes on from it, and by then its
/// other delta is resolved.
#[test]
fn a_chain_with_a_delta_beside_each_step_is_resolved_in_work_linear_in_its_depth() {
    for depth in [64, 128, 256] {
        let (entries, objects) = comb(depth);
        let dir = TempDir::new().unwrap();
        let pack = written(&dir, &entries);
        let all = index_pack(&pack, HashAlgo::Sha1, false).unwrap();
        let whole = std::fs::read(&all.idx_path).unwrap();
        assert_eq!(whole, by_git(&pack));
        let few = index_pack_within(&pack, HashAlgo::Sha1, false, &limits(4000)).unwrap();
        assert_eq!(std::fs::read(&few.idx_path).unwrap(), whole);
        for work in [all.work, few.work] {
            assert_eq!(work.hashed, 2 * depth as u64, "{work:?}");
            assert_eq!(work.applied, 2 * depth as u64 - 1, "{work:?}");
            // the object of a step and the one made of it
            assert!(work.held <= 2000, "{work:?}");
        }
        reads_back(&pack, objects.into_iter());
    }
}

#[test]
fn bases_that_have_to_wait_are_held_within_their_limit_and_made_again_at_the_cost_of_the_pack() {
    const DEPTH: usize = 200;
    let (entries, objects) = comb_by_id(DEPTH);
    let dir = TempDir::new().unwrap();
    let pack = written(&dir, &entries);
    let all = index_pack(&pack, HashAlgo::Sha1, false).unwrap();
    let whole = std::fs::read(&all.idx_path).unwrap();
    assert_eq!(whole, by_git(&pack));
    // every base fits: nothing is made twice
    assert_eq!(all.work.hashed, 2 * DEPTH as u64);
    assert_eq!(all.work.applied, 2 * DEPTH as u64 - 1);
    assert_eq!(all.work.held, 1000 * DEPTH);

    let few = index_pack_within(&pack, HashAlgo::Sha1, false, &limits(4000)).unwrap();
    assert_eq!(std::fs::read(&few.idx_path).unwrap(), whole);
    assert_eq!(few.work.hashed, 2 * DEPTH as u64);
    // room for four bases, and for the one more that is being added
    assert!(few.work.held <= 5000, "{:?}", few.work);
    // a base that was let go of is made again from the one held below
    // it, or from the pack
    let again = few.work.applied - all.work.applied;
    assert!(again > 0, "{:?}", few.work);
    reads_back(&pack, objects.into_iter());

    // what is made again is counted: with room for what the pack
    // inflates to and for half of that again, it is refused
    let inflated: usize = entries
        .iter()
        .skip(1)
        .map(|_| delta(1000, 0, &wide(1)).len())
        .sum();
    let inflated = (inflated + 2 * DEPTH * 1000) as u64;
    let exact = IndexLimits {
        inflated_bytes: inflated,
        ..IndexLimits::default()
    };
    assert!(index_pack_within(&pack, HashAlgo::Sha1, false, &exact).is_ok());
    for (times, fits) in [(1000 * (again + 1), true), (1000 * again / 2, false)] {
        let counted = IndexLimits {
            inflated_bytes: inflated + times,
            ..limits(4000)
        };
        let indexed = index_pack_within(&pack, HashAlgo::Sha1, false, &counted);
        match fits {
            true => assert!(indexed.is_ok(), "{indexed:?}"),
            false => assert!(
                matches!(indexed, Err(PackError::Limit("inflated bytes"))),
                "{indexed:?}"
            ),
        }
    }
}

#[test]
fn a_chain_deeper_than_git_makes_them_is_refused() {
    let depth = alt_git_pack::MAX_DELTA_DEPTH as usize;
    for (deltas, fits) in [(depth, true), (depth + 1, false)] {
        let dir = TempDir::new().unwrap();
        let mut entries = vec![plain_entry(&object(0))];
        for n in 1..=deltas {
            let last = entries.last().unwrap().len();
            let base = object(n - 1).len();
            entries.push(ofs_entry(last, &delta(base, 18, &object(n))));
        }
        let indexed = index_pack(&written(&dir, &entries), HashAlgo::Sha1, false);
        match fits {
            true => assert_eq!(indexed.unwrap().work.applied, deltas as u64),
            false => assert!(
                matches!(indexed, Err(PackError::Limit("delta depth"))),
                "{indexed:?}"
            ),
        }
    }
}

/// Sizes and counts as the sender wrote them: no room is made for what
/// the bytes of the pack cannot hold.
#[test]
fn an_entry_size_or_a_count_beyond_what_the_pack_holds_makes_no_room() {
    let dir = TempDir::new().unwrap();
    for (size, why) in [
        (1 << 50, "entry size beyond what its stream holds"),
        (u64::MAX, "entry size too long"),
    ] {
        let mut huge = entry_header(3, size);
        huge.extend(zlib(b"small"));
        let refused = index_pack(&written(&dir, &[huge]), HashAlgo::Sha1, false);
        assert!(
            matches!(refused, Err(PackError::Format(said)) if said == why),
            "{refused:?}"
        );
    }

    let mut pack = pack_header(u32::MAX);
    pack.extend(plain_entry(b"one"));
    let path = dir.path().join("count.pack");
    std::fs::write(&path, finish(pack)).unwrap();
    let refused = index_pack(&path, HashAlgo::Sha1, false);
    assert!(
        matches!(
            refused,
            Err(PackError::Format("pack header count exceeds body"))
        ),
        "{refused:?}"
    );
}

#[test]
fn objects_beyond_the_limits_given_are_refused() {
    let dir = TempDir::new().unwrap();
    // a delta that makes sixteen copies of its base of a thousand bytes
    let mut grows = Vec::new();
    size(&mut grows, 1000);
    size(&mut grows, 16_000);
    for _ in 0..16 {
        grows.extend([0x80 | 0x10 | 0x20, 0xe8, 0x03]);
    }
    let entries = [plain_entry(&wide(0)), ref_entry(&blob(&wide(0)), &grows)];
    let pack = written(&dir, &entries);
    let within = |object_bytes, inflated_bytes| {
        let limits = IndexLimits {
            object_bytes,
            inflated_bytes,
            ..IndexLimits::default()
        };
        index_pack_within(&pack, HashAlgo::Sha1, false, &limits).map(|indexed| indexed.objects)
    };
    // the entries inflate to the base and the delta, the objects to
    // the base and what the delta makes of it
    let entries = 1000 + grows.len() as u64;
    assert_eq!(within(16_000, entries + 16_000).unwrap(), 2);
    let refused = within(15_999, u64::MAX);
    assert!(matches!(refused, Err(PackError::Limit(_))), "{refused:?}");
    let refused = within(u64::MAX, entries + 15_999);
    assert!(
        matches!(refused, Err(PackError::Limit("inflated bytes"))),
        "{refused:?}"
    );
}

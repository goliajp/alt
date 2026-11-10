use super::{entry, resign};
use alt_catalog_block::*;

fn mapping(n: u64, sha256: bool) -> GitIndexEntry {
    let mut raw = [0; 32];
    raw[..8].copy_from_slice(&(n + 1).to_be_bytes());
    GitIndexEntry {
        oid: if sha256 {
            ObjectId::Sha256(raw)
        } else {
            ObjectId::Sha1(raw[..20].try_into().unwrap())
        },
        kind: GitKind::Blob,
        digest: entry(n).digest,
    }
}
fn unbounded<K>() -> KeyRange<K> {
    KeyRange {
        lower: None,
        upper: None,
    }
}

#[test]
fn secondary_index_sorts_algorithms_and_rejects_conflicts_across_pages() {
    let input: Vec<_> = (0..100)
        .flat_map(|n| [mapping(n, true), mapping(n, false)])
        .collect();
    let limits = Limits {
        members: 13,
        bytes: 450,
    };
    let pages = paginate_git(input.clone(), limits).unwrap();
    let mut sorted = input.clone();
    sorted.sort_by_key(|e| e.oid);
    assert_eq!(
        pages
            .iter()
            .flat_map(|b| b.entries())
            .copied()
            .collect::<Vec<_>>(),
        sorted
    );
    let keys: Vec<_> = sorted.iter().map(|e| e.oid.key()).collect();
    assert!(keys.windows(2).all(|p| p[0] < p[1]));
    for b in &pages {
        assert!(b.encoded().len() <= limits.bytes);
        assert_eq!(
            GitIndexBlock::decode(b.encoded(), b.id(), limits).unwrap(),
            *b
        );
        for e in b.entries() {
            assert_eq!(b.find(&e.oid), Some(e));
        }
        assert_eq!(b.find(&mapping(500, false).oid), None);
        assert!(b.page().unwrap().contains(&b.entries()[0].oid));
    }
    for p in pages.windows(2) {
        p[0].page()
            .unwrap()
            .precedes(&p[1].page().unwrap())
            .unwrap();
    }
    let mut duplicate = input.clone();
    duplicate.extend(input.clone());
    assert_eq!(paginate_git(duplicate, limits).unwrap(), pages);
    for wrong_kind in [false, true] {
        let mut corrupt = input.clone();
        let mut conflict = input[0];
        if wrong_kind {
            conflict.kind = GitKind::Tree;
        } else {
            conflict.digest = [9; 32];
        }
        corrupt.push(conflict);
        assert!(paginate_git(corrupt, limits).is_err());
    }
}

#[test]
fn secondary_parser_rejects_truncation_damage_bad_extent_and_noncanonical_payloads() {
    let b = GitIndexBlock::build(
        vec![mapping(1, false), mapping(2, false)],
        Limits::default(),
    )
    .unwrap();
    for end in 0..b.encoded().len() {
        assert!(GitIndexBlock::decode(&b.encoded()[..end], b.id(), Limits::default()).is_err());
        let mut bytes = b.encoded().to_vec();
        bytes[end] ^= 1;
        assert!(GitIndexBlock::decode(&bytes, b.id(), Limits::default()).is_err());
    }
    for (offset, value) in [(0, b'X'), (8, 3), (12, 0), (16, 3), (37, 4)] {
        let mut bytes = b.encoded().to_vec();
        bytes[offset] = value;
        let id = resign(&mut bytes);
        assert!(GitIndexBlock::decode(&bytes, &id, Limits::default()).is_err());
    }
    let mut bytes = b.encoded().to_vec();
    bytes.copy_within(16..70, 70);
    let id = resign(&mut bytes);
    assert!(GitIndexBlock::decode(&bytes, &id, Limits::default()).is_err());
    let mut bytes = b.encoded().to_vec();
    bytes[17..37].fill(0);
    let id = resign(&mut bytes);
    assert!(GitIndexBlock::decode(&bytes, &id, Limits::default()).is_err());
    let empty = GitIndexBlock::build(vec![], Limits::default()).unwrap();
    assert!(empty.page().is_err());
    assert_eq!(
        GitIndexBlock::decode(empty.encoded(), empty.id(), Limits::default()).unwrap(),
        empty
    );
    assert!(paginate_git(vec![], Limits::default()).unwrap().is_empty());
    let max = GitIndexBlock::build(
        (0..MAX_MEMBERS as u64).map(|i| mapping(i, false)).collect(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        GitIndexBlock::decode(max.encoded(), max.id(), Limits::default()).unwrap(),
        max
    );
    assert!(
        GitIndexBlock::build(
            (0..=MAX_MEMBERS as u64)
                .map(|i| mapping(i, false))
                .collect(),
            Limits::default()
        )
        .is_err()
    );
    for bytes in [101, 102] {
        assert_eq!(
            paginate_git(vec![mapping(1, false)], Limits { members: 1, bytes }).is_ok(),
            bytes == 102
        );
    }
    // The SHA-256 record is 12 bytes longer.
    assert!(
        paginate_git(
            vec![mapping(1, true)],
            Limits {
                members: 1,
                bytes: 113
            }
        )
        .is_err()
    );
    assert_eq!(
        paginate_git(
            vec![mapping(1, true)],
            Limits {
                members: 1,
                bytes: 114
            }
        )
        .unwrap()[0]
            .encoded()
            .len(),
        114
    );
}

#[test]
fn local_edit_splits_without_touching_other_pages_or_old_pin() {
    let limits = Limits {
        members: 4,
        bytes: 300,
    };
    let old = paginate((0..12).map(|i| entry(i * 2)).collect(), limits).unwrap();
    let selected = MemberSet::from_indices(&old[1], [0, 2]).unwrap();
    let range = KeyRange {
        lower: Some(entry(8).digest),
        upper: Some(entry(16).digest),
    };
    let mut replacement = entry(10);
    replacement.roles |= CHUNK;
    let updated = edit_page(
        &old[1],
        old[1].id(),
        &range,
        vec![
            Edit::Insert(entry(9)),
            Edit::Insert(entry(11)),
            Edit::Replace(replacement.clone()),
        ],
        limits,
    )
    .unwrap();
    assert_eq!(
        updated
            .iter()
            .map(|b| b.entries().len())
            .collect::<Vec<_>>(),
        [4, 2]
    );
    let new: Vec<_> = std::iter::once(old[0].clone())
        .chain(updated)
        .chain(std::iter::once(old[2].clone()))
        .collect();
    assert_eq!(new[0].id(), old[0].id());
    assert_eq!(new.last().unwrap().id(), old[2].id());
    for p in new.windows(2) {
        p[0].page()
            .unwrap()
            .precedes(&p[1].page().unwrap())
            .unwrap();
    }
    assert_eq!(
        MemberSet::decode(&selected.encode(), &old[1])
            .unwrap()
            .indices()
            .map(|i| old[1].entries()[i].digest)
            .collect::<Vec<_>>(),
        [entry(8).digest, entry(12).digest]
    );
    let mut expected: Vec<_> = (0..12)
        .map(|i| {
            if i == 5 {
                replacement.clone()
            } else {
                entry(i * 2)
            }
        })
        .collect();
    expected.extend([entry(9), entry(11)]);
    expected.sort_by_key(|e| e.digest);
    assert_eq!(
        new.iter()
            .flat_map(|b| b.entries())
            .cloned()
            .collect::<Vec<_>>(),
        expected
    );
    let deleted = edit_page(
        &old[0],
        old[0].id(),
        &unbounded(),
        old[0]
            .entries()
            .iter()
            .map(|e| Edit::Remove(e.digest))
            .collect(),
        limits,
    )
    .unwrap();
    assert!(deleted.is_empty());
}

#[test]
fn local_edit_rejects_stale_roots_wrong_ranges_missing_members_and_oversize() {
    let limits = Limits {
        members: 4,
        bytes: 300,
    };
    let b = Block::build(vec![entry(10), entry(12)], limits).unwrap();
    let range = KeyRange {
        lower: Some(entry(8).digest),
        upper: Some(entry(16).digest),
    };
    assert!(edit_page(&b, &[0; 32], &range, vec![], limits).is_err());
    for edits in [
        vec![Edit::Insert(entry(10))],
        vec![Edit::Replace(entry(11))],
        vec![Edit::Remove(entry(11).digest)],
        vec![Edit::Remove(entry(10).digest), Edit::Insert(entry(10))],
        vec![Edit::Insert(entry(7))],
        vec![Edit::Insert(entry(16))],
    ] {
        assert!(edit_page(&b, b.id(), &range, edits, limits).is_err());
    }
    for range in [
        KeyRange {
            lower: Some(entry(11).digest),
            upper: None,
        },
        KeyRange {
            lower: None,
            upper: Some(entry(12).digest),
        },
        KeyRange {
            lower: Some(entry(12).digest),
            upper: Some(entry(12).digest),
        },
    ] {
        assert!(edit_page(&b, b.id(), &range, vec![], limits).is_err());
    }
    let mut huge = entry(11);
    huge.attributes = vec![0; 300];
    assert!(edit_page(&b, b.id(), &range, vec![Edit::Insert(huge)], limits).is_err());
    assert!(
        edit_page(
            &b,
            b.id(),
            &range,
            (0..5).map(|i| Edit::Insert(entry(i))).collect(),
            limits
        )
        .is_err()
    );
    // Wrong descriptors and overlap are rejected even if individual blocks decode.
    let p = b.page().unwrap();
    assert!(p.precedes(&p).is_err());
    let mut bad = p.clone();
    bad.members = 1;
    assert!(bad.validate().is_err());
    bad = p.clone();
    bad.first = entry(20).digest;
    assert!(bad.validate().is_err());
    bad = p;
    bad.members = 0;
    assert!(bad.validate().is_err());
}

#[test]
fn secondary_edits_preserve_identity_and_reject_mapping_replacement() {
    let limits = Limits {
        members: 3,
        bytes: 300,
    };
    let b = GitIndexBlock::build(
        vec![mapping(10, false), mapping(12, false), mapping(14, false)],
        limits,
    )
    .unwrap();
    let range = KeyRange {
        lower: Some(mapping(8, false).oid),
        upper: Some(mapping(16, false).oid),
    };
    let next = edit_git_page(
        &b,
        b.id(),
        &range,
        vec![GitIndexEdit::Insert(mapping(11, false))],
        limits,
    )
    .unwrap();
    assert_eq!(
        next.iter().map(|p| p.entries().len()).collect::<Vec<_>>(),
        [3, 1]
    );
    next[0]
        .page()
        .unwrap()
        .precedes(&next[1].page().unwrap())
        .unwrap();
    assert_eq!(
        next.iter()
            .flat_map(|p| p.entries())
            .copied()
            .collect::<Vec<_>>(),
        [
            mapping(10, false),
            mapping(11, false),
            mapping(12, false),
            mapping(14, false)
        ]
    );
    let mut conflict = mapping(10, false);
    conflict.digest = [99; 32];
    for edits in [
        vec![GitIndexEdit::Insert(conflict)],
        vec![GitIndexEdit::Remove(mapping(11, false).oid)],
        vec![GitIndexEdit::Insert(mapping(16, false))],
        vec![
            GitIndexEdit::Remove(mapping(10, false).oid),
            GitIndexEdit::Insert(conflict),
        ],
    ] {
        assert!(edit_git_page(&b, b.id(), &range, edits, limits).is_err());
    }
    assert!(edit_git_page(&b, &[0; 32], &range, vec![], limits).is_err());
    assert!(
        edit_git_page(
            &b,
            b.id(),
            &range,
            b.entries()
                .iter()
                .map(|e| GitIndexEdit::Remove(e.oid))
                .collect(),
            limits
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn incremental_pages_match_independent_map_through_repeated_split_and_delete() {
    use std::collections::BTreeMap;
    let limits = Limits {
        members: 7,
        bytes: 390,
    };
    let mut pages = paginate(vec![entry(500)], limits).unwrap();
    let mut oracle = BTreeMap::from([(entry(500).digest, entry(500))]);
    let mut seed = 0xa192786bbca31742u64;
    for _ in 0..1500 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut e = entry(seed % 1000);
        let i = pages
            .partition_point(|p| p.entries()[0].digest <= e.digest)
            .saturating_sub(1);
        let range = KeyRange {
            lower: if i == 0 {
                None
            } else {
                Some(pages[i].entries()[0].digest)
            },
            upper: pages.get(i + 1).map(|p| p.entries()[0].digest),
        };
        let edit = match oracle.entry(e.digest) {
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if seed & 1 == 0 {
                    slot.remove();
                    Edit::Remove(e.digest)
                } else {
                    e.attributes = seed.to_le_bytes().to_vec();
                    slot.insert(e.clone());
                    Edit::Replace(e)
                }
            }
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(e.clone());
                Edit::Insert(e)
            }
        };
        let old = pages[i].clone();
        let updated = edit_page(&old, old.id(), &range, vec![edit], limits).unwrap();
        pages.splice(i..=i, updated);
        assert_eq!(
            pages
                .iter()
                .flat_map(|p| p.entries())
                .cloned()
                .collect::<Vec<_>>(),
            oracle.values().cloned().collect::<Vec<_>>()
        );
        for pair in pages.windows(2) {
            pair[0]
                .page()
                .unwrap()
                .precedes(&pair[1].page().unwrap())
                .unwrap();
        }
    }
}

#[test]
fn replacements_preserve_size_and_registered_identities_while_metadata_changes() {
    let limits = Limits::default();
    let mut e = entry(1);
    e.git = vec![GitIdentity {
        oid: mapping(1, false).oid,
        kind: GitKind::Blob,
    }];
    e.roles |= GitKind::Blob.role();
    let old = Block::build(vec![e.clone()], limits).unwrap();
    let mut wrong = e.clone();
    wrong.bytes += 1;
    assert!(
        edit_page(
            &old,
            old.id(),
            &unbounded(),
            vec![Edit::Replace(wrong)],
            limits
        )
        .is_err()
    );
    let mut wrong = e.clone();
    wrong.git.clear();
    wrong.roles &= 0xf0;
    assert!(
        edit_page(
            &old,
            old.id(),
            &unbounded(),
            vec![Edit::Replace(wrong)],
            limits
        )
        .is_err()
    );
    let mut wrong = e.clone();
    wrong.git[0].kind = GitKind::Tree;
    wrong.roles = ATTACHMENT | GitKind::Tree.role();
    assert!(
        edit_page(
            &old,
            old.id(),
            &unbounded(),
            vec![Edit::Replace(wrong)],
            limits
        )
        .is_err()
    );
    let mut changed = e;
    changed.attributes = b"state=deleting".to_vec();
    changed.git.push(GitIdentity {
        oid: mapping(1, true).oid,
        kind: GitKind::Blob,
    });
    let next = edit_page(
        &old,
        old.id(),
        &unbounded(),
        vec![Edit::Replace(changed.clone())],
        limits,
    )
    .unwrap();
    assert_eq!(next[0].entries(), &[changed]);
    assert_ne!(old.id(), next[0].id());
}

#[test]
fn expired_orphans_without_roles_roundtrip_all_lifecycle_metadata() {
    let mut ids = std::collections::BTreeSet::new();
    for state in ["staging", "stored", "deleting"] {
        let mut e = entry(1);
        e.roles = 0;
        e.attributes = format!("generation=7;state={state};accounted=true").into_bytes();
        e.location = if state == "staging" {
            Location::Pending
        } else {
            Location::Loose
        };
        let b = Block::build(vec![e.clone()], Limits::default()).unwrap();
        assert!(ids.insert(*b.id()));
        assert_eq!(
            Block::decode(b.encoded(), b.id(), Limits::default())
                .unwrap()
                .entries(),
            &[e]
        );
    }
    let mut invalid = entry(1);
    invalid.roles = 0;
    invalid.git = vec![GitIdentity {
        oid: mapping(1, false).oid,
        kind: GitKind::Blob,
    }];
    assert!(Block::build(vec![invalid], Limits::default()).is_err());
}

use alt_catalog_block::*;
use std::collections::BTreeSet;
mod indexed;

fn entry(n: u64) -> Entry {
    let mut digest = [0; 32];
    digest[..8].copy_from_slice(&n.to_be_bytes());
    Entry {
        digest,
        bytes: n % 17,
        roles: ATTACHMENT,
        git: vec![],
        location: Location::Loose,
        attributes: vec![],
    }
}
fn block(n: usize) -> Block {
    Block::build((0..n as u64).map(entry).collect(), Limits::default()).unwrap()
}
fn resign(bytes: &mut [u8]) -> Digest {
    let end = bytes.len() - 32;
    let id = *alt_hash::blake3::hash(&bytes[..end]).as_bytes();
    bytes[end..].copy_from_slice(&id);
    id
}

#[test]
fn deterministic_multi_role_and_multi_algorithm_roundtrip() {
    let mut first = entry(7);
    first.roles |= RECORD | TASK_BUNDLE | CHUNK | GitKind::Blob.role() | GitKind::Tree.role();
    first.git = vec![
        GitIdentity {
            oid: ObjectId::Sha256([3; 32]),
            kind: GitKind::Blob,
        },
        GitIdentity {
            oid: ObjectId::Sha1([1; 20]),
            kind: GitKind::Blob,
        },
        GitIdentity {
            oid: ObjectId::Sha1([2; 20]),
            kind: GitKind::Tree,
        },
    ];
    first.location = Location::Segment {
        id: [7; 16],
        index: [9; 32],
        offset: 4096,
    };
    let a = Block::build(vec![entry(8), first.clone(), entry(0)], Limits::default()).unwrap();
    first.git.reverse();
    let b = Block::build(vec![first, entry(0), entry(8)], Limits::default()).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        Block::decode(a.encoded(), a.id(), Limits::default()).unwrap(),
        a
    );
    assert_eq!(a.find(&entry(7).digest).unwrap().0, 1);
    assert!(a.find(&entry(9).digest).is_none());
}

#[test]
fn duplicate_payloads_union_roles_without_shadowing() {
    let a = entry(1);
    let mut b = a.clone();
    b.roles = CHUNK;
    let block = Block::build(vec![a.clone(), b.clone(), a], Limits::default()).unwrap();
    assert_eq!(block.entries().len(), 1);
    assert_eq!(block.entries()[0].roles, ATTACHMENT | CHUNK);
    b.bytes += 1;
    assert!(Block::build(vec![entry(1), b], Limits::default()).is_err());
    let mut b = entry(1);
    b.location = Location::Segment {
        id: [1; 16],
        index: [2; 32],
        offset: 64,
    };
    assert!(Block::build(vec![entry(1), b], Limits::default()).is_err());
}

#[test]
fn rejects_conflicting_git_identity_across_page_boundary() {
    let mut a = entry(0);
    a.roles = GitKind::Blob.role();
    a.git.push(GitIdentity {
        oid: ObjectId::Sha1([1; 20]),
        kind: GitKind::Blob,
    });
    let mut b = a.clone();
    b.digest = entry(9000).digest;
    let mut entries: Vec<_> = (1..9000).map(entry).collect();
    entries.extend([a, b]);
    assert_eq!(
        paginate(entries, Limits::default()),
        Err(Error::Conflict("Git mapping"))
    );
}

#[test]
fn rejects_zero_git_ids_and_kind_alias_conflicts() {
    let mut a = entry(0);
    a.roles = GitKind::Blob.role();
    a.git = vec![GitIdentity {
        oid: ObjectId::Sha1([0; 20]),
        kind: GitKind::Blob,
    }];
    assert!(Block::build(vec![a.clone()], Limits::default()).is_err());
    a.git[0].oid = ObjectId::Sha1([1; 20]);
    a.git.push(GitIdentity {
        oid: ObjectId::Sha1([2; 20]),
        kind: GitKind::Blob,
    });
    assert!(Block::build(vec![a.clone()], Limits::default()).is_err());
    a.git[1].oid = ObjectId::Sha1([1; 20]);
    a.git[1].kind = GitKind::Tree;
    a.roles |= GitKind::Tree.role();
    assert!(Block::build(vec![a], Limits::default()).is_err());
}

#[test]
fn rejects_role_mismatch_and_segment_overflow() {
    for roles in [GitKind::Blob.role(), GitKind::Commit.role()] {
        let mut a = entry(0);
        a.roles = roles;
        assert!(Block::build(vec![a], Limits::default()).is_err());
    }
    for offset in [63, u64::MAX] {
        let mut a = entry(1);
        a.location = Location::Segment {
            id: [0; 16],
            index: [0; 32],
            offset,
        };
        assert!(Block::build(vec![a], Limits::default()).is_err());
    }
}

#[test]
fn empty_and_member_limit_boundaries() {
    let empty = block(0);
    assert_eq!(
        Block::decode(empty.encoded(), empty.id(), Limits::default()).unwrap(),
        empty
    );
    assert!(paginate(vec![], Limits::default()).unwrap().is_empty());
    let maximum = block(MAX_MEMBERS);
    assert_eq!(maximum.entries().len(), MAX_MEMBERS);
    assert!(
        Block::build(
            (0..=MAX_MEMBERS as u64).map(entry).collect(),
            Limits::default()
        )
        .is_err()
    );
    let pages = paginate(
        (0..=MAX_MEMBERS as u64).rev().map(entry).collect(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        pages.iter().map(|b| b.entries().len()).collect::<Vec<_>>(),
        [MAX_MEMBERS, 1]
    );
    assert_eq!(pages[0], maximum);
    assert!(
        Block::decode(
            maximum.encoded(),
            maximum.id(),
            Limits {
                members: MAX_MEMBERS - 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
}

#[test]
fn byte_limit_splits_and_permutation_does_not_change_pages() {
    // 48-byte envelope + two 48-byte members exactly fill a page.
    let limits = Limits {
        members: 10,
        bytes: 144,
    };
    let expected = paginate((0..7).map(entry).collect(), limits).unwrap();
    let actual = paginate((0..7).rev().map(entry).collect(), limits).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        actual.iter().map(|b| b.entries().len()).collect::<Vec<_>>(),
        [2, 2, 2, 1]
    );
    assert!(Block::build((0..3).map(entry).collect(), limits).is_err());
    assert!(
        paginate(
            vec![entry(0)],
            Limits {
                members: 1,
                bytes: 95
            }
        )
        .is_err()
    );
    assert_eq!(
        paginate(
            vec![entry(0)],
            Limits {
                members: 1,
                bytes: 96
            }
        )
        .unwrap()
        .len(),
        1
    );
    for limits in [
        Limits {
            members: 0,
            bytes: MAX_BYTES,
        },
        Limits {
            members: MAX_MEMBERS + 1,
            bytes: MAX_BYTES,
        },
        Limits {
            members: 1,
            bytes: MAX_BYTES + 1,
        },
        Limits {
            members: 1,
            bytes: 47,
        },
    ] {
        assert!(paginate(vec![], limits).is_err());
    }
}

#[test]
fn truncation_damage_extra_bytes_and_wrong_identity_fail() {
    let a = block(4);
    for n in 0..a.encoded().len() {
        assert!(Block::decode(&a.encoded()[..n], a.id(), Limits::default()).is_err());
    }
    for n in 0..a.encoded().len() {
        let mut bytes = a.encoded().to_vec();
        bytes[n] ^= 1;
        assert!(Block::decode(&bytes, a.id(), Limits::default()).is_err());
    }
    let mut bytes = a.encoded().to_vec();
    bytes.push(0);
    assert!(Block::decode(&bytes, a.id(), Limits::default()).is_err());
    assert!(Block::decode(a.encoded(), &[0; 32], Limits::default()).is_err());
}

#[test]
fn checksummed_invalid_format_is_still_rejected() {
    let a = block(2);
    for (offset, value) in [
        (0, b'X'),
        (8, 3),
        (12, 1),
        (56, GitKind::Commit.role()),
        (57, 9),
        (58, 3),
        (59, 1),
    ] {
        let mut bytes = a.encoded().to_vec();
        bytes[offset] = value;
        let id = resign(&mut bytes);
        assert!(
            Block::decode(&bytes, &id, Limits::default()).is_err(),
            "offset {offset}"
        );
    }
    // Duplicate a key and then reverse the two distinct entries.
    let mut bytes = a.encoded().to_vec();
    bytes.copy_within(16..48, 64);
    let id = resign(&mut bytes);
    assert!(Block::decode(&bytes, &id, Limits::default()).is_err());
    let mut bytes = a.encoded().to_vec();
    let first = bytes[16..64].to_vec();
    bytes.copy_within(64..112, 16);
    bytes[64..112].copy_from_slice(&first);
    let id = resign(&mut bytes);
    assert!(Block::decode(&bytes, &id, Limits::default()).is_err());
}

#[test]
fn exact_sets_match_independent_set_algebra_for_all_byte_tails() {
    for n in 0..=65 {
        let b = block(n);
        let left: BTreeSet<_> = (0..n).filter(|i| i % 3 == 0 || i % 7 == 0).collect();
        let right: BTreeSet<_> = (0..n).filter(|i| i % 2 == 0 || i % 5 == 0).collect();
        let a = MemberSet::from_indices(&b, left.iter().copied()).unwrap();
        let c = MemberSet::from_indices(&b, right.iter().copied()).unwrap();
        assert_eq!(MemberSet::decode(&a.encode(), &b).unwrap(), a);
        assert_eq!(
            a.union(&c).unwrap().indices().collect::<BTreeSet<_>>(),
            left.union(&right).copied().collect()
        );
        assert_eq!(
            a.intersection(&c)
                .unwrap()
                .indices()
                .collect::<BTreeSet<_>>(),
            left.intersection(&right).copied().collect()
        );
        assert_eq!(
            a.difference(&c).unwrap().indices().collect::<BTreeSet<_>>(),
            left.difference(&right).copied().collect()
        );
        assert!(!a.contains(n));
        assert!(MemberSet::from_indices(&b, [n]).is_err());
    }
}

#[test]
fn maximum_set_and_duplicate_indices() {
    let b = block(MAX_MEMBERS);
    let all = MemberSet::from_indices(&b, 0..MAX_MEMBERS).unwrap();
    assert_eq!(
        MemberSet::decode(&all.encode(), &b)
            .unwrap()
            .indices()
            .count(),
        MAX_MEMBERS
    );
    let one = MemberSet::from_indices(&b, [3, 3, 3]).unwrap();
    assert_eq!(one.indices().collect::<Vec<_>>(), [3]);
}

#[test]
fn member_sets_reject_changed_blocks_counts_unused_bits_and_damage() {
    let b = block(9);
    let set = MemberSet::from_indices(&b, [0, 8]).unwrap();
    let mut changed = b.entries().to_vec();
    changed[0].roles |= CHUNK;
    let changed = Block::build(changed, Limits::default()).unwrap();
    assert!(MemberSet::decode(&set.encode(), &changed).is_err());
    assert!(set.union(&MemberSet::empty(&changed)).is_err());
    assert!(set.intersection(&MemberSet::empty(&block(8))).is_err());
    for n in 0..set.encode().len() {
        assert!(MemberSet::decode(&set.encode()[..n], &b).is_err());
        let mut bytes = set.encode();
        bytes[n] ^= 1;
        assert!(MemberSet::decode(&bytes, &b).is_err());
    }
    for (offset, value) in [(0, b'X'), (8, 99), (40, 8), (45, 0x80)] {
        let mut bytes = set.encode();
        bytes[offset] = value;
        resign(&mut bytes);
        assert!(MemberSet::decode(&bytes, &b).is_err());
    }
    let mut bytes = set.encode();
    bytes.push(0);
    assert!(MemberSet::decode(&bytes, &b).is_err());
}

#[test]
fn rebuilding_directory_keeps_previous_pinned_members_unchanged() {
    let limits = Limits {
        members: 3,
        ..Limits::default()
    };
    let old = paginate((10..19).map(entry).collect(), limits).unwrap();
    let pins: Vec<_> = old
        .iter()
        .map(|b| MemberSet::from_indices(b, [1]).unwrap())
        .collect();
    let pinned: Vec<_> = old
        .iter()
        .zip(&pins)
        .map(|(b, p)| b.entries()[p.indices().next().unwrap()].clone())
        .collect();
    let mut next: Vec<_> = old
        .iter()
        .flat_map(|b| b.entries().to_vec())
        .filter(|e| e.digest != entry(11).digest)
        .collect();
    next.extend([entry(9), entry(12), entry(19)]);
    let updated = paginate(next, limits).unwrap();
    assert_ne!(old[0].id(), updated[0].id());
    for ((b, p), expected) in old.iter().zip(pins).zip(pinned) {
        let restored = Block::decode(b.encoded(), b.id(), limits).unwrap();
        let members = MemberSet::decode(&p.encode(), &restored).unwrap();
        assert_eq!(
            restored.entries()[members.indices().next().unwrap()],
            expected
        );
    }
    assert_eq!(
        updated
            .iter()
            .flat_map(|b| b.entries())
            .map(|e| e.digest)
            .collect::<BTreeSet<_>>(),
        (9..20)
            .filter(|i| *i != 11)
            .map(|i| entry(i).digest)
            .collect()
    );
}

#[test]
fn deterministic_malformed_input_sweep_does_not_panic_or_accept_noncanonical_bytes() {
    let b = block(20);
    let mut seed = 0xa192786bbca31742u64;
    for _ in 0..1000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut bytes = b.encoded().to_vec();
        let offset = seed as usize % (bytes.len() - 32);
        bytes[offset] ^= ((seed >> 32) as u8) | 1;
        let id = resign(&mut bytes);
        if let Ok(decoded) = Block::decode(&bytes, &id, Limits::default()) {
            let rebuilt = Block::build(decoded.entries().to_vec(), Limits::default()).unwrap();
            assert_eq!(rebuilt.encoded(), bytes);
        }
    }
}

#[test]
fn pending_members_are_distinct_from_stored_bytes_and_registered_git_objects() {
    let mut pending = entry(0);
    pending.location = Location::Pending;
    pending.roles = 0;
    let before = Block::build(vec![pending.clone()], Limits::default()).unwrap();
    assert_eq!(
        Block::decode(before.encoded(), before.id(), Limits::default()).unwrap(),
        before
    );
    let mut stored = pending.clone();
    stored.location = Location::Loose;
    stored.roles = CHUNK;
    let after = Block::build(vec![stored.clone()], Limits::default()).unwrap();
    assert_ne!(before.id(), after.id());
    let pin = MemberSet::from_indices(&before, [0]).unwrap();
    assert!(MemberSet::decode(&pin.encode(), &after).is_err());
    assert!(Block::build(vec![pending.clone(), stored], Limits::default()).is_err());
    pending.git.push(GitIdentity {
        oid: ObjectId::Sha1([1; 20]),
        kind: GitKind::Blob,
    });
    pending.roles = GitKind::Blob.role();
    assert!(Block::build(vec![pending], Limits::default()).is_err());
}

#[test]
fn attributes_are_bound_to_identity_and_exact_byte_limit() {
    let mut a = entry(1);
    a.attributes = b"generation=1;state=stored".to_vec();
    let old = Block::build(vec![a.clone()], Limits::default()).unwrap();
    a.attributes = b"generation=1;state=deleting".to_vec();
    let next = Block::build(vec![a.clone()], Limits::default()).unwrap();
    assert_ne!(old.id(), next.id());
    assert!(
        MemberSet::decode(&MemberSet::from_indices(&old, [0]).unwrap().encode(), &next).is_err()
    );
    assert!(Block::build(vec![old.entries()[0].clone(), a.clone()], Limits::default()).is_err());
    a.attributes = vec![0xa5; MAX_BYTES - 96];
    let maximum = Block::build(vec![a.clone()], Limits::default()).unwrap();
    assert_eq!(maximum.encoded().len(), MAX_BYTES);
    assert_eq!(
        Block::decode(maximum.encoded(), maximum.id(), Limits::default())
            .unwrap()
            .entries()[0],
        a
    );
    a.attributes.push(0);
    assert!(Block::build(vec![a], Limits::default()).is_err());
    let mut corrupt = old.encoded().to_vec();
    corrupt[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
    let id = resign(&mut corrupt);
    assert!(Block::decode(&corrupt, &id, Limits::default()).is_err());
    let mut legacy = old.encoded().to_vec();
    legacy[7] = b'1';
    let id = resign(&mut legacy);
    assert!(Block::decode(&legacy, &id, Limits::default()).is_err());
}

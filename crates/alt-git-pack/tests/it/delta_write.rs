use std::{
    fs::File,
    process::{Command, Stdio},
};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_pack::{DeltaOptions, EntryKind, IndexedPack, PackWriter};

fn versions() -> Vec<Vec<u8>> {
    let mut state = 0x5190a71bu64;
    let mut data: Vec<_> = (0..65536)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let mut result = vec![data.clone()];
    for edit in 1..=11 {
        data.splice(edit * 91..edit * 91 + 9, (0..17).map(|i| (i + edit) as u8));
        data.drain(3000 + edit..3000 + edit + 7);
        data.extend_from_slice(&[edit as u8; 11]);
        result.push(data.clone());
    }
    result
}

#[test]
fn bounded_delta_packs_resolve_and_pass_independent_git_for_both_hashes() {
    for (format, algo) in [("sha1", HashAlgo::Sha1), ("sha256", HashAlgo::Sha256)] {
        let mut inputs = versions();
        // Exercise bounded index coverage on objects larger than the complete
        // slot capacity, including unaligned changes, with independent Git.
        let mut state = 0x719af05u64;
        let large: Vec<_> = (0..32 << 20)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let mut edited = b"unaligned insertion".to_vec();
        edited.extend_from_slice(&large);
        edited[7_000_019..7_000_051].fill(91);
        edited.drain(21_000_007..21_000_020);
        inputs.extend([large, edited]);
        let objects: Vec<_> = inputs
            .into_iter()
            .map(|data| (ObjectId::hash_object(algo, ObjectKind::Blob, &data), data))
            .collect();
        let output = alt_os::temp::tempdir().unwrap();
        let options = DeltaOptions {
            window: 1,
            max_depth: 2,
            ..DeltaOptions::default()
        };
        let mut writer =
            PackWriter::create_with_deltas(output.path(), algo, objects.len() as u32, options)
                .unwrap();
        for (oid, data) in &objects {
            writer.add(*oid, ObjectKind::Blob, data).unwrap();
        }
        let written = writer.finish().unwrap();
        let pack = IndexedPack::open(&written.pack_path, algo).unwrap();
        let mut depths = std::collections::BTreeMap::new();
        for (oid, original) in &objects {
            let index = pack.idx().lookup(oid).unwrap();
            let entry = pack
                .pack()
                .entry_info(pack.idx().offset_at(index).unwrap())
                .unwrap();
            let depth = match entry.kind {
                EntryKind::Plain(ObjectKind::Blob) => 0,
                EntryKind::RefDelta { base } => {
                    depths
                        .get(&base)
                        .copied()
                        .expect("base was already emitted")
                        + 1
                }
                other => panic!("unexpected entry: {other:?}"),
            };
            assert!(depth <= 2);
            depths.insert(*oid, depth);
            let resolved = pack.read(oid).unwrap().unwrap();
            assert_eq!(resolved.kind, ObjectKind::Blob);
            assert_eq!(*resolved.data, *original);
        }
        assert_eq!(
            depths
                .values()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [0, 1, 2].into()
        );

        // A fresh Git repo has neither our idx nor external bases. Git builds
        // its own index, verifies the delta chain, then resolves every byte.
        let repo = alt_os::temp::tempdir().unwrap();
        let init = Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(format!("--object-format={format}"))
            .arg(repo.path())
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let checked = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["index-pack", "--strict", "--stdin"])
            .stdin(Stdio::from(File::open(&written.pack_path).unwrap()))
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        for (oid, original) in &objects {
            let read = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["cat-file", "blob", &oid.to_string()])
                .output()
                .unwrap();
            assert!(
                read.status.success(),
                "{}",
                String::from_utf8_lossy(&read.stderr)
            );
            assert_eq!(read.stdout, *original);
        }
        let verify = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .arg("verify-pack")
            .arg(&written.idx_path)
            .output()
            .unwrap();
        assert!(
            verify.status.success(),
            "{}",
            String::from_utf8_lossy(&verify.stderr)
        );
        let fsck = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["fsck", "--full", "--strict"])
            .output()
            .unwrap();
        assert!(
            fsck.status.success(),
            "{}",
            String::from_utf8_lossy(&fsck.stderr)
        );
    }
}

#[test]
fn exceeded_or_disabled_delta_limits_keep_plain_entries() {
    for options in [
        DeltaOptions {
            window: 0,
            ..DeltaOptions::default()
        },
        DeltaOptions {
            memory_bytes: 100,
            ..DeltaOptions::default()
        },
        DeltaOptions {
            max_object_bytes: 100,
            ..DeltaOptions::default()
        },
        DeltaOptions {
            max_depth: 0,
            ..DeltaOptions::default()
        },
    ] {
        let dir = alt_os::temp::tempdir().unwrap();
        let objects = versions();
        let mut writer = PackWriter::create_with_deltas(
            dir.path(),
            HashAlgo::Sha1,
            objects.len() as u32,
            options,
        )
        .unwrap();
        let mut ids = Vec::new();
        for data in &objects {
            let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, data);
            writer.add(oid, ObjectKind::Blob, data).unwrap();
            ids.push(oid);
        }
        let output = writer.finish().unwrap();
        let pack = IndexedPack::open(&output.pack_path, HashAlgo::Sha1).unwrap();
        for (oid, data) in ids.iter().zip(&objects) {
            let entry = pack
                .pack()
                .entry_info(
                    pack.idx()
                        .offset_at(pack.idx().lookup(oid).unwrap())
                        .unwrap(),
                )
                .unwrap();
            assert_eq!(entry.kind, EntryKind::Plain(ObjectKind::Blob));
            assert_eq!(*pack.read(oid).unwrap().unwrap().data, *data);
        }
    }
}

#[test]
fn mixed_candidate_filter_keeps_shifted_objects_for_independent_git() {
    for (format, algo) in [("sha1", HashAlgo::Sha1), ("sha256", HashAlgo::Sha256)] {
        let mut state = 0x94ab59d719u64;
        let mut inputs: Vec<Vec<u8>> = (0..10)
            .map(|_| {
                (0..4096)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state as u8
                    })
                    .collect()
            })
            .collect();
        let mut shifted = b"unaligned insertion".to_vec();
        shifted.extend_from_slice(&inputs[7][31..]);
        inputs.push(shifted);
        let output = alt_os::temp::tempdir().unwrap();
        let mut writer = PackWriter::create_with_deltas(
            output.path(),
            algo,
            inputs.len() as u32,
            DeltaOptions::default(),
        )
        .unwrap();
        let ids: Vec<_> = inputs
            .iter()
            .map(|data| ObjectId::hash_object(algo, ObjectKind::Blob, data))
            .collect();
        for (oid, data) in ids.iter().zip(&inputs) {
            writer.add(*oid, ObjectKind::Blob, data).unwrap();
        }
        let written = writer.finish().unwrap();
        let pack = IndexedPack::open(&written.pack_path, algo).unwrap();
        let entry = pack
            .pack()
            .entry_info(
                pack.idx()
                    .offset_at(pack.idx().lookup(ids.last().unwrap()).unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert!(matches!(entry.kind, EntryKind::RefDelta { .. }));
        let repo = alt_os::temp::tempdir().unwrap();
        let init = Command::new("git")
            .args(["init", "--bare", &format!("--object-format={format}")])
            .arg(repo.path())
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let checked = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["index-pack", "--strict", "--stdin"])
            .stdin(Stdio::from(File::open(&written.pack_path).unwrap()))
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        for (oid, data) in ids.iter().zip(&inputs) {
            let result = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["cat-file", "blob", &oid.to_string()])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(&result.stdout, data);
        }
    }
}

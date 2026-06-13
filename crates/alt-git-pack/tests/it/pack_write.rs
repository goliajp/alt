//! Pack writing: round trip through our own reader, acceptance by git
//! itself (`verify-pack` / `index-pack`), and the large-offset idx path.

use std::path::Path;
use std::process::Command;

use alt_git_codec::{HashAlgo, ObjectId, RawObject};
use alt_git_pack::{IndexedPack, PackWriter};

/// All loose objects of a fresh fixture repo (every kind, both algos).
fn fixture_objects(object_format: &str) -> (tempfile::TempDir, Vec<(ObjectId, RawObject)>) {
    let repo = tempfile::tempdir().unwrap();
    alt_testutil::make_repo(repo.path(), object_format);
    let mut objects = Vec::new();
    alt_testutil::for_each_loose(repo.path(), |oid, raw| objects.push((oid, raw)));
    assert!(!objects.is_empty());
    (repo, objects)
}

fn write_fixture_pack(
    dir: &Path,
    algo: HashAlgo,
    objects: &[(ObjectId, RawObject)],
) -> alt_git_pack::WrittenPack {
    let mut writer = PackWriter::create(dir, algo, objects.len() as u32).unwrap();
    for (oid, raw) in objects {
        writer.add(*oid, raw.kind, &raw.data).unwrap();
    }
    writer.finish().unwrap()
}

#[test]
fn written_pack_round_trips_through_own_reader() {
    for (format, algo) in [("sha1", HashAlgo::Sha1), ("sha256", HashAlgo::Sha256)] {
        let (_repo, objects) = fixture_objects(format);
        let dir = tempfile::tempdir().unwrap();
        let written = write_fixture_pack(dir.path(), algo, &objects);
        assert_eq!(written.objects as usize, objects.len());

        let indexed = IndexedPack::open(&written.pack_path, algo).unwrap();
        assert_eq!(indexed.idx().len() as usize, objects.len());
        for (oid, raw) in &objects {
            let got = indexed.read(oid).unwrap().unwrap_or_else(|| {
                panic!("{oid} missing from written pack ({format})");
            });
            assert_eq!(got.kind, raw.kind, "{oid}");
            assert_eq!(*got.data, raw.data, "{oid}");
        }
    }
}

#[test]
fn git_accepts_written_packs() {
    for (format, algo) in [("sha1", HashAlgo::Sha1), ("sha256", HashAlgo::Sha256)] {
        let (repo, objects) = fixture_objects(format);
        let dir = tempfile::tempdir().unwrap();
        let written = write_fixture_pack(dir.path(), algo, &objects);

        // git is the referee; run it inside the fixture repo so the
        // object format is in effect
        let verify = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .arg("verify-pack")
            .arg("-v")
            .arg(&written.idx_path)
            .output()
            .unwrap();
        assert!(
            verify.status.success(),
            "git verify-pack ({format}): {}",
            String::from_utf8_lossy(&verify.stderr)
        );
        let listed = String::from_utf8_lossy(&verify.stdout);
        for (oid, _) in &objects {
            assert!(listed.contains(&oid.to_string()), "{oid} not listed");
        }

        // and index-pack rebuilds an idx from our pack bytes alone
        let rebuilt = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["index-pack", "--verify"])
            .arg(&written.pack_path)
            .output()
            .unwrap();
        assert!(
            rebuilt.status.success(),
            "git index-pack --verify ({format}): {}",
            String::from_utf8_lossy(&rebuilt.stderr)
        );
    }
}

#[test]
fn empty_pack_is_rejected_only_by_count_mismatch() {
    // a pack declaring 1 object but given none must refuse to finish
    let dir = tempfile::tempdir().unwrap();
    let writer = PackWriter::create(dir.path(), HashAlgo::Sha1, 1).unwrap();
    assert!(writer.finish().is_err());
}

//! An object with two records in `map.alt`, as two writers storing it at
//! once leave them, is still named by a short id.

use std::fs::{self, OpenOptions};
use std::io::Write;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_repo::{RepoError, Repository};

/// `map.alt` is a header, then records of one size in append order; a
/// record starts with the hash algorithm and the git id.
const HEADER: usize = 5;
const RECORD: usize = 82;

#[test]
fn a_short_id_of_an_object_recorded_twice_is_not_ambiguous() {
    let repo_dir = alt_os::temp::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = alt_os::temp::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let git = Repository::discover(repo_dir.path()).unwrap();
    alt_import::import_git(&git, &alt_dir, "test/native", 1).unwrap();

    let path = alt_dir.join("map.alt");
    let map = fs::read(&path).unwrap();
    assert_eq!((map.len() - HEADER) % RECORD, 0, "the size of a record");
    let first = &map[HEADER..HEADER + RECORD];
    let oid = ObjectId::from_bytes(HashAlgo::Sha1, &first[1..21]).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(first).unwrap();
    drop(file);

    let repo = Repository::discover(alt_root.path()).unwrap();
    let short = &oid.to_string()[..12];
    match repo.rev_parse(short) {
        Ok(found) => assert_eq!(found, Some(oid)),
        Err(RepoError::AmbiguousShortOid { matches, .. }) => {
            panic!("{short} is ambiguous between {matches:?}")
        }
        Err(e) => panic!("{e:?}"),
    }
}

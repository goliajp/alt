//! What the refs lead to, found where a record of an object they do not lead
//! to cannot be read.

use super::*;

/// The files of the segments under `directory`, every one.
fn files(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        match path.is_dir() {
            true => files(&path, found),
            false => found.push(path),
        }
    }
}

/// A walk of the refs that ends, where asking whether what it found holds
/// every object of the segments reads a record that is not read as it was
/// written: the push fails, and what was found is kept and written down. The
/// next push of the same refs goes on from it, and asks no more.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn what_the_refs_lead_to_is_kept_where_a_record_they_do_not_lead_to_is_not_read() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let url = format!("{}/git/repository", server.url);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    let pushed = |what: &[&str]| {
        let pushing = super::git(work, &token, &[&["push", "-q", &url][..], what].concat());
        let said = String::from_utf8_lossy(&pushing.stderr).into_owned();
        (pushing.status.success(), said)
    };
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    let commit = |n: usize| {
        std::fs::write(work.join("file"), format!("{n}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", &format!("revision {n}")]);
        git(&["rev-parse", "HEAD"])
    };
    commit(0);
    assert!(pushed(&["main"]).0);
    git(&["checkout", "-q", "-b", "gone"]);
    let gone = commit(1);
    assert!(pushed(&["gone"]).0);
    git(&["checkout", "-q", "main"]);
    assert!(pushed(&[":gone"]).0);

    // The records of the objects of the deleted branch, in the segment its
    // push made: all of them, as the records are read in the order of the
    // ids and the first one the refs do not lead to ends the reading.
    let ids: Vec<Vec<u8>> = [
        gone.clone(),
        git(&["rev-parse", "gone^{tree}"]),
        git(&["rev-parse", "gone:file"]),
    ]
    .iter()
    .map(|id| {
        (0..20)
            .map(|at| u8::from_str_radix(&id[at * 2..at * 2 + 2], 16).unwrap())
            .collect()
    })
    .collect();
    let mut segments = Vec::new();
    files(&f.root.path().join("bytes"), &mut segments);
    let mut changed = 0;
    for path in segments {
        let mut bytes = std::fs::read(&path).unwrap();
        let places: Vec<usize> = (0..bytes.len().saturating_sub(88))
            .filter(|at| ids.iter().any(|id| bytes[*at..*at + 20] == id[..]))
            .filter(|at| (1..=4).contains(&bytes[at + 80]) && bytes[at + 81] <= 2)
            .collect();
        for at in places {
            bytes[at + 81] = 9;
            changed += 1;
        }
        if changed > 0 {
            use std::io::{Seek, Write};
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(std::io::SeekFrom::Start(0)).unwrap();
            file.write_all(&bytes).unwrap();
            // A segment small enough for its row is written out of the row
            // again where the process opens it anew, as it does once the
            // mapping is let go of among those of every test: the row is
            // changed the same.
            let id = path.file_stem().unwrap().to_str().unwrap().to_owned();
            let row =
                "UPDATE alt_pack_segments SET inline=$2 WHERE segment_id=$1 AND inline IS NOT NULL";
            f.client().execute(row, &[&id, &bytes]).unwrap();
            break;
        }
    }
    assert!(changed > 0);

    commit(2);
    let (done, said) = pushed(&["main"]);
    assert!(!done, "{said}");
    let packs = f.root.path().join("bytes").join("packs");
    let stored = packs.join("led_computed");
    assert_eq!(std::fs::read_dir(stored).unwrap().count(), 1);
    let (done, said) = pushed(&["main"]);
    assert!(done, "{said}");
}

//! What the refs lead to, found by a server, is gone on from by one that
//! starts again on the same bytes.

use super::*;

fn copied(from: &Path, to: &Path) {
    std::fs::create_dir(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copied(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A server that walks the refs of a repository that had a branch deleted
/// leaves what it found beside the segments. One that starts again on a copy
/// of those bytes, whose walks may read three trees, takes a push of a commit:
/// it goes on from what was left. Without it, the walk reads more and the push
/// is refused. What was left and is not read as it was written, here with the
/// commit of the deleted branch put among the objects, is found anew: the
/// commit is no more led to than it was, and what is found is written again.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn what_a_server_found_the_refs_lead_to_is_gone_on_from_by_one_that_starts_again() {
    let f = Fixture::new();
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    // three directories, of which a commit changes one: two trees are new with it
    let commit = |n: usize| {
        let directory = work.join(format!("d{}", n % 3));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("file"), format!("{n}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", &format!("revision {n}")]);
        git(&["rev-parse", "HEAD"])
    };
    let server = |bytes: &Path, steps: usize| {
        let (mut factory, options) = Running::parts(&f, None, vec![]);
        factory.bytes = ByteStore::open(bytes).unwrap();
        factory.limits.walk_steps = steps;
        Running::serve(factory, options, None, false)
    };
    let pushed = |server: &Running, what: &[&str]| {
        let url = format!("{}/git/repository", server.url);
        let pushing = super::git(work, &token, &[&["push", "-q", &url][..], what].concat());
        let said = String::from_utf8_lossy(&pushing.stderr).into_owned();
        (pushing.status.success(), said)
    };
    let stored = |bytes: &Path| {
        let directory = bytes.join("packs").join("led_computed");
        let files: Vec<_> = (std::fs::read_dir(directory).unwrap())
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        files[0].clone()
    };
    let bytes = f.root.path().join("bytes");
    let first = server(&bytes, 100_000);
    (0..6).for_each(|n| drop(commit(n)));
    git(&["checkout", "-q", "-b", "gone"]);
    let gone = commit(6);
    git(&["checkout", "-q", "main"]);
    assert!(pushed(&first, &["main", "gone"]).0);
    assert!(pushed(&first, &[":gone"]).0);
    commit(7);
    assert!(pushed(&first, &["main"]).0);
    drop(first);

    // what is left is gone on from; from nothing, a walk reads more than three trees
    let again = f.root.path().join("bytes-again");
    copied(&bytes, &again);
    commit(8);
    let (done, said) = pushed(&server(&again, 3), &["main"]);
    assert!(done, "{said}");
    let empty = f.root.path().join("bytes-empty");
    copied(&again, &empty);
    std::fs::remove_file(stored(&empty)).unwrap();
    commit(9);
    assert!(
        !pushed(&server(&empty, 3), &["main"]).0,
        "a walk from nothing reads three trees"
    );

    // what is left, with the commit of the deleted branch in the place of an object
    let changed = f.root.path().join("bytes-changed");
    copied(&again, &changed);
    let mut left = std::fs::read(stored(&changed)).unwrap();
    let number = |at: usize| u64::from_le_bytes(left[at..at + 8].try_into().unwrap()) as usize;
    let named = number(17);
    let tips = number(25 + named);
    let objects = 41 + named + tips * 20;
    let id = |hex: &str| -> Vec<u8> {
        let byte = |at: usize| u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).unwrap();
        (0..20).map(byte).collect()
    };
    left[objects..objects + 20].copy_from_slice(&id(&gone));
    std::fs::write(stored(&changed), &left).unwrap();
    let fourth = server(&changed, 100_000);
    let zero = "0".repeat(40);
    let mut body = Vec::new();
    let command = format!("{zero} {gone} refs/heads/back\0report-status\n");
    alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&super::led::packed(work, &token, &[]));
    let url = format!("{}/git/repository", fourth.url);
    let (status, report) = super::raw_push::receive(&url, &token, &body);
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ng refs/heads/back"), "{report}");
    assert!(pushed(&fourth, &["main"]).0);
    drop(fourth);
    let written = std::fs::read(stored(&changed)).unwrap();
    assert!(written != left);
}

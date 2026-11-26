//! A push whose refs moved between its verification and its publication is
//! refused where it leans on, or names, what is lost by it: what the old
//! targets led to, through annotated tags too, that the refs as they are now
//! do not lead to. One that leans on what is not lost is published.

use super::led::packed;
use super::read_moved::{HOOKED, held_while};
use super::*;

const CHANGED: &str = "a ref changed since it was read";

/// a repository of `main` and `gone`, which holds a file nothing else does,
/// pushed; the work tree, the url and the tip of `main`
struct Pushed {
    _temp: alt_os::temp::TempDir,
    work: std::path::PathBuf,
    url: String,
    main: String,
}

fn pushed(f: &Fixture, server: &Running) -> Pushed {
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let url = format!("{}/git/repository", server.url);
    let git = |args: &[&str]| text(ok(&work, &f.token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(&work, &f.token);
    let commit = |file: &str, content: &str| {
        std::fs::write(work.join(file), content).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", file]);
        git(&["rev-parse", "HEAD"])
    };
    let main = commit("main.txt", "main\n");
    git(&["checkout", "-q", "-b", "gone"]);
    commit("gone.txt", "only gone has this\n");
    git(&["checkout", "-q", "main"]);
    git(&["push", "-q", &url, "main", "gone"]);
    Pushed {
        _temp: temp,
        work,
        url,
        main,
    }
}

/// the body of a push of `updates`, each an old id, a new id and a ref, with `pack`
fn body(updates: &[(&str, &str, &str)], pack: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    for (at, (old, new, name)) in updates.iter().enumerate() {
        let caps = if at == 0 { "\0report-status" } else { "" };
        let line = format!("{old} {new} {name}{caps}\n");
        alt_wire::pkt::write_data(&mut body, line.as_bytes()).unwrap();
    }
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(pack);
    body
}

fn id(kind: ObjectKind, data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, kind, data)
}

/// A pack whose tree names the file only `gone` holds and as many new files
/// beside it as are looked up in the segments at once, all of whose ids fall
/// where that one does: the file is looked up with them. While the push waits
/// to be published `gone` is deleted, and the push is refused.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_is_refused_for_a_lost_object_looked_up_with_thousands_of_others() {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut f = Fixture::new();
    // a pack of some four thousand objects, and a tree of as many entries
    let server = super::super::sign_in_deadline::start(&mut f, |factory, _| {
        factory.limits.graph_objects = 8192;
        factory.limits.tree_paths = 8192;
    });
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    let lost = ObjectId::from_hex(git(&["rev-parse", "gone:gone.txt"]).as_bytes()).unwrap();
    // the segments look up the ids of one shard of 64, 4096 at a time
    let shard = lost.as_bytes()[1] % 64;
    let mut files = Vec::new();
    let mut n = 0u64;
    while files.len() < 4095 {
        let data = format!("file {n}\n").into_bytes();
        n += 1;
        if id(ObjectKind::Blob, &data).as_bytes()[1] % 64 == shard {
            files.push(data);
        }
    }
    let mut tree = Vec::new();
    tree.extend_from_slice(b"100644 0\0");
    tree.extend_from_slice(lost.as_bytes());
    for (at, data) in files.iter().enumerate() {
        tree.extend_from_slice(format!("100644 f{at:05}\0").as_bytes());
        tree.extend_from_slice(id(ObjectKind::Blob, data).as_bytes());
    }
    let tree_id = id(ObjectKind::Tree, &tree);
    let commit = format!(
        "tree {tree_id}\nparent {}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nmany\n",
        p.main
    )
    .into_bytes();
    let commit_id = id(ObjectKind::Commit, &commit);
    let mut objects: Vec<(alt_pack_segment::Kind, &[u8])> = (files.iter())
        .map(|data| (alt_pack_segment::Kind::Blob, data.as_slice()))
        .collect();
    objects.push((alt_pack_segment::Kind::Tree, &tree));
    objects.push((alt_pack_segment::Kind::Commit, &commit));
    let pack = alt_pack_segment::build_pack(objects.into_iter()).unwrap();
    let new = commit_id.to_string();
    let body = body(&[(&p.main, &new, "refs/heads/main")], &pack);
    let (status, report) = held_while(&f, &p.url, &f.token, body, || {
        git(&["push", "-q", &p.url, ":gone"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains(&format!("ng refs/heads/main {CHANGED}")),
        "{report}"
    );
    let refs = git(&["ls-remote", "--refs", &p.url]);
    assert_eq!(refs, format!("{}\trefs/heads/main", p.main));
}

/// A branch is pushed to a commit only an annotated tag leads to, with a pack
/// of nothing; the tag is deleted while the push waits to be published, and
/// the push is refused.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_is_refused_for_a_commit_only_a_deleted_annotated_tag_led_to() {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    git(&["checkout", "-q", "--detach", &p.main]);
    std::fs::write(p.work.join("tagged.txt"), "tagged\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "tagged"]);
    let tagged = git(&["rev-parse", "HEAD"]);
    git(&["tag", "-a", "-m", "v1", "v1"]);
    git(&["checkout", "-q", "main"]);
    git(&["push", "-q", &p.url, "refs/tags/v1"]);
    let zero = "0".repeat(40);
    let body = body(
        &[(&zero, &tagged, "refs/heads/back")],
        &packed(&p.work, &f.token, &[]),
    );
    let (status, report) = held_while(&f, &p.url, &f.token, body, || {
        git(&["push", "-q", &p.url, ":refs/tags/v1"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains(&format!("ng refs/heads/back {CHANGED}")),
        "{report}"
    );
    let refs = git(&["ls-remote", "--refs", &p.url]);
    assert!(!refs.contains("refs/heads/back"), "{refs}");
}

/// A tag is pushed to the file only `gone` holds, with a pack of nothing:
/// the new target is no commit and the push leans on nothing it carries. The
/// branch is deleted while the push waits to be published, and the push is
/// refused.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_of_nothing_is_refused_for_a_new_target_that_is_no_commit_and_is_lost() {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    let file = git(&["rev-parse", "gone:gone.txt"]);
    let zero = "0".repeat(40);
    let body = body(
        &[(&zero, &file, "refs/tags/file")],
        &packed(&p.work, &f.token, &[]),
    );
    let (status, report) = held_while(&f, &p.url, &f.token, body, || {
        git(&["push", "-q", &p.url, ":gone"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains(&format!("ng refs/tags/file {CHANGED}")),
        "{report}"
    );
    let refs = git(&["ls-remote", "--refs", &p.url]);
    assert_eq!(refs, format!("{}\trefs/heads/main", p.main));
}

/// A branch rebased onto `main` is pushed with a pack of its commit and tree
/// alone, as a client that knows what the server holds sends it: it leaves
/// out the file the branch had before, which `main` does not hold (Git sends
/// that file again, finding what to leave out in the trees of the parents
/// alone). A branch that has nothing to do with it is deleted while the push
/// waits to be published, and the push is published; a clone of it has every
/// object.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_rebased_branch_is_published_though_an_unrelated_branch_is_deleted_meanwhile() {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    git(&["checkout", "-q", "-b", "feature", &p.main]);
    std::fs::write(p.work.join("feature.txt"), "a feature of its own\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "feature"]);
    let before = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(p.work.join("main.txt"), "main, later\n").unwrap();
    git(&["commit", "-qam", "later"]);
    git(&["push", "-q", &p.url, "main", "feature"]);
    git(&["checkout", "-q", "feature"]);
    git(&["rebase", "-q", "main"]);
    let after = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "main"]);
    let tree = format!("{after}^{{tree}}");
    let pack = packed(&p.work, &f.token, &[&after, &tree]);
    let body = body(&[(&before, &after, "refs/heads/feature")], &pack);
    let (status, report) = held_while(&f, &p.url, &f.token, body, || {
        git(&["push", "-q", &p.url, ":gone"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ok refs/heads/feature"), "{report}");
    let temp = alt_os::temp::tempdir().unwrap();
    ok(
        temp.path(),
        &f.token,
        &["clone", "-q", "--mirror", &p.url, "c"],
    );
    let cloned = temp.path().join("c");
    ok(&cloned, &f.token, &["fsck", "--strict"]);
    assert_eq!(
        text(ok(&cloned, &f.token, &["rev-parse", "feature"])),
        after
    );
}

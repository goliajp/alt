//! A push whose refs moved between its verification and its publication is
//! refused where it leans on what is lost by it, however many other objects
//! it leans on.

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

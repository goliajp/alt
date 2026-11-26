//! A directory whose name `git fsck` refuses, kept where the parent had it:
//! nothing is brought anew under it, by a Git push or a native publication.
//! What it held may stay, or go.

use super::fsck_anew::{commit, id, native, planted, tree};
use super::*;
use alt_wire_http::native::Transport;

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn nothing_is_brought_anew_under_a_kept_directory_whose_name_git_fsck_refuses() {
    use alt_pack_segment::Kind;
    let f = Fixture::new();
    let (x, y) = (b"x\n".as_slice(), b"y\n".as_slice());
    let (blob, other) = (id(ObjectKind::Blob, x), id(ObjectKind::Blob, y));
    let deeper = tree(&[("100644", b"a", blob)]);
    let deeper_id = id(ObjectKind::Tree, &deeper);
    // `ALT~1` a directory of `config` and of a directory `d`
    let under = tree(&[("100644", b"config", blob), ("40000", b"d", deeper_id)]);
    let under_id = id(ObjectKind::Tree, &under);
    let root = tree(&[("40000", b"ALT~1", under_id)]);
    let root_id = id(ObjectKind::Tree, &root);
    let first = commit(root_id, None, "taken before");
    let first_id = id(ObjectKind::Commit, &first);
    let objects = [
        (Kind::Blob, x),
        (Kind::Blob, y),
        (Kind::Tree, &deeper[..]),
        (Kind::Tree, &under[..]),
        (Kind::Tree, &root[..]),
        (Kind::Commit, &first[..]),
    ];
    planted(&f, &objects, first_id);
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let url = format!("{}/git/repository", server.url);
    let token = f.token.clone();
    ok(
        temp.path(),
        &token,
        &["clone", "-q", "--bare", &url, "work"],
    );
    let work = temp.path().join("work");
    let work = work.as_path();
    let git = |args: &[&str]| text(ok(work, &token, args));
    identity(work, &token);
    let written = |data: &[u8]| {
        let name = format!("tree-{}", id(ObjectKind::Tree, data));
        std::fs::write(work.join(&name), data).unwrap();
        let written = git(&["hash-object", "-t", "tree", "--literally", "-w", &name]);
        ObjectId::from_hex(written.as_bytes()).unwrap()
    };
    let pushed = |tree: ObjectId, parent: &str| {
        let tree = tree.to_string();
        let made = git(&["commit-tree", &tree, "-p", parent, "-m", "next"]);
        let pushing = super::git(
            work,
            &token,
            &["push", &url, &format!("{made}:refs/heads/main")],
        );
        let said = String::from_utf8_lossy(&pushing.stderr).into_owned();
        (made, pushing.status.success(), said)
    };
    // the root with `ALT~1` as `under` and a file beside it
    let rooted = |under: ObjectId| {
        written(&tree(&[
            ("40000", b"ALT~1", under),
            ("100644", b"m", other),
        ]))
    };
    let refused = |at: ObjectId| format!("remote: error: object {at}: hasDotalt: contains '.alt'");
    let main = || git(&["ls-remote", &url, "refs/heads/main"]);

    // a file new under it, the bytes of a file under it changed, a file new
    // in a directory below it: each refused for the name of the directory
    let hooks = written(&tree(&[
        ("100644", b"config", blob),
        ("40000", b"d", deeper_id),
        ("100644", b"hooks", other),
    ]));
    let changed = written(&tree(&[
        ("100644", b"config", other),
        ("40000", b"d", deeper_id),
    ]));
    let below = written(&tree(&[("100644", b"a", blob), ("100644", b"b", other)]));
    let deep = written(&tree(&[
        ("100644", b"config", blob),
        ("40000", b"d", below),
    ]));
    for (under, at) in [(hooks, hooks), (changed, changed), (deep, below)] {
        let (_, taken, said) = pushed(rooted(under), &first_id.to_string());
        assert!(!taken, "{said}");
        assert!(said.contains(&refused(at)), "{said}");
        assert!(main().starts_with(&first_id.to_string()), "{}", main());
    }
    // kept as it was, and with less in it: taken
    let (same, taken, said) = pushed(rooted(under_id), &first_id.to_string());
    assert!(taken, "{said}");
    let less = written(&tree(&[("100644", b"config", blob)]));
    let (kept, taken, said) = pushed(rooted(less), &same);
    assert!(taken, "{said}");
    assert!(main().starts_with(&kept), "{}", main());

    // the same through the native protocol, from the head that has `config` alone
    let client: Transport = super::super::native_led::client(&server, &token);
    let parent = ObjectId::from_hex(kept.as_bytes()).unwrap();
    let alone = tree(&[("100644", b"config", blob)]);
    let more = tree(&[("100644", b"config", blob), ("100644", b"hooks", other)]);
    let other_bytes = tree(&[("100644", b"config", other)]);
    for (request, under) in [("native-hooks", &more), ("native-bytes", &other_bytes)] {
        let next_root = tree(&[
            ("40000", b"ALT~1", id(ObjectKind::Tree, under)),
            ("100644", b"m", other),
        ]);
        let next = commit(id(ObjectKind::Tree, &next_root), Some(parent), request);
        let next_id = id(ObjectKind::Commit, &next);
        let objects = [
            (ContentKind::Tree, &under[..]),
            (ContentKind::Tree, &next_root[..]),
            (ContentKind::Commit, &next[..]),
        ];
        let payload = native(request, &objects, parent, next_id);
        assert!(
            !super::super::native_led::sent(&client, &payload),
            "{request}"
        );
        assert!(main().starts_with(&kept), "{}", main());
    }
    // one that keeps it and changes a file beside it is published
    let beside = tree(&[
        ("40000", b"ALT~1", id(ObjectKind::Tree, &alone)),
        ("100644", b"m", blob),
    ]);
    let next = commit(id(ObjectKind::Tree, &beside), Some(parent), "native keeps");
    let next_id = id(ObjectKind::Commit, &next);
    let objects = [
        (ContentKind::Tree, &beside[..]),
        (ContentKind::Commit, &next[..]),
    ];
    let payload = native("native-keeps", &objects, parent, next_id);
    assert!(super::super::native_led::sent(&client, &payload));
    assert!(main().starts_with(&next_id.to_string()), "{}", main());
}

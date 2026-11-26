//! A file or a symbolic link whose name `git fsck` refuses, kept where the
//! parent had it: it is kept only as it was. Other bytes, another mode or
//! another target under the name are refused, by a Git push or a native
//! publication.

use super::fsck_anew::{commit, id, native, planted, tree};
use super::*;
use alt_wire_http::native::Transport;

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_file_or_a_symbolic_link_whose_name_git_fsck_refuses_is_kept_only_as_it_was() {
    use alt_pack_segment::Kind;
    let f = Fixture::new();
    let (x, evil) = (b"x\n".as_slice(), b"gitdir: ../evil\n".as_slice());
    let (to_a, to_b) = (b"a".as_slice(), b"b".as_slice());
    let gitdir = tree(&[("100644", b".GIT", blob(x))]);
    let gitdir_id = id(ObjectKind::Tree, &gitdir);
    // `sub/.GIT` a file, `ALT~1` a file, `.ALT` a symbolic link to `a`
    let root_of = |sub: ObjectId, mode: &str, file: &[u8], link: &[u8], m: Option<&[u8]>| {
        let mut entries = vec![
            ("120000", b".ALT".as_slice(), blob(link)),
            (mode, b"ALT~1".as_slice(), blob(file)),
        ];
        entries.extend(m.map(|m| ("100644", b"m".as_slice(), blob(m))));
        entries.push(("40000", b"sub".as_slice(), sub));
        tree(&entries)
    };
    let root = root_of(gitdir_id, "100644", x, to_a, None);
    let root_id = id(ObjectKind::Tree, &root);
    let first = commit(root_id, None, "taken before");
    let first_id = id(ObjectKind::Commit, &first);
    let objects = [
        (Kind::Blob, x),
        (Kind::Blob, evil),
        (Kind::Blob, to_a),
        (Kind::Blob, to_b),
        (Kind::Tree, &gitdir[..]),
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
    let written = |kind: &str, data: &[u8]| {
        let name = format!("{kind}-{}", blake(data));
        std::fs::write(work.join(&name), data).unwrap();
        let written = git(&["hash-object", "-t", kind, "--literally", "-w", &name]);
        ObjectId::from_hex(written.as_bytes()).unwrap()
    };
    for data in [evil, to_b] {
        written("blob", data);
    }
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
    let main = || git(&["ls-remote", &url, "refs/heads/main"]);
    let refused = |at: ObjectId, word: &str| {
        format!("remote: error: object {at}: hasDot{word}: contains '.{word}'")
    };

    // `sub/.GIT` of other bytes, `ALT~1` made to run, `.ALT` to `b`: each
    // refused for the name, of the tree that holds it
    let evil_dir = written("tree", &tree(&[("100644", b".GIT", blob(evil))]));
    let changed = [
        (
            root_of(evil_dir, "100644", x, to_a, None),
            Some(evil_dir),
            "git",
        ),
        (root_of(gitdir_id, "100755", x, to_a, None), None, "alt"),
        (root_of(gitdir_id, "100644", x, to_b, None), None, "alt"),
    ];
    for (root, at, word) in changed {
        let root = written("tree", &root);
        let at = at.unwrap_or(root);
        let (_, taken, said) = pushed(root, &first_id.to_string());
        assert!(!taken, "{said}");
        assert!(said.contains(&refused(at, word)), "{said}");
        assert!(main().starts_with(&first_id.to_string()), "{}", main());
    }
    // kept as they were, with a file beside them: taken
    let kept_root = written("tree", &root_of(gitdir_id, "100644", x, to_a, Some(x)));
    let (kept, taken, said) = pushed(kept_root, &first_id.to_string());
    assert!(taken, "{said}");
    assert!(main().starts_with(&kept), "{}", main());

    // the same through the native protocol, from the head pushed
    let client: Transport = super::super::native_led::client(&server, &token);
    let parent = ObjectId::from_hex(kept.as_bytes()).unwrap();
    let evil_tree = tree(&[("100644", b".GIT", blob(evil))]);
    let changed = [
        (
            "native-bytes",
            root_of(evil_dir, "100644", x, to_a, Some(x)),
            true,
        ),
        (
            "native-mode",
            root_of(gitdir_id, "100755", x, to_a, Some(x)),
            false,
        ),
        (
            "native-target",
            root_of(gitdir_id, "100644", x, to_b, Some(x)),
            false,
        ),
        (
            "native-keeps",
            root_of(gitdir_id, "100644", x, to_a, Some(to_b)),
            false,
        ),
    ];
    for (request, root, below) in changed {
        let next = commit(id(ObjectKind::Tree, &root), Some(parent), request);
        let next_id = id(ObjectKind::Commit, &next);
        let mut objects = vec![
            (ContentKind::Tree, &root[..]),
            (ContentKind::Commit, &next[..]),
        ];
        if below {
            objects.insert(0, (ContentKind::Tree, &evil_tree[..]));
        }
        let payload = native(request, &objects, parent, next_id);
        let sent = super::super::native_led::sent(&client, &payload);
        let keeps = request == "native-keeps";
        assert_eq!(sent, keeps, "{request}");
        let head = if keeps {
            next_id.to_string()
        } else {
            kept.clone()
        };
        assert!(main().starts_with(&head), "{request}: {}", main());
    }
}

fn blob(data: &[u8]) -> ObjectId {
    id(ObjectKind::Blob, data)
}

fn blake(data: &[u8]) -> String {
    alt_hash::blake3::hash(data).to_hex().to_string()
}

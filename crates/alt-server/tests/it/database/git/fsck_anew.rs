//! A repository that already holds trees `git fsck` refuses, as a release
//! before this one took them: a push, Git or native, that keeps them where
//! they were is taken, and one that brings such a name, or such a
//! `.gitmodules`, to a place where the parent did not have it is refused.

use super::*;
use alt_wire_http::native::Transport;

/// a tree of `entries`, each a mode, a name and the id it names, in the order given
pub(super) fn tree(entries: &[(&str, &[u8], ObjectId)]) -> Vec<u8> {
    let mut data = Vec::new();
    for (mode, name, id) in entries {
        data.extend_from_slice(mode.as_bytes());
        data.push(b' ');
        data.extend_from_slice(name);
        data.push(0);
        data.extend_from_slice(id.as_bytes());
    }
    data
}

pub(super) fn commit(tree: ObjectId, parent: Option<ObjectId>, message: &str) -> Vec<u8> {
    let parent = parent.map(|p| format!("parent {p}\n")).unwrap_or_default();
    format!("tree {tree}\n{parent}author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\n{message}\n").into_bytes()
}

pub(super) fn id(kind: ObjectKind, data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, kind, data)
}

/// The objects as a live segment of the repository and `refs/heads/main` at
/// `head`, as a release that took any tree left them.
pub(super) fn planted(f: &Fixture, objects: &[(alt_pack_segment::Kind, &[u8])], head: ObjectId) {
    let none = alt_pack_segment::SegmentSet::default();
    let out = std::io::Cursor::new(Vec::new());
    let mut builder = alt_pack_segment::Builder::new(out, &none, objects.len() as u64).unwrap();
    for (kind, data) in objects {
        builder.add(*kind, data).unwrap();
    }
    let built = builder.finish().unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    let bytes = built.out.into_inner();
    std::fs::write(f.store.pack_path("repository", id).unwrap(), &bytes).unwrap();
    let mut client = f.client();
    client
        .execute(
            "INSERT INTO alt_pack_segments(repository,segment_id,sequence,objects,bytes,flags,state) VALUES('repository',$1,1,$2,$3,0,'live')",
            &[&id, &(built.objects as i64), &(bytes.len() as i64)],
        )
        .unwrap();
    client
        .execute(
            "INSERT INTO alt_refs(repository,name,oid) VALUES('repository','refs/heads/main',$1)",
            &[&head.to_string()],
        )
        .unwrap();
}

/// A native publication of `objects` that moves `refs/heads/main` from
/// `parent` to `head`.
pub(super) fn native(
    request: &str,
    objects: &[(ContentKind, &[u8])],
    parent: ObjectId,
    head: ObjectId,
) -> Payload {
    let mut payload = Payload::new(request, Some(parent));
    let entry = |(kind, data): &(ContentKind, &[u8])| {
        let digest = alt_hash::blake3::hash(data).to_hex().to_string();
        let git = match kind {
            ContentKind::Tree => ObjectKind::Tree,
            ContentKind::Commit => ObjectKind::Commit,
            _ => ObjectKind::Blob,
        };
        ContentEntry {
            kind: *kind,
            blake3: digest.clone(),
            bytes: data.len() as u64,
            git_oid: Some(id(git, data).to_string()),
            chunks: vec![Chunk {
                offset: 0,
                bytes: data.len() as u64,
                blake3: digest,
            }],
        }
    };
    payload.manifest.entries = objects.iter().map(entry).collect();
    payload.contents = objects.iter().map(|(_, data)| data.to_vec()).collect();
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload.publication.refs[0].new = Some(head.to_string());
    payload.commit = head;
    payload
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_is_refused_for_what_git_fsck_refuses_only_where_it_brings_it_anew() {
    use alt_pack_segment::Kind;
    let f = Fixture::new();
    let x = b"x\n".as_slice();
    let modules = b"[submodule \"a\"]\n\tpath = -x\n".as_slice();
    let (blob, bad_modules) = (id(ObjectKind::Blob, x), id(ObjectKind::Blob, modules));
    let below = tree(&[("100644", b"git~1", blob)]);
    let below_id = id(ObjectKind::Tree, &below);
    let root = tree(&[
        ("100644", b".alt", blob),
        ("100644", b".gitmodules", bad_modules),
        ("40000", b"d", below_id),
    ]);
    let root_id = id(ObjectKind::Tree, &root);
    let first = commit(root_id, None, "taken before");
    let first_id = id(ObjectKind::Commit, &first);
    let objects = [
        (Kind::Blob, x),
        (Kind::Blob, modules),
        (Kind::Tree, &below[..]),
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
    let written = |name: &str, data: &[u8]| {
        std::fs::write(work.join(name), data).unwrap();
        git(&["hash-object", "-t", "tree", "--literally", "-w", name])
    };
    let blob_of = |name: &str, data: &[u8]| {
        std::fs::write(work.join(name), data).unwrap();
        git(&["hash-object", "-w", name])
    };
    let pushed = |tree: &str, parent: &str| {
        let made = git(&["commit-tree", tree, "-p", parent, "-m", "next"]);
        let pushing = super::git(
            work,
            &token,
            &["push", &url, &format!("{made}:refs/heads/main")],
        );
        (
            made,
            pushing.status.success(),
            String::from_utf8_lossy(&pushing.stderr).into_owned(),
        )
    };
    let new = blob_of("new", b"new\n");
    let new = ObjectId::from_hex(new.as_bytes()).unwrap();

    // the names and the `.gitmodules` stay where they were, and a file comes beside them
    let kept_below = tree(&[("100644", b"git~1", blob), ("100644", b"n", new)]);
    let kept_below_id = id(ObjectKind::Tree, &kept_below);
    written("kept-below", &kept_below);
    let kept = tree(&[
        ("100644", b".alt", blob),
        ("100644", b".gitmodules", bad_modules),
        ("40000", b"d", kept_below_id),
    ]);
    let (second, taken, said) = pushed(&written("kept", &kept), &first_id.to_string());
    assert!(taken, "{said}");

    // `git~1` in a new tree, at a place that did not have it; a tree the
    // repository holds is not looked at again, as Git does not
    let elsewhere = tree(&[("100644", b"git~1", blob), ("100644", b"o", new)]);
    let elsewhere_id = written("elsewhere", &elsewhere);
    let elsewhere = ObjectId::from_hex(elsewhere_id.as_bytes()).unwrap();
    let moved = tree(&[("100644", b".alt", blob), ("40000", b"e", elsewhere)]);
    let (_, taken, said) = pushed(&written("moved", &moved), &second);
    assert!(!taken);
    let expected = format!("remote: error: object {elsewhere}: hasDotgit: contains '.git'");
    assert!(said.contains(&expected), "{said}");
    // another `.gitmodules` Git refuses
    let worse = blob_of("worse", b"[submodule \"a\"]\n\turl = -x\n");
    let worse = ObjectId::from_hex(worse.as_bytes()).unwrap();
    let changed = tree(&[("100644", b".alt", blob), ("100644", b".gitmodules", worse)]);
    let (_, taken, said) = pushed(&written("changed", &changed), &second);
    assert!(!taken);
    let expected =
        format!("remote: error: object {worse}: gitmodulesUrl: disallowed submodule url");
    assert!(said.contains(&expected), "{said}");
    let main = || git(&["ls-remote", &url, "refs/heads/main"]);
    assert!(main().starts_with(&second), "{}", main());

    // the same through the native protocol
    let client: Transport = super::super::native_led::client(&server, &token);
    let second = ObjectId::from_hex(second.as_bytes()).unwrap();
    let more = b"more\n".as_slice();
    let more_id = id(ObjectKind::Blob, more);
    let beside = tree(&[
        ("100644", b".alt", blob),
        ("100644", b".gitmodules", bad_modules),
        ("40000", b"d", kept_below_id),
        ("100644", b"m", more_id),
    ]);
    let third = commit(id(ObjectKind::Tree, &beside), Some(second), "native");
    let third_id = id(ObjectKind::Commit, &third);
    let objects = [
        (ContentKind::Blob, more),
        (ContentKind::Tree, &beside[..]),
        (ContentKind::Commit, &third[..]),
    ];
    let payload = native("native-keeps", &objects, second, third_id);
    assert!(super::super::native_led::sent(&client, &payload));
    let fresh = tree(&[("100644", b".alt", blob), ("100644", b"ALT~1", more_id)]);
    let fourth = commit(id(ObjectKind::Tree, &fresh), Some(third_id), "native anew");
    let fourth_id = id(ObjectKind::Commit, &fourth);
    let objects = [
        (ContentKind::Tree, &fresh[..]),
        (ContentKind::Commit, &fourth[..]),
    ];
    let payload = native("native-brings", &objects, third_id, fourth_id);
    assert!(!super::super::native_led::sent(&client, &payload));
    assert!(main().starts_with(&third_id.to_string()), "{}", main());
}

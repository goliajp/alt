//! Trees `git fsck` refuses are refused at the push, as a Git server that
//! checks what it receives says so; trees only written out of Git's order are
//! taken, kept byte for byte, and exported as they are.

use super::*;
use alt_wire_http::native::portable::ExportCommand;

/// a tree of `entries`, each a mode, a name and the id it names, in the order given
fn tree(entries: &[(&str, &[u8], &str)]) -> Vec<u8> {
    let mut data = Vec::new();
    for (mode, name, id) in entries {
        data.extend_from_slice(mode.as_bytes());
        data.push(b' ');
        data.extend_from_slice(name);
        data.push(0);
        let byte = |at: usize| u8::from_str_radix(&id[at * 2..at * 2 + 2], 16).unwrap();
        data.extend((0..20).map(byte));
    }
    data
}

/// Each of these trees, alone or below a directory, is refused with what Git
/// says of it, and no ref moves: `.git` as Git, NTFS and HFS+ read it, `..`,
/// `.alt` as `.git` is, and a name twice (Git itself sends no tree with an empty name). A tree out
/// of order, and modes with zeros before them, are taken: a clone has the bytes
/// that were pushed, and a portable export of the repository holds them as
/// they are.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn trees_git_fsck_refuses_are_refused_and_trees_out_of_order_are_kept_as_they_are() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let url = format!("{}/git/repository", server.url);
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    std::fs::write(work.join("file"), "file\n").unwrap();
    let blob = git(&["hash-object", "-w", "file"]);
    let written = |name: &str, data: &[u8]| {
        std::fs::write(work.join(name), data).unwrap();
        git(&["hash-object", "-t", "tree", "--literally", "-w", name])
    };
    let committed = |tree: &str| git(&["commit-tree", tree, "-m", "tree"]);
    let plain = written("plain", &tree(&[("100644", b"file", &blob)]));
    git(&[
        "push",
        "-q",
        &url,
        &format!("{}:refs/heads/main", committed(&plain)),
    ]);
    let refs = || git(&["ls-remote", "--refs", &url]);
    let before = refs();

    let refused: &[(&[u8], &str)] = &[
        (b".GIT", "hasDotgit: contains '.git'"),
        (b"git~1", "hasDotgit: contains '.git'"),
        (".g\u{200c}it".as_bytes(), "hasDotgit: contains '.git'"),
        (b"..", "hasDotdot: contains '..'"),
        (b".alt", "hasDotalt: contains '.alt'"),
        (b"ALT~1", "hasDotalt: contains '.alt'"),
        (".A\u{feff}lt".as_bytes(), "hasDotalt: contains '.alt'"),
    ];
    for (at, (name, said)) in refused.iter().enumerate() {
        let bad = written(&format!("bad{at}"), &tree(&[("100644", name, &blob)]));
        // the same tree as the root, and below a directory
        let above = written(&format!("above{at}"), &tree(&[("40000", b"d", &bad)]));
        for root in [&bad, &above] {
            let pushing = super::git(
                work,
                &token,
                &["push", &url, &format!("{}:refs/heads/x", committed(root))],
            );
            let stderr = String::from_utf8_lossy(&pushing.stderr);
            assert!(!pushing.status.success(), "{name:?}");
            let expected = format!("remote: error: object {bad}: {said}");
            assert!(stderr.contains(&expected), "{name:?}: {stderr}");
            assert_eq!(refs(), before);
        }
    }
    let twice = written(
        "twice",
        &tree(&[("100644", b"a", &blob), ("100644", b"a", &blob)]),
    );
    let pushing = super::git(
        work,
        &token,
        &["push", &url, &format!("{}:refs/heads/x", committed(&twice))],
    );
    let stderr = String::from_utf8_lossy(&pushing.stderr);
    let expected =
        format!("remote: error: object {twice}: duplicateEntries: contains duplicate file entries");
    assert!(
        !pushing.status.success() && stderr.contains(&expected),
        "{stderr}"
    );
    assert_eq!(refs(), before);

    // out of order, and modes with zeros before them
    let unordered = tree(&[("100644", b"b", &blob), ("100644", b"a", &blob)]);
    let unordered_id = written("unordered", &unordered);
    let padded = tree(&[("040000", b"d", &unordered_id), ("0100644", b"f", &blob)]);
    let padded_id = written("padded", &padded);
    let top = committed(&padded_id);
    git(&["push", "-q", &url, &format!("{top}:refs/heads/kept")]);
    ok(temp.path(), &token, &["clone", "-q", &url, "c"]);
    let cloned = temp.path().join("c");
    for (id, data) in [(&unordered_id, &unordered), (&padded_id, &padded)] {
        assert_eq!(&ok(&cloned, &token, &["cat-file", "tree", id]), data);
    }

    let scope = Scope {
        installation_id: f.spec.installation_id.clone(),
        repository_instance_id: "repository".into(),
        recovery_epoch: 1,
    };
    let key = RequestKey {
        scope,
        actor_id: "owner".into(),
        request_id: "export-as-pushed".into(),
    };
    let package = (super::super::portable::bounds())
        .verification(limits(), task_limits())
        .unwrap()
        .package;
    let client = super::super::portable::client(&server, &token);
    let source_key = SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
    let exported = client.portable_export(&key, ExportCommand::Prepare, &source_key, package);
    let alt_portable::http::ExportState::Available { envelope, .. } = exported.unwrap() else {
        panic!("prepared")
    };
    for (id, data) in [(&unordered_id, &unordered), (&padded_id, &padded)] {
        let object = envelope
            .catalog
            .objects
            .iter()
            .find(|object| &object.oid == id);
        assert!(object.is_some(), "{id}");
        let blob = alt_portable::Blob {
            blake3: alt_hash::blake3::hash(data).to_hex().to_string(),
            bytes: data.len() as u64,
        };
        let mut raw = Vec::new();
        client
            .portable_download(&key.scope, &key.request_id, &blob, &mut raw)
            .unwrap();
        assert_eq!(&raw, data);
    }
}

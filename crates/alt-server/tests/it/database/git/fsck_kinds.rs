//! A name `git fsck` refuses that the parent had as a file is brought anew
//! where it comes as a directory, and one the parent had as a directory where
//! it comes as a file: what such a directory holds would otherwise be checked
//! against nothing.

use super::fsck_anew::{commit, id, native, planted, tree};
use super::*;
use alt_wire_http::native::Transport;

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_name_git_fsck_refuses_is_brought_anew_where_a_file_becomes_a_directory_or_a_directory_a_file()
{
    use alt_pack_segment::Kind;
    let f = Fixture::new();
    let x = b"x\n".as_slice();
    let blob = id(ObjectKind::Blob, x);
    let under = tree(&[("100644", b"a", blob)]);
    let under_id = id(ObjectKind::Tree, &under);
    // `.alt` a file, `ALT~1` a directory
    let root = tree(&[("100644", b".alt", blob), ("40000", b"ALT~1", under_id)]);
    let root_id = id(ObjectKind::Tree, &root);
    let first = commit(root_id, None, "taken before");
    let first_id = id(ObjectKind::Commit, &first);
    let objects = [
        (Kind::Blob, x),
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
    // the files are beside those of the bare repository
    let written = |name: &str, data: &[u8]| {
        let name = format!("tree-{name}");
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
    let refused =
        |root: ObjectId| format!("remote: error: object {root}: hasDotalt: contains '.alt'");

    // the same types again, a file beside them: taken
    let more = tree(&[
        ("100644", b".alt", blob),
        ("40000", b"ALT~1", under_id),
        ("100644", b"m", blob),
    ]);
    let (second, taken, said) = pushed(written("more", &more), &first_id.to_string());
    assert!(taken, "{said}");
    // the file `.alt` as a directory of what a repository holds
    let config = tree(&[("100644", b"config", blob), ("100644", b"HEAD", blob)]);
    let config_id = written("config", &config);
    let directory = written("directory", &tree(&[("40000", b".alt", config_id)]));
    let (_, taken, said) = pushed(directory, &second);
    assert!(!taken);
    assert!(said.contains(&refused(directory)), "{said}");
    // the directory `ALT~1` as a file
    let file = written("file", &tree(&[("100644", b"ALT~1", blob)]));
    let (_, taken, said) = pushed(file, &second);
    assert!(!taken);
    assert!(said.contains(&refused(file)), "{said}");
    let main = || git(&["ls-remote", &url, "refs/heads/main"]);
    let head = main();
    assert!(head.starts_with(&second), "{head}");

    // the same through the native protocol, from the head the push above left
    let client: Transport = super::super::native_led::client(&server, &token);
    let parent = ObjectId::from_hex(second.as_bytes()).unwrap();
    for (request, root) in [
        (
            "native-directory",
            tree(&[("40000", b".alt", id(ObjectKind::Tree, &config))]),
        ),
        ("native-file", tree(&[("100644", b"ALT~1", blob)])),
    ] {
        let next = commit(id(ObjectKind::Tree, &root), Some(parent), request);
        let next_id = id(ObjectKind::Commit, &next);
        let mut objects = vec![
            (ContentKind::Tree, &root[..]),
            (ContentKind::Commit, &next[..]),
        ];
        if request == "native-directory" {
            objects.insert(0, (ContentKind::Tree, &config[..]));
        }
        let payload = native(request, &objects, parent, next_id);
        assert!(
            !super::super::native_led::sent(&client, &payload),
            "{request}"
        );
        assert_eq!(main(), head);
    }
    // and one that keeps the types is published
    let kept = tree(&[
        ("100644", b".alt", blob),
        ("40000", b"ALT~1", under_id),
        ("100644", b"n", id(ObjectKind::Blob, b"n\n")),
    ]);
    let next = commit(id(ObjectKind::Tree, &kept), Some(parent), "native keeps");
    let next_id = id(ObjectKind::Commit, &next);
    let objects = [
        (ContentKind::Blob, b"n\n".as_slice()),
        (ContentKind::Tree, &kept[..]),
        (ContentKind::Commit, &next[..]),
    ];
    let payload = native("native-keeps", &objects, parent, next_id);
    assert!(super::super::native_led::sent(&client, &payload));
    assert!(main().starts_with(&next_id.to_string()), "{}", main());
}

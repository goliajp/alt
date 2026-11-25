//! A pack that holds an object many times over, each a delta of the one
//! before by its id: the copies lie no deeper than the first of them.

use std::fs;
use std::process::Command;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_pack_limits::git;

fn size(out: &mut Vec<u8>, mut value: usize) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

/// An entry of type `kind` of `data`, a delta of `base` where it names one.
fn entry(kind: u8, data: &[u8], base: Option<&ObjectId>) -> Vec<u8> {
    let mut left = data.len() >> 4;
    let mut out = vec![(kind << 4) | (data.len() & 15) as u8];
    while left > 0 {
        *out.last_mut().unwrap() |= 128;
        out.push((left & 127) as u8);
        left >>= 7;
    }
    if let Some(base) = base {
        out.extend_from_slice(base.as_bytes());
    }
    out.extend(alt_deflate::compress(data, 1, alt_deflate::Format::Zlib));
    out
}

/// A delta that makes `target` of `base` out of nothing of it.
fn delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    size(&mut out, base.len());
    size(&mut out, target.len());
    for chunk in target.chunks(127) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out
}

fn pkt(line: &str) -> Vec<u8> {
    format!("{:04x}{line}", line.len() + 4).into_bytes()
}

#[test]
fn a_pack_of_an_object_made_of_itself_more_times_than_a_chain_may_be_deep_is_taken() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    fs::create_dir(&origin).unwrap();
    ok(alt(&origin, &["init", "."]));
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);

    let id = |kind, data: &[u8]| ObjectId::hash_object(HashAlgo::Sha1, kind, data);
    let (base, file) = (b"a base\n".as_slice(), b"the file\n".as_slice());
    let (base_id, file_id) = (id(ObjectKind::Blob, base), id(ObjectKind::Blob, file));
    let mut tree = b"100644 file\0".to_vec();
    tree.extend_from_slice(file_id.as_bytes());
    let tree_id = id(ObjectKind::Tree, &tree);
    let commit = format!(
        "tree {tree_id}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\ncopies\n"
    );
    let commit_id = id(ObjectKind::Commit, commit.as_bytes());
    let mut entries = vec![
        entry(3, base, None),
        entry(7, &delta(base, file), Some(&base_id)),
    ];
    // more copies than the deepest chain a pack may hold
    let again = entry(7, &delta(file, file), Some(&file_id));
    entries.extend(std::iter::repeat_n(again, 5000));
    entries.push(entry(2, &tree, None));
    entries.push(entry(1, commit.as_bytes(), None));
    let mut pack = b"PACK".to_vec();
    pack.extend(2u32.to_be_bytes());
    pack.extend((entries.len() as u32).to_be_bytes());
    pack.extend(entries.concat());
    let mut sha1 = alt_hash::Sha1::new();
    sha1.update(&pack);
    pack.extend(sha1.finalize());

    let zero = "0".repeat(40);
    let mut body = pkt(&format!(
        "{zero} {commit_id} refs/heads/main\0report-status"
    ));
    body.extend(b"0000");
    body.extend(pack);
    let request = tmp.path().join("request");
    fs::write(&request, &body).unwrap();
    let out = Command::new("curl")
        .args(["-sS", "--max-time", "60", "-H"])
        .arg("Content-Type: application/x-git-receive-pack-request")
        .arg("--data-binary")
        .arg(format!("@{}", request.display()))
        .arg(format!("http://{}/git-receive-pack", server.addr))
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("unpack ok"), "{report}");
    assert!(report.contains("ok refs/heads/main"), "{report}");

    let clone = tmp.path().join("clone");
    let url = format!("http://{}/", server.addr);
    let cloned = git(tmp.path(), &["clone", "-q", &url, clone.to_str().unwrap()]);
    assert!(cloned.status.success(), "{cloned:?}");
    assert_eq!(fs::read(clone.join("file")).unwrap(), file);
    ok(alt(&origin, &["fsck"]));
}

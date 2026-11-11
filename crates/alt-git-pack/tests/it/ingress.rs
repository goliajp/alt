use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_pack::*;
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
};
fn limits() -> IngressLimits {
    IngressLimits {
        wire_bytes: 1_000_000,
        objects: 100,
        object_bytes: 100_000,
        inflated_bytes: 1_000_000,
        resolved_bytes: 1_000_000,
        delta_depth: 20,
        steps: 10000,
    }
}
fn finish(mut bytes: Vec<u8>) -> Vec<u8> {
    let digest = alt_hash::sha1(&bytes);
    bytes.extend_from_slice(&digest);
    bytes
}
fn pack_header(n: u32) -> Vec<u8> {
    let mut b = b"PACK".to_vec();
    b.extend_from_slice(&2u32.to_be_bytes());
    b.extend_from_slice(&n.to_be_bytes());
    b
}
fn entry_header(kind: u8, mut size: u64) -> Vec<u8> {
    let mut b = vec![(kind << 4) | (size as u8 & 15)];
    size >>= 4;
    while size > 0 {
        let last = b.len() - 1;
        b[last] |= 128;
        b.push(size as u8 & 127);
        size >>= 7;
    }
    b
}
fn zlib(bytes: &[u8]) -> Vec<u8> {
    alt_deflate::compress(bytes, 6, alt_deflate::Format::Zlib)
}
fn plain(data: &[u8]) -> Vec<u8> {
    let mut b = pack_header(1);
    b.extend(entry_header(3, data.len() as u64));
    b.extend(zlib(data));
    finish(b)
}
#[test]
fn plain_and_empty_packs_preserve_exact_object_identity() {
    let raw = plain(b"original bytes");
    let p = ingest_pack(
        &mut Cursor::new(raw),
        HashAlgo::Sha1,
        limits(),
        &mut NoBases,
    )
    .unwrap();
    assert_eq!(p.objects.len(), 1);
    assert_eq!(p.objects[0].data.as_slice(), b"original bytes");
    assert_eq!(
        p.objects[0].oid,
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"original bytes")
    );
    assert!(
        ingest_pack(
            &mut Cursor::new(finish(pack_header(0))),
            HashAlgo::Sha1,
            limits(),
            &mut NoBases
        )
        .unwrap()
        .objects
        .is_empty()
    );
}
#[test]
fn malformed_headers_checksums_counts_and_trailing_bytes_are_rejected() {
    let mut cases = vec![vec![], b"PACK".to_vec()];
    let mut wrong = plain(b"x");
    wrong[14] ^= 1;
    cases.push(wrong);
    let mut wrong = pack_header(u32::MAX);
    wrong.extend([0x30, 0]);
    cases.push(finish(wrong));
    let mut wrong = pack_header(1);
    wrong.extend([0xb0; 20]);
    cases.push(finish(wrong));
    let mut wrong = pack_header(1);
    wrong.extend([0x50, 0]);
    cases.push(finish(wrong));
    let mut wrong = pack_header(1);
    wrong.push(0x60);
    wrong.extend([0xff; 20]);
    cases.push(finish(wrong));
    let mut wrong = pack_header(0);
    wrong.extend(b"trailing");
    cases.push(finish(wrong));
    for raw in cases {
        assert!(
            ingest_pack(
                &mut Cursor::new(raw),
                HashAlgo::Sha1,
                limits(),
                &mut NoBases
            )
            .is_err()
        );
    }
}
#[test]
fn compressed_expansion_is_bounded_before_allocation_or_output() {
    let mut raw = pack_header(1);
    raw.push(0x31);
    raw.extend(zlib(&vec![0; 100_000]));
    assert!(
        ingest_pack(
            &mut Cursor::new(finish(raw)),
            HashAlgo::Sha1,
            limits(),
            &mut NoBases
        )
        .is_err()
    );
    for mode in ["wire", "entry", "inflated", "resolved", "steps"] {
        let mut small = limits();
        match mode {
            "wire" => small.wire_bytes = 12,
            "entry" => small.object_bytes = 1,
            "inflated" => small.inflated_bytes = 1,
            "resolved" => small.resolved_bytes = 1,
            "steps" => small.steps = 1,
            _ => unreachable!(),
        }
        assert!(
            ingest_pack(
                &mut Cursor::new(plain(b"more than one")),
                HashAlgo::Sha1,
                small,
                &mut NoBases
            )
            .is_err(),
            "{mode}"
        );
    }
}
#[derive(Default)]
struct Bases {
    objects: BTreeMap<ObjectId, (ObjectKind, Vec<u8>)>,
    opens: usize,
}
impl BaseSource for Bases {
    fn open(&mut self, id: ObjectId) -> Result<Option<BaseReader<'_>>, alt_git_pack::PackError> {
        self.opens += 1;
        Ok(self.objects.get(&id).map(|(kind, bytes)| BaseReader {
            kind: *kind,
            bytes: bytes.len() as u64,
            reader: Box::new(Cursor::new(bytes)),
        }))
    }
}
#[test]
fn thin_pack_requires_scoped_base_and_checks_base_bytes_and_delta_limits() {
    let base = b"hello world";
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, base);
    let delta = b"\x0b\x09\x91\x06\x05\x01 \x90\x03";
    let mut raw = pack_header(1);
    raw.extend(entry_header(7, delta.len() as u64));
    raw.extend(oid.as_bytes());
    raw.extend(zlib(delta));
    let raw = finish(raw);
    assert!(
        ingest_pack(
            &mut Cursor::new(&raw),
            HashAlgo::Sha1,
            limits(),
            &mut NoBases
        )
        .is_err()
    );
    let mut bases = Bases {
        objects: BTreeMap::from([(oid, (ObjectKind::Blob, base.to_vec()))]),
        opens: 0,
    };
    let p = ingest_pack(&mut Cursor::new(&raw), HashAlgo::Sha1, limits(), &mut bases).unwrap();
    assert_eq!(p.objects[0].data.as_slice(), b"world hel");
    assert_eq!(bases.opens, 1);
    for mode in ["depth", "size", "total", "count"] {
        let mut small = limits();
        match mode {
            "depth" => small.delta_depth = 0,
            "size" => small.object_bytes = 5,
            "total" => small.resolved_bytes = 15,
            "count" => small.objects = 1,
            _ => unreachable!(),
        }
        assert!(
            ingest_pack(&mut Cursor::new(&raw), HashAlgo::Sha1, small, &mut bases).is_err(),
            "{mode}"
        );
    }
    bases.objects.get_mut(&oid).unwrap().1[0] = b'X';
    assert!(ingest_pack(&mut Cursor::new(&raw), HashAlgo::Sha1, limits(), &mut bases).is_err());
    // A small delta program must not amplify a tiny declared target.
    let mut raw = pack_header(1);
    let delta = b"\x0b\x01\x90\x0b";
    raw.extend(entry_header(7, delta.len() as u64));
    raw.extend(oid.as_bytes());
    raw.extend(zlib(delta));
    bases.objects.get_mut(&oid).unwrap().1 = base.to_vec();
    assert!(
        ingest_pack(
            &mut Cursor::new(finish(raw)),
            HashAlgo::Sha1,
            limits(),
            &mut bases
        )
        .is_err()
    );
}
fn ofs(mut distance: usize) -> Vec<u8> {
    let mut v = vec![(distance & 127) as u8];
    while distance >> 7 > 0 {
        distance = (distance >> 7) - 1;
        v.push(128 | (distance & 127) as u8);
    }
    v.reverse();
    v
}
#[test]
fn ofs_delta_depth_and_entry_offsets_are_checked() {
    let mut raw = pack_header(3);
    raw.extend(entry_header(3, 1));
    raw.extend(zlib(b"a"));
    let second = raw.len();
    let delta = b"\x01\x01\x01b";
    raw.extend(entry_header(6, delta.len() as u64));
    raw.extend(ofs(second - 12));
    raw.extend(zlib(delta));
    let third = raw.len();
    let delta = b"\x01\x01\x01c";
    raw.extend(entry_header(6, delta.len() as u64));
    raw.extend(ofs(third - second));
    raw.extend(zlib(delta));
    let raw = finish(raw);
    let p = ingest_pack(
        &mut Cursor::new(&raw),
        HashAlgo::Sha1,
        limits(),
        &mut NoBases,
    )
    .unwrap();
    assert_eq!(p.objects[2].data.as_slice(), b"c");
    let mut small = limits();
    small.delta_depth = 1;
    assert!(ingest_pack(&mut Cursor::new(&raw), HashAlgo::Sha1, small, &mut NoBases).is_err());
}

#[test]
fn real_git_thin_pack_resolves_only_authorized_published_bases() {
    use std::process::{Command, Stdio};
    let dir = alt_os::temp::tempdir().unwrap();
    let git = |args: &[&str], input: Option<&[u8]>| {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child.stdin.as_mut().unwrap().write_all(input).unwrap();
        }
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    git(&["init", "-q", "--initial-branch=main"], None);
    git(&["config", "user.name", "Original"], None);
    git(&["config", "user.email", "original@example.test"], None);
    let content = (0..2000)
        .map(|i| format!("independent source line {i:04}: original content\n"))
        .collect::<String>();
    std::fs::write(dir.path().join("source.txt"), &content).unwrap();
    git(&["add", "source.txt"], None);
    git(&["commit", "-qm", "base"], None);
    let mut changed = content.clone();
    changed.push_str("reviewed incremental change\n");
    std::fs::write(dir.path().join("source.txt"), changed).unwrap();
    git(&["add", "source.txt"], None);
    git(&["commit", "-qm", "change"], None);
    let mut bases = Bases::default();
    let inventory = git(&["rev-list", "--objects", "HEAD~1"], None);
    for line in std::str::from_utf8(&inventory).unwrap().lines() {
        let id = line.split(' ').next().unwrap();
        let kind = git(&["cat-file", "-t", id], None);
        let kind = std::str::from_utf8(&kind).unwrap().trim();
        let bytes = git(&["cat-file", kind, id], None);
        bases
            .objects
            .insert(id.parse().unwrap(), (kind.parse().unwrap(), bytes));
    }
    let pack = git(
        &[
            "pack-objects",
            "--stdout",
            "--revs",
            "--thin",
            "--window=10",
            "--depth=10",
        ],
        Some(b"HEAD\n^HEAD~1\n"),
    );
    assert!(
        ingest_pack(
            &mut Cursor::new(&pack),
            HashAlgo::Sha1,
            limits(),
            &mut NoBases
        )
        .is_err(),
        "fixture must actually contain an external delta base"
    );
    let decoded = ingest_pack(
        &mut Cursor::new(&pack),
        HashAlgo::Sha1,
        limits(),
        &mut bases,
    )
    .unwrap();
    assert!(bases.opens > 0);
    for object in decoded.objects {
        let original = git(
            &[
                "cat-file",
                &object.kind.to_string(),
                &object.oid.to_string(),
            ],
            None,
        );
        assert_eq!(object.data.as_slice(), original);
    }
}

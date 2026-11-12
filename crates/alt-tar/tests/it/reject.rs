use crate::{header, member, octal, read_files, seal, write};
use alt_tar::{Archive, Error, MAX_EXTENSION, MAX_PATH};
use std::io::Read;

fn finished(mut archive: Vec<u8>) -> Vec<u8> {
    archive.extend_from_slice(&[0; 1024]);
    archive
}

fn with_extension(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut archive = member(b"extension", kind, payload);
    archive.extend(member(b"plain", b'0', b"data"));
    finished(archive)
}

fn pax(key: &str, value: &str) -> Vec<u8> {
    let body = format!(" {key}={value}\n");
    let mut length = body.len() + 1;
    while (length.to_string().len() + body.len()) != length {
        length += 1;
    }
    format!("{length}{body}").into_bytes()
}

#[test]
fn plain_and_extended_paths_are_accepted() {
    let archive = finished(member(b"dir/./file", b'0', b"data"));
    assert_eq!(read_files(&archive).unwrap()[0].0, b"dir/./file");
    let archive = with_extension(b'L', b"long/name\0");
    assert_eq!(read_files(&archive).unwrap()[0].0, b"long/name");
    let archive = with_extension(b'x', &pax("path", "pax/name"));
    assert_eq!(read_files(&archive).unwrap()[0].0, b"pax/name");
    let archive = with_extension(b'g', &pax("comment", "ignored"));
    assert_eq!(read_files(&archive).unwrap()[0].0, b"plain");
}

#[test]
fn paths_that_escape_the_root_are_rejected() {
    for name in ["../a", "a/../b", "a/..", "..", "/etc/passwd", ""] {
        let archive = finished(member(name.as_bytes(), b'0', b"data"));
        assert!(matches!(read_files(&archive), Err(Error::Path)), "{name}");
        let long = format!("{name}\0");
        let archive = with_extension(b'L', long.as_bytes());
        assert!(matches!(read_files(&archive), Err(Error::Path)), "{name}");
        let archive = with_extension(b'x', &pax("path", name));
        assert!(matches!(read_files(&archive), Err(Error::Path)), "{name}");
    }
    let archive = with_extension(b'x', &pax("path", "a\0b"));
    assert!(matches!(read_files(&archive), Err(Error::Path)));
    let mut block = header(b"name", b'0', &octal(0));
    block[345..348].copy_from_slice(b"../");
    seal(&mut block);
    let archive = finished(block.to_vec());
    assert!(matches!(read_files(&archive), Err(Error::Path)));
}

#[test]
fn oversized_paths_and_extensions_are_rejected() {
    let mut name = vec![b'a'; MAX_PATH + 1];
    let archive = with_extension(b'L', &name);
    assert!(matches!(read_files(&archive), Err(Error::TooLong)));
    name.truncate(MAX_PATH);
    assert_eq!(read_files(&with_extension(b'L', &name)).unwrap()[0].0, name);
    for kind in *b"LKxg" {
        let block = header(b"extension", kind, &octal(MAX_EXTENSION as usize + 1));
        assert!(matches!(read_files(&block), Err(Error::TooLong)));
    }
}

#[test]
fn checksum_mismatch_is_rejected() {
    let mut archive = finished(member(b"file", b'0', b"data"));
    archive[0] ^= 1;
    assert!(matches!(read_files(&archive), Err(Error::Checksum)));
}

#[test]
fn malformed_and_overflowing_numbers_are_rejected() {
    for size in [&b"00000000008\0"[..], b"-0000000001\0", b"\0", b"1 2\0"] {
        let block = header(b"file", b'0', size);
        assert!(matches!(read_files(&block), Err(Error::Number)));
    }
    let mut wide = [0u8; 12];
    wide[0] = 0x80;
    wide[3] = 1;
    assert!(matches!(
        read_files(&header(b"file", b'0', &wide)),
        Err(Error::Number)
    ));
    let negative = [0xffu8; 12];
    assert!(matches!(
        read_files(&header(b"file", b'0', &negative)),
        Err(Error::Number)
    ));
    let mut block = header(b"file", b'0', &octal(0));
    block[148..156].copy_from_slice(b"9999999\0");
    assert!(matches!(read_files(&block), Err(Error::Number)));
    let archive = with_extension(b'x', &pax("size", "18446744073709551616"));
    assert!(matches!(read_files(&archive), Err(Error::Number)));
}

#[test]
fn base_256_and_pax_sizes_are_honoured() {
    let mut size = [0u8; 12];
    size[0] = 0x80;
    size[11] = 4;
    let mut archive = header(b"file", b'0', &size).to_vec();
    archive.extend_from_slice(b"data");
    archive.resize(1024, 0);
    assert_eq!(read_files(&finished(archive)).unwrap()[0].1, b"data");
    let mut archive = member(b"extension", b'x', &pax("size", "4"));
    archive.extend_from_slice(&header(b"file", b'0', &octal(0)));
    archive.extend_from_slice(b"data");
    archive.resize(archive.len().next_multiple_of(512), 0);
    assert_eq!(read_files(&finished(archive)).unwrap()[0].1, b"data");
}

#[test]
fn archive_shorter_than_a_member_claims_is_rejected() {
    let mut archive = header(b"file", b'0', &octal(1000)).to_vec();
    archive.extend_from_slice(&[7; 512]);
    let mut reader = Archive::new(&archive[..]);
    let mut entry = reader.next_entry().unwrap().unwrap();
    let error = entry.read_to_end(&mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    let mut reader = Archive::new(&archive[..]);
    let mut entry = reader.next_entry().unwrap().unwrap();
    let error = entry.copy_to(&mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    let mut reader = Archive::new(&archive[..]);
    reader.next_entry().unwrap().unwrap();
    assert!(matches!(reader.next_entry(), Err(Error::Truncated)));
}

#[test]
fn truncation_anywhere_is_rejected() {
    let whole = write(&[(b"file".to_vec(), vec![7; 700])]);
    for cut in [100, 511, 513, 1211, 1212, 1500] {
        let result = read_files(&whole[..cut]);
        let rejected = match result {
            Err(Error::Truncated) => true,
            Err(Error::Io(error)) => error.kind() == std::io::ErrorKind::UnexpectedEof,
            _ => false,
        };
        assert!(rejected, "cut at {cut}");
    }
    let dangling = member(b"extension", b'L', b"name\0");
    assert!(matches!(read_files(&dangling), Err(Error::Truncated)));
    let dangling = finished(member(b"extension", b'x', &pax("path", "name")));
    assert!(matches!(read_files(&dangling), Err(Error::Truncated)));
}

#[test]
fn malformed_and_repeated_extensions_are_rejected() {
    for record in [
        &b"12 path=name\n"[..],
        b"99 path=name\n",
        b"13 path=name ",
        b"9 pathnam\n",
        b"path=name\n",
        b"3 \n",
    ] {
        let archive = with_extension(b'x', record);
        assert!(matches!(read_files(&archive), Err(Error::Extension)));
    }
    let archive = with_extension(b'x', b"x path=name\n");
    assert!(matches!(read_files(&archive), Err(Error::Number)));
    for kind in *b"Lx" {
        let payload = if kind == b'L' {
            b"name\0".to_vec()
        } else {
            pax("path", "name")
        };
        let mut archive = member(b"extension", kind, &payload);
        archive.extend(with_extension(kind, &payload));
        assert!(matches!(read_files(&archive), Err(Error::Extension)));
    }
}

#[test]
fn sparse_members_are_rejected() {
    let archive = finished(member(b"file", b'S', b""));
    assert!(matches!(read_files(&archive), Err(Error::Unsupported)));
    let archive = with_extension(b'x', &pax("GNU.sparse.major", "1"));
    assert!(matches!(read_files(&archive), Err(Error::Unsupported)));
}

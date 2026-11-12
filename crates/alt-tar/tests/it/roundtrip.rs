use crate::{Members, read_files, write};
use alt_tar::{Archive, Builder, Error};
use std::io::Read;

fn sample() -> Members {
    [0usize, 1, 511, 512, 513, 65_536, 600_001]
        .into_iter()
        .enumerate()
        .map(|(index, size)| {
            let name = format!("dir{}/{index}", "n".repeat(index * 40));
            let data = (0..size).map(|i| (i * 31 + index) as u8).collect();
            (name.into_bytes(), data)
        })
        .collect()
}

#[test]
fn members_survive_a_round_trip() {
    let members = sample();
    let archive = write(&members);
    assert_eq!(archive.len() % 512, 0);
    assert_eq!(read_files(&archive).unwrap(), members);
}

#[test]
fn unread_data_is_skipped() {
    let members = sample();
    let bytes = write(&members);
    let mut archive = Archive::new(&bytes[..]);
    let mut paths = Vec::new();
    while let Some(mut entry) = archive.next_entry().unwrap() {
        let mut head = [0; 7];
        let taken = entry.read(&mut head).unwrap();
        assert_eq!(taken as u64, entry.size().min(7));
        paths.push(entry.path().to_vec());
    }
    let expected: Vec<_> = members.into_iter().map(|(path, _)| path).collect();
    assert_eq!(paths, expected);
}

#[test]
fn copy_to_moves_the_whole_member() {
    let members = sample();
    let bytes = write(&members);
    let mut archive = Archive::new(&bytes[..]);
    for (_, data) in &members {
        let mut entry = archive.next_entry().unwrap().unwrap();
        let mut copy = Vec::new();
        assert_eq!(entry.copy_to(&mut copy).unwrap(), data.len() as u64);
        assert!(&copy == data);
        assert_eq!(entry.read(&mut [0; 8]).unwrap(), 0);
    }
    assert!(archive.next_entry().unwrap().is_none());
}

#[test]
fn stream_is_left_after_the_first_zero_block() {
    let mut bytes = write(&sample()[..2]);
    bytes.extend_from_slice(b"trailer");
    let mut archive = Archive::new(&bytes[..]);
    while archive.next_entry().unwrap().is_some() {}
    let mut rest = Vec::new();
    archive.into_inner().read_to_end(&mut rest).unwrap();
    assert_eq!(rest.len(), 512 + 7);
    assert!(rest.ends_with(b"trailer"));
}

#[test]
fn writer_rejects_data_shorter_or_longer_than_declared() {
    let mut builder = Builder::new(Vec::new());
    let short = builder.append_file(b"a", 0o600, 0, 5, &mut &b"1234"[..]);
    assert!(matches!(short, Err(Error::Size)));
    let mut builder = Builder::new(Vec::new());
    let long = builder.append_file(b"a", 0o600, 0, 5, &mut &b"123456"[..]);
    assert!(matches!(long, Err(Error::Size)));
}

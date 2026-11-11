use alt_os::{map::Mmap, temp};
use std::io::Write;

#[test]
fn mapping_equals_reading() {
    let mut file = temp::NamedTempFile::new().unwrap();
    let content: Vec<u8> = (0..200_003u32).map(|i| (i * 31 % 251) as u8).collect();
    file.write_all(&content).unwrap();
    // SAFETY: nothing else writes the file
    let map = unsafe { Mmap::map(file.as_file()).unwrap() };
    assert_eq!(map.len(), content.len());
    assert_eq!(&map[..], &content[..]);
    assert_eq!(&map[..], &std::fs::read(file.path()).unwrap()[..]);
}

#[test]
fn empty_file_maps_to_an_empty_slice() {
    let file = temp::tempfile().unwrap();
    // SAFETY: nothing else writes the file
    let map = unsafe { Mmap::map(&file).unwrap() };
    assert!(map.is_empty());
    assert_eq!(&map[..], b"");
}

#[test]
fn mapping_outlives_the_file_and_crosses_threads() {
    let mut file = temp::tempfile().unwrap();
    file.write_all(b"still here").unwrap();
    // SAFETY: nothing else writes the file
    let map = unsafe { Mmap::map(&file).unwrap() };
    drop(file);
    let map = std::sync::Arc::new(map);
    let other = map.clone();
    std::thread::spawn(move || assert_eq!(&other[..], b"still here"))
        .join()
        .unwrap();
    assert_eq!(&map[..], b"still here");
}

#[test]
fn directory_cannot_be_mapped() {
    let directory = temp::tempdir().unwrap();
    let file = std::fs::File::open(directory.path()).unwrap();
    // SAFETY: the call is expected to fail without creating a mapping
    assert!(unsafe { Mmap::map(&file) }.is_err());
}

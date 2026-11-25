use alt_os::{holes::data_from, temp};
use std::os::unix::fs::FileExt;

const MIB: u64 = 1 << 20;

fn a_file_without_holes_is_data_to_its_end() {
    let file = temp::tempfile().unwrap();
    assert_eq!(data_from(&file, 0).unwrap(), None);
    file.write_all_at(&[7; 10_000], 0).unwrap();
    assert_eq!(data_from(&file, 0).unwrap(), Some((0, 10_000)));
    assert_eq!(data_from(&file, 9_999).unwrap(), Some((9_999, 10_000)));
    for past in [10_000, 10_001, u64::MAX] {
        assert_eq!(data_from(&file, past).unwrap(), None);
    }
}

fn a_hole_after_the_data_is_told_without_reading_it() {
    let file = temp::tempfile().unwrap();
    file.write_all_at(&[7; 10_000], 0).unwrap();
    file.set_len(10_000 + 1024 * MIB).unwrap();
    // the file system keeps whole blocks, of a size that is its own
    let (start, end) = data_from(&file, 0).unwrap().unwrap();
    assert!(start == 0 && (10_000..MIB).contains(&end), "{end}");
    assert_eq!(data_from(&file, end).unwrap(), None);
    assert_eq!(data_from(&file, 512 * MIB).unwrap(), None);
}

fn data_after_a_hole_is_found_from_within_the_hole() {
    let file = temp::tempfile().unwrap();
    file.write_all_at(&[7; 10_000], 0).unwrap();
    file.write_all_at(&[8; 10_000], 256 * MIB).unwrap();
    let len = 256 * MIB + 10_000;
    let (_, first) = data_from(&file, 0).unwrap().unwrap();
    assert!(first < MIB, "{first}");
    for from in [first, 100 * MIB, 256 * MIB - 1] {
        let (start, end) = data_from(&file, from).unwrap().unwrap();
        assert!((255 * MIB..=256 * MIB).contains(&start), "{start}");
        assert_eq!(end, len);
    }
    assert_eq!(
        data_from(&file, 256 * MIB + 5).unwrap(),
        Some((256 * MIB + 5, len))
    );
}

tests![
    a_file_without_holes_is_data_to_its_end,
    a_hole_after_the_data_is_told_without_reading_it,
    data_after_a_hole_is_found_from_within_the_hole,
];

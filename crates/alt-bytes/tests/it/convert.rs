use std::{
    collections::{BTreeSet, HashSet},
    ffi::OsStr,
    path::Path,
};

use alt_bytes::{BStr, BString, ByteSlice};

#[test]
fn to_str_accepts_only_valid_utf8() {
    assert_eq!(b"".to_str(), Ok(""));
    assert_eq!("héllo".as_bytes().to_str(), Ok("héllo"));
    let error = b"ab\xffcd".to_str().unwrap_err();
    assert_eq!(error.valid_up_to(), 2);
    assert!(b"\xe2\x82".to_str().is_err());
}

#[test]
fn to_str_lossy_borrows_valid_input_and_replaces_invalid_sequences() {
    assert!(matches!(
        b"plain".to_str_lossy(),
        std::borrow::Cow::Borrowed("plain")
    ));
    assert_eq!(b"a\xffb".to_str_lossy(), "a\u{fffd}b");
    assert_eq!(
        b"\xe2\x82z\xff\xff".to_str_lossy(),
        "\u{fffd}z\u{fffd}\u{fffd}"
    );
    assert_eq!(b"".to_str_lossy(), "");
}

#[test]
fn paths_and_os_strings_keep_valid_utf8_unchanged() {
    assert_eq!(b"src/lib.rs".to_path(), Ok(Path::new("src/lib.rs")));
    assert_eq!(
        "dir/日本".as_bytes().to_os_str(),
        Ok(OsStr::new("dir/日本"))
    );
    assert_eq!(b"".to_path(), Ok(Path::new("")));
}

#[cfg(unix)]
#[test]
fn paths_and_os_strings_keep_arbitrary_bytes_on_unix() {
    use std::os::unix::ffi::OsStrExt;

    let name = b"caf\xe9/\xff.txt";
    assert_eq!(name.to_os_str().unwrap().as_bytes(), name);
    assert_eq!(name.to_path().unwrap().as_os_str().as_bytes(), name);
}

#[test]
fn as_bytes_and_as_bstr_view_the_same_bytes() {
    let bytes = b"a\xffb";
    assert_eq!(bytes.as_bytes(), bytes);
    assert_eq!(&bytes.as_bstr()[..], bytes);
    assert_eq!(bytes.as_bstr().len(), 3);
    assert_eq!(bytes.as_bstr(), b"a\xffb".as_bstr());
    assert_ne!(bytes.as_bstr(), b"a\xffc".as_bstr());
}

#[test]
fn borrowed_strings_convert_from_byte_literals() {
    let name: &BStr = b"main".into();
    assert_eq!(name, b"main".as_bstr());
    assert_eq!(name.to_str(), Ok("main"));
}

#[test]
fn owned_strings_convert_from_text_slices_and_vectors() {
    let from_text = BString::from("a/b");
    let from_slice = BString::from(&b"a/b"[..]);
    let from_vec = BString::from(b"a/b".to_vec());
    assert_eq!(from_text, from_slice);
    assert_eq!(from_slice, from_vec);
    assert_eq!(AsRef::<[u8]>::as_ref(&from_vec), b"a/b");
    assert_eq!(BString::default().len(), 0);
}

#[test]
fn owned_strings_compare_with_text_and_bytes() {
    let name = BString::from("refs/heads/main");
    let same: &[u8] = b"refs/heads/main";
    let longer: &[u8] = b"refs/heads/main\0";
    assert!(name == "refs/heads/main");
    assert!(name != "refs/heads/mai");
    assert!(name == *same);
    assert!(name == same);
    assert!(name != longer);
    assert!(BString::from(&b"\xff"[..]) != "\u{fffd}");
}

#[test]
fn owned_strings_are_mutable_through_the_vector() {
    let mut name = BString::from("a");
    name.push(b'/');
    name.extend_from_slice(b"b");
    assert_eq!(name, "a/b");
    name.clear();
    assert!(name.is_empty());
}

#[test]
fn owned_strings_order_and_hash_by_bytes() {
    let invalid = BString::from(&b"a\xff"[..]);
    let mut names = ["b", "a/b", "a", "a\u{e9}", ""].map(BString::from).to_vec();
    names.push(invalid.clone());
    let sorted: Vec<BString> = names
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut expected = ["", "a", "a/b", "a\u{e9}"].map(BString::from).to_vec();
    expected.extend([invalid, BString::from("b")]);
    assert_eq!(sorted, expected);

    let set: HashSet<BString> = names.into_iter().collect();
    assert!(set.contains(&BString::from(&b"a/b"[..])));
    assert!(!set.contains(&BString::from("a/c")));
}

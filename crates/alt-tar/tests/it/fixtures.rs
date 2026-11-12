use crate::{Members, fixture, read_files, write};

// the members the reference writer was given, in order; every one had mode
// 0600 and mtime 0 in a GNU header, paths over 100 bytes as long-name members
fn reference_members() -> Members {
    let names = [
        "database.dump".to_owned(),
        "empty".to_owned(),
        format!("bytes/{}/{}", "a".repeat(29), "0".repeat(64)),
        format!("bytes/{}/{}", "b".repeat(30), "1".repeat(64)),
        format!("bytes/{}/{}", "c".repeat(128), "2".repeat(64)),
        format!("packs/x{}/{}", "\u{e9}".repeat(47), "3".repeat(32)),
    ];
    let sizes = [1300, 0, 512, 513, 5, 1];
    names
        .into_iter()
        .zip(sizes)
        .map(|(name, size)| {
            let data = (0..size).map(|i| (i * 7 + name.len()) as u8).collect();
            (name.into_bytes(), data)
        })
        .collect()
}

fn tree(long: bool) -> Members {
    let mut names = vec![
        "short.txt".to_owned(),
        format!("{}/{}", "d".repeat(60), "m".repeat(90)),
    ];
    if long {
        names.push(format!("{}/{}", "l".repeat(120), "n".repeat(200)));
    }
    names
        .into_iter()
        .map(|name| (name.clone().into_bytes(), name.into_bytes()))
        .collect()
}

#[test]
fn reads_archive_written_by_the_previous_writer() {
    let members = read_files(&fixture("tar-crate-gnu.tar")).unwrap();
    assert_eq!(members, reference_members());
}

#[test]
fn writes_the_same_bytes_as_the_previous_writer() {
    assert!(write(&reference_members()) == fixture("tar-crate-gnu.tar"));
}

#[test]
fn reads_gnu_tar_archives() {
    assert_eq!(read_files(&fixture("gnutar-gnu.tar")).unwrap(), tree(true));
    assert_eq!(read_files(&fixture("gnutar-pax.tar")).unwrap(), tree(true));
    assert_eq!(
        read_files(&fixture("gnutar-ustar.tar")).unwrap(),
        tree(false)
    );
}

#[test]
fn reads_bsdtar_archives() {
    assert_eq!(read_files(&fixture("bsdtar-gnu.tar")).unwrap(), tree(true));
    assert_eq!(read_files(&fixture("bsdtar-pax.tar")).unwrap(), tree(true));
    assert_eq!(
        read_files(&fixture("bsdtar-ustar.tar")).unwrap(),
        tree(false)
    );
}

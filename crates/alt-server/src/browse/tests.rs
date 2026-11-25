use super::*;

fn directory(entries: usize) -> Tree {
    let mut raw = Vec::new();
    for n in 0..entries {
        raw.extend_from_slice(format!("100644 file-{n:05}\0").as_bytes());
        raw.extend_from_slice(&[7; 20]);
    }
    Tree::parse(&raw, HashAlgo::Sha1).unwrap()
}

#[test]
fn a_directory_is_listed_a_page_at_a_time_within_what_a_page_is_written_in() {
    let written = |entries: &[TreeEntry]| alt_json::to_vec(&entries).unwrap().len();
    let names = |entries: &[TreeEntry]| -> Vec<String> {
        entries.iter().map(|entry| entry.name.clone()).collect()
    };
    // as long a path as is taken: all of it in one page is past a hundred times the bound
    let deep = [b'd'; 4096];
    let (all, next) = listed(&deep, directory(10_000), 0, usize::MAX, usize::MAX).unwrap();
    assert!(written(&all) > 100 << 20, "{} bytes", written(&all));
    assert_eq!((all.len(), next), (10_000, None));
    let all = names(&all);

    // with no other end to a page than the bound, page after page is within it,
    // and they give every entry once and in order
    let (mut seen, mut after, mut pages) = (Vec::new(), 0, 0);
    loop {
        let (page, next) = listed(&deep, directory(10_000), after, usize::MAX, STRUCTURED).unwrap();
        assert!(!page.is_empty() && written(&page) <= STRUCTURED);
        seen.extend(names(&page));
        pages += 1;
        match next {
            Some(next) => {
                assert_eq!(next, after + page.len());
                after = next;
            }
            None => break,
        }
    }
    assert_eq!(seen, all);
    assert!(pages > 10, "{pages} pages");

    // near the root the pages are as long as a page is, and more than ten thousand entries are listed
    let (page, next) = listed(b"src", directory(12_345), 0, ENTRIES, STRUCTURED).unwrap();
    assert_eq!((page.len(), next), (ENTRIES, Some(ENTRIES)));
    let (page, next) = listed(b"src", directory(12_345), 12_000, ENTRIES, STRUCTURED).unwrap();
    assert_eq!((page.len(), next), (345, None));
    assert_eq!(page[0].name, "file-12000");
    // the page after the last is empty, and there is none after that
    let (page, next) = listed(b"src", directory(12_345), 12_345, ENTRIES, STRUCTURED).unwrap();
    assert_eq!((page.len(), next), (0, None));
    let past = listed(b"src", directory(12_345), 12_346, ENTRIES, STRUCTURED);
    assert!(matches!(past, Err(Error::Precondition("tree page"))));
    // an entry that is past the bound by itself is a page
    let (page, next) = listed(&deep, directory(3), 0, ENTRIES, 100).unwrap();
    assert_eq!((page.len(), next), (1, Some(1)));
}

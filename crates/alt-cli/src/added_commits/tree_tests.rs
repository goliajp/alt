//! The trees of the commits a push adds.

use alt_git_codec::ObjectKind;

use super::tests::{Held, id};

/// A gitlink names a commit of another repository: nothing asks for it.
#[test]
fn a_tree_with_a_gitlink_is_whole_without_the_commit_it_names() {
    let mut held = Held::default();
    let base = held.commit(&[], 1, "base");
    let elsewhere = id(ObjectKind::Commit, b"of another repository");
    let file = held.put(ObjectKind::Blob, b"file");
    let mut data = b"100644 file\0".to_vec();
    data.extend_from_slice(file.as_bytes());
    data.extend_from_slice(b"160000 sub\0");
    data.extend_from_slice(elsewhere.as_bytes());
    let tree = held.put(ObjectKind::Tree, &data);
    let new = held.commit_of(tree, &[base], 2);
    let added = held.added(&[new], &[base], &[new]).unwrap();
    assert!(added.contains(&tree) && added.contains(&file));
    assert!(!added.contains(&elsewhere));
}

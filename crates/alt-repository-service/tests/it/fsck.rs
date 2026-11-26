//! What `git fsck` refuses is refused where a publication brings it anew.

use super::*;

/// a repository that holds `held` and is read from as well
struct Beside<'a> {
    new: &'a Objects,
    held: &'a Objects,
}
impl ObjectSource for Beside<'_> {
    fn open(&self, oid: ObjectId) -> Result<Option<ObjectReader<'_>>> {
        match self.new.open(oid)? {
            Some(object) => Ok(Some(object)),
            None => self.held.open(oid),
        }
    }
    fn settled(&self, oid: ObjectId) -> Result<Option<ObjectKind>> {
        Ok(self.held.0.get(&oid).map(|(kind, _)| *kind))
    }
}

/// A name `git fsck` refuses that the tree at the same place in the parent
/// already had is no fault of the publication; one it brings there anew is,
/// and so is a `.gitmodules` whose blob Git refuses where the tree before
/// named another blob, and not where it named the same.
#[test]
fn what_git_fsck_refuses_is_refused_where_a_publication_brings_it_anew() {
    let mut held = Objects::default();
    let blob = held.put(ObjectKind::Blob, b"x");
    let bad_modules = held.put(ObjectKind::Blob, b"[submodule \"a\"]\n\tpath = -x\n");
    let below = held.tree(&[("100644", b"git~1", blob)]);
    let old_tree = held.tree(&[
        ("100644", b".alt", blob),
        ("100644", b".gitmodules", bad_modules),
        ("40000", b"d", below),
    ]);
    let tip = held.commit(old_tree, &[]);
    let verify = |new: &Objects, root: ObjectId| {
        let source = Beside { new, held: &held };
        verify_new_graph(&source, HashAlgo::Sha1, &[root], graph_limits()).map(drop)
    };

    let mut new = Objects::default();
    let more = new.put(ObjectKind::Blob, b"more");
    let new_below = new.tree(&[("100644", b"git~1", blob), ("100644", b"m", more)]);
    let new_tree = new.tree(&[
        ("100644", b".alt", blob),
        ("100644", b".gitmodules", bad_modules),
        ("40000", b"d", new_below),
    ]);
    let next = new.commit(new_tree, &[tip]);
    assert!(verify(&new, next).is_ok());
    // the same trees in a commit without a parent: every name is new
    let alone = new.commit(new_tree, &[]);
    let refused = verify(&new, alone);
    assert!(
        matches!(refused, Err(Error::Integrity("hasDotalt: contains '.alt'"))),
        "{refused:?}"
    );
    // `git~1` brought to another place
    let moved = new.tree(&[("100644", b".alt", blob), ("40000", b"e", new_below)]);
    let moved = new.commit(moved, &[tip]);
    let refused = verify(&new, moved);
    assert!(
        matches!(refused, Err(Error::Integrity("hasDotgit: contains '.git'"))),
        "{refused:?}"
    );
    // another `.gitmodules` Git refuses
    let worse = new.put(ObjectKind::Blob, b"[submodule \"a\"]\n\turl = -x\n");
    let changed = new.tree(&[("100644", b".alt", blob), ("100644", b".gitmodules", worse)]);
    let changed = new.commit(changed, &[tip]);
    let refused = verify(&new, changed);
    assert!(
        matches!(
            refused,
            Err(Error::Integrity("gitmodulesUrl: disallowed submodule url"))
        ),
        "{refused:?}"
    );
    // `.gitmodules` as a symbolic link where it was a file
    let link = new.tree(&[("100644", b".alt", blob), ("120000", b".gitmodules", blob)]);
    let link = new.commit(link, &[tip]);
    let refused = verify(&new, link);
    assert!(
        matches!(
            refused,
            Err(Error::Integrity(
                "gitmodulesSymlink: .gitmodules is a symbolic link"
            ))
        ),
        "{refused:?}"
    );
}

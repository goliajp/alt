//! A blob that only a tree which a tag or a ref names leads to, lost with
//! records after it that an older build recorded as synced: it is one the
//! refs lead to, and its records go only with `--committed`.

use std::fs;

use alt_git_codec::ObjectKind;

use super::super::super::fsck_reach::oid_of;
use super::super::super::fsck_trees::{named, tag_of};
use super::super::super::mixed_versions::{new_bin, old_bin, run};
use super::super::super::staged_durability::repo;
use super::super::super::sync_order::lens;
use super::{an_older_build_wrote_past_a_lost_blob, ok, text};

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_lost_blob_below_a_tree_that_a_tag_or_a_ref_names_goes_only_with_committed() {
    let (old, new) = (old_bin(), new_bin());
    for tagged in [false, true] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        repo(dir);
        // the blob `b.txt` is about to be staged as, not stored yet
        let blob = oid_of(ObjectKind::Blob, b"b.txt\n");
        let tree = [&b"100644 b.txt\0"[..], blob.as_bytes()].concat();
        let tree_oid = oid_of(ObjectKind::Tree, &tree);
        let tag = tag_of(tree_oid);
        match tagged {
            true => {
                let objects = [(ObjectKind::Tree, &tree[..]), (ObjectKind::Tag, &tag[..])];
                named(
                    dir,
                    "refs/kept/tree",
                    oid_of(ObjectKind::Tag, &tag),
                    &objects,
                );
            }
            false => named(
                dir,
                "refs/kept/tree",
                tree_oid,
                &[(ObjectKind::Tree, &tree)],
            ),
        }
        an_older_build_wrote_past_a_lost_blob(&old, dir);
        let alt_dir = dir.join(".alt");
        let before = lens(&alt_dir);
        let refused = run(&new, dir, &["fsck", "--drop-lost", "--yes"]);
        assert!(!refused.status.success(), "tagged {tagged}");
        let line =
            format!("blob {blob}: tree {tree_oid}, reached from refs/kept/tree, names it at b.txt");
        let listed = text(&refused.stdout);
        assert!(listed.contains(&line), "tagged {tagged}: {listed}");
        let said = text(&refused.stderr);
        assert!(said.contains("--committed"), "tagged {tagged}: {said}");
        // no commit names the blob
        let named = "1 object(s) that a commit, a tag or a tree a ref leads to names";
        assert!(said.contains(named), "tagged {tagged}: {said}");
        assert_eq!(lens(&alt_dir), before, "tagged {tagged}");
        assert!(!alt_dir.join("objects.lost").exists(), "tagged {tagged}");
        let taken = ok(run(
            &new,
            dir,
            &["fsck", "--drop-lost", "--yes", "--committed"],
        ));
        assert!(taken.contains("took 1 record(s) out"), "{taken}");
        let lost = fs::read_to_string(alt_dir.join("objects.lost")).unwrap();
        assert_eq!(lost, format!("{blob}\n"), "tagged {tagged}");
        let checked = run(&new, dir, &["fsck"]);
        assert!(!checked.status.success(), "tagged {tagged}");
        let said = text(&checked.stderr);
        assert!(
            said.contains(&format!("missing {line}; its records were taken out with")),
            "tagged {tagged}: {said}"
        );
    }
}

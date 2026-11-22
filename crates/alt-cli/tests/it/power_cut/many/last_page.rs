//! The last page of what an add appended to a log is lost in the boot of
//! the add, with no record after it that reads. The command that syncs
//! cuts the log there and has the files added again; whole records went,
//! so what the add noted of its staged blobs being stored holds no more,
//! and an add of a file by its name, unchanged since, stores it again.

use std::fs;
use std::os::unix::fs::FileExt;

use super::super::super::staged_stored::noted_stored;
use super::super::super::sync_order::FILES;
use super::super::super::unconfirmed::{alt, ok, stderr};
use super::super::base::copy_tree;
use super::{Many, PAGE, VERIFIED, files_stored, head_reads};

#[test]
fn files_named_by_the_last_page_of_a_log_lost_in_its_boot_are_stored_again_when_added_by_name() {
    let base = Many::build();
    // the blob map, the Tier 1 map and map.alt
    for file in 1..4 {
        let what = format!(
            "the last page of {} lost in the boot of the add",
            FILES[file]
        );
        let dir = base.tmp.path().join(format!("last-page-{file}"));
        copy_tree(&base.tmp.path().join("base"), &dir);
        // an add that looks through all that is staged and finds it stored
        ok(alt(&dir, &["add", "."]));
        assert!(noted_stored(&dir), "{what}");
        let log = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".alt").join(FILES[file]));
        let at = base.ends[file] - PAGE;
        log.unwrap()
            .write_all_at(&[0u8; PAGE as usize], at)
            .unwrap();
        let refused = alt(&dir, &["commit", "-m", "second"]);
        let said = stderr(&refused);
        assert!(!refused.status.success(), "{what}: {said}");
        assert!(said.contains("add the file again"), "{what}: {said}");

        let mut names = Vec::new();
        for kind in ["small", "parts", "chunked"] {
            for entry in fs::read_dir(dir.join(kind)).unwrap() {
                let name = entry.unwrap().file_name().into_string().unwrap();
                names.push(format!("{kind}/{name}"));
            }
        }
        let mut add = vec!["add"];
        add.extend(names.iter().map(String::as_str));
        let added = alt(&dir, &add);
        assert!(added.status.success(), "{what}: {}", stderr(&added));
        let committed = alt(&dir, &["commit", "-m", "second"]);
        assert!(committed.status.success(), "{what}: {}", stderr(&committed));
        assert!(ok(alt(&dir, &["fsck"])).ends_with(VERIFIED), "{what}");
        head_reads(&what, &dir);
        files_stored(&what, &dir);
        fs::remove_dir_all(dir).unwrap();
    }
}

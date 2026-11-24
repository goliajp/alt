//! What a fetch is answered with when the history has many versions of a file.

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_pack_limits::git;
use crate::altd_server_sending::{answer, asked};
use crate::altd_server_slow_peers::{listed, noise};
use crate::altd_server_untrusted::repository;

/// A file of bytes no compression makes fewer has twelve versions that differ
/// in a few bytes each, in two directories: the response to a clone is not much
/// longer than one version, where the versions sent whole are twenty-four times
/// as long. What git makes of it is every version as it was committed.
#[test]
fn the_versions_of_a_file_are_sent_as_what_each_has_that_the_others_have_not() {
    const BYTES: usize = 300_000;
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    std::fs::create_dir(origin.join("other")).unwrap();
    let mut content = noise(BYTES);
    for version in 0..12u8 {
        content[usize::from(version) * 20_000..][..4].copy_from_slice(&[version; 4]);
        std::fs::write(origin.join("noise"), &content).unwrap();
        content[100] = version;
        std::fs::write(origin.join("other/noise"), &content).unwrap();
        ok(alt(&origin, &["add", "."]));
        ok(alt(&origin, &["commit", "-m", "version"]));
    }
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);
    let refs = listed(&url).unwrap();
    let head = refs.split_whitespace().next().unwrap();

    let response = answer(asked(&server.addr, head));
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.len() < 2 * BYTES, "{} bytes", response.len());

    let cloned = git(tmp.path(), &["clone", "-q", &url, "clone"]);
    assert!(cloned.status.success(), "{cloned:?}");
    let clone = tmp.path().join("clone");
    let checked = git(&clone, &["fsck", "--strict", "--full"]);
    assert!(checked.status.success(), "{checked:?}");
    let counted = git(&clone, &["rev-list", "--objects", "--all"]);
    // the seed and its tree and commit, and of each version: two files, two
    // trees and a commit
    let objects = String::from_utf8_lossy(&counted.stdout).lines().count();
    assert_eq!(objects, 3 + 12 * 5);
    assert_eq!(std::fs::read(clone.join("other/noise")).unwrap(), content);
    for back in 0..12u8 {
        let at = format!("HEAD~{back}:noise");
        let version = git(&clone, &["cat-file", "blob", &at]).stdout;
        assert_eq!(version.len(), BYTES);
        let newest = 11 - back;
        assert_eq!(version[usize::from(newest) * 20_000], newest);
    }
}

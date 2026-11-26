//! The objects a Git push stores, downloaded by the digests of their bytes.

use super::*;

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn objects_pushed_by_git_are_downloadable_by_content_digest() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&work, &f.token);
    let content = "only ever pushed by git\n";
    std::fs::write(work.join("file"), content).unwrap();
    ok(&work, &f.token, &["add", "file"]);
    ok(&work, &f.token, &["commit", "-qm", "one"]);
    ok(&work, &f.token, &["push", "-q", &url, "main"]);
    let download = |digest: &str| {
        server.send(
            "GET",
            &format!("/v1/repositories/repository/epochs/1/content/{digest}"),
            Some(&f.token),
            b"",
        )
    };
    let digest = alt_hash::blake3::hash(content.as_bytes()).to_hex();
    assert_eq!(download(digest.as_str()).into_string().unwrap(), content);
    // the commit, too: its canonical bytes, as `git cat-file commit` prints them
    let commit = ok(&work, &f.token, &["cat-file", "commit", "HEAD"]);
    let digest = alt_hash::blake3::hash(&commit).to_hex();
    assert_eq!(
        download(digest.as_str()).into_string().unwrap().as_bytes(),
        commit
    );
    let absent = alt_hash::blake3::hash(b"never stored").to_hex();
    assert_eq!(download(absent.as_str()).status(), 403);
}

//! Git pushes and native publications on one repository: every Git object ends up
//! in the pack segments, whichever way it came.
use super::git::{identity, ok, text};
use super::*;

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn git_pushes_and_native_publications_take_turns_on_one_branch() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let native = portable::client(&server, &f.token);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&work, &f.token);
    let mut payloads = Vec::new();
    let mut pushed = Vec::new();
    for round in 0..3 {
        if round > 0 {
            // take the native commit that now heads the branch
            ok(&work, &f.token, &["fetch", "-q", &url, "main"]);
            ok(&work, &f.token, &["reset", "-q", "--hard", "FETCH_HEAD"]);
        }
        let content = format!("pushed by git in round {round}\n");
        std::fs::write(work.join(format!("file-{round}")), &content).unwrap();
        ok(&work, &f.token, &["add", "-A"]);
        ok(&work, &f.token, &["commit", "-qm", &format!("git {round}")]);
        ok(&work, &f.token, &["push", "-q", &url, "main"]);
        let head = text(ok(&work, &f.token, &["rev-parse", "HEAD"]));
        pushed.push(content);
        let payload = Payload::new(&format!("native-{round}"), Some(head.parse().unwrap()));
        portable::publish(&native, &payload);
        payloads.push(payload);
    }
    // the content store holds no Git object and no retention for one
    let row = f.client().query_one("SELECT (SELECT count(*) FROM alt_blobs WHERE state='stored'),(SELECT count(*) FROM alt_objects),(SELECT count(*) FROM alt_pins),(SELECT stored_bytes FROM alt_repositories WHERE id='repository'),(SELECT sum(bytes) FROM alt_pack_segments WHERE state='live')::bigint,(SELECT count(*) FROM alt_pack_segments WHERE state='live')", &[]).unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2)
        ),
        (0, 0, 0)
    );
    assert_eq!(row.get::<_, i64>(3), row.get::<_, i64>(4));
    assert_eq!(row.get::<_, i64>(5), 6);

    // every object is there for a Git client
    let clone = temp.path().join("clone");
    ok(
        temp.path(),
        &f.token,
        &["clone", "-q", &url, clone.to_str().unwrap()],
    );
    ok(&clone, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(
        text(ok(&clone, &f.token, &["rev-parse", "HEAD"])),
        payloads[2].commit.to_string()
    );
    assert_eq!(
        text(ok(&clone, &f.token, &["rev-list", "--count", "HEAD"])),
        "6"
    );
    // and for a native client, by the digest of its content
    let download = |bytes: &[u8]| {
        let digest = alt_hash::blake3::hash(bytes).to_hex();
        let response = server.send(
            "GET",
            &format!("/v1/repositories/repository/epochs/1/content/{digest}"),
            Some(&f.token),
            b"",
        );
        assert_eq!(response.status(), 200);
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut response.into_reader(), &mut body).unwrap();
        assert_eq!(body, bytes);
    };
    for content in &pushed {
        download(content.as_bytes());
    }
    for payload in &payloads {
        for bytes in &payload.contents {
            download(bytes);
        }
    }
}

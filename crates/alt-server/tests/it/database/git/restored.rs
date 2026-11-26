//! An object stored as a delta, sent by the digest of its content, is restored
//! within the memory the packs of pushes are verified in.

use super::*;
use crate::database::sign_in_deadline::start;
use alt_pack_segment::{Delta, Memory};
use std::time::Duration;

/// bytes no two of which are alike in a run, which Git does not compress
fn noise(count: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Two versions of a file of noise that differ in a few bytes, pushed together:
/// Git sends one of them as a delta of the other, and it is stored so. Sent by
/// the digest of its content, it is restored whole: while the memory of the
/// server is all taken by others it is told to come again, and the other
/// version, stored whole, is sent as it is; once there is memory it is sent,
/// and the memory is given back once it is.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn an_object_stored_as_a_delta_is_sent_by_its_digest_within_the_memory_there_is() {
    let mut f = Fixture::new();
    let mut memory: Option<&'static Memory> = None;
    let server = start(&mut f, |_, options| {
        options.work_timeout = Duration::from_secs(3);
        memory = Some(options.verifying);
    });
    let memory = memory.unwrap();
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let url = format!("{}/git/repository", server.url);
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    let mut versions = vec![noise(300_000)];
    let mut second = versions[0].clone();
    second[150_000..150_016].copy_from_slice(b"a few bytes more");
    versions.push(second);
    for (at, content) in versions.iter().enumerate() {
        std::fs::write(work.join("large"), content).unwrap();
        git(&["add", "large"]);
        git(&["commit", "-qm", &format!("version {at}")]);
    }
    git(&["push", "-q", &url, "main"]);

    let view = f.store.pack_view(&f.credential, "repository").unwrap();
    let stored = |name: &str| {
        let hex = git(&["rev-parse", name]);
        let oid: Vec<u8> = (0..20)
            .map(|at| u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).unwrap())
            .collect();
        let record = view.segments.record(&oid.try_into().unwrap());
        record.unwrap().unwrap().delta
    };
    let stored = [stored("HEAD~1:large"), stored("HEAD:large")];
    let delta = stored.iter().position(|delta| *delta != Delta::Plain);
    let delta = delta.expect("one version is stored as a delta");
    assert_eq!(stored[1 - delta], Delta::Plain, "{delta}");
    let download = |content: &[u8]| {
        let digest = alt_hash::blake3::hash(content).to_hex();
        let path = format!("/v1/repositories/repository/epochs/1/content/{digest}");
        let response = server.send("GET", &path, Some(&token), b"");
        let status = response.status();
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut response.into_reader(), &mut body).unwrap();
        (status, body)
    };
    let (as_delta, whole) = (&versions[delta], &versions[1 - delta]);

    let others = memory.take(memory.all()).unwrap();
    assert_eq!(download(as_delta).0, 503);
    assert_eq!(download(whole), (200, whole.clone()));
    drop(others);
    assert_eq!(download(as_delta), (200, as_delta.clone()));
    assert_eq!(memory.taken(), (0, 0));
}

//! The packs of pushes are verified within the memory the instance has for them.
use super::*;

fn repository(directory: &Path, token: &str) {
    std::fs::create_dir(directory).unwrap();
    ok(directory, token, &["init", "-q", "--initial-branch=main"]);
    identity(directory, token);
    std::fs::write(directory.join("file"), b"one\n").unwrap();
    ok(directory, token, &["add", "file"]);
    ok(directory, token, &["commit", "-qm", "one"]);
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_waits_for_the_memory_other_packs_are_verified_in_and_is_told_when_it_waited_too_long() {
    let f = Fixture::new();
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    options.work_timeout = Duration::from_secs(8);
    let memory = options.verifying;
    let server = Running::serve(factory, options, None, false);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    repository(&first, &f.token);
    let url = format!("{}/git/repository", server.url);

    // other packs have all of it
    let others = memory.take(memory.all()).unwrap();
    let began = std::time::Instant::now();
    let pushed = git(&first, &f.token, &["push", &url, "main"]);
    let waited = began.elapsed();
    assert!(!pushed.status.success());
    let stderr = String::from_utf8_lossy(&pushed.stderr);
    assert!(
        stderr.contains("the packs of other pushes are being verified; push again"),
        "{stderr}"
    );
    // for half as long as its answer is waited for
    assert!(waited >= Duration::from_millis(3900), "{waited:?}");
    assert_eq!(f.counts().0, 0);

    // given back: the same push goes through, and gives back what it took
    drop(others);
    ok(&first, &f.token, &["push", "-q", &url, "main"]);
    assert_eq!(f.counts().0, 1);
    drop(memory.take(memory.all()).unwrap());
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_pack_that_needs_more_memory_than_packs_are_verified_in_is_refused_and_told_which_limit() {
    let f = Fixture::new();
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    options.verifying = Box::leak(Box::new(alt_server::PackMemory::new(1 << 10)));
    let server = Running::serve(factory, options, None, false);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    repository(&first, &f.token);
    let url = format!("{}/git/repository", server.url);
    let pushed = git(&first, &f.token, &["push", &url, "main"]);
    assert!(!pushed.status.success());
    let stderr = String::from_utf8_lossy(&pushed.stderr);
    assert!(
        stderr.contains("server limit exceeded: index memory"),
        "{stderr}"
    );
    assert_eq!(f.counts().0, 0);
}

/// A process in a group of its own with what it runs. The group is ended when this
/// is let go of, however the test ends: a push that waits would be left waiting.
struct Group(std::process::Child);

impl Group {
    /// ends every process of the group
    fn end(&self) -> bool {
        let group = format!("-{}", self.0.id());
        let killed = Command::new("kill").args(["-KILL", "--", &group]).status();
        killed.is_ok_and(|killed| killed.success())
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.end();
        let _ = self.0.wait();
    }
}

/// A push waits for the memory other packs are verified in, and the client of it
/// goes away: the push leaves the line long before it would have waited to its end.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_that_waits_for_memory_leaves_the_line_once_its_client_is_gone() {
    use std::os::unix::process::CommandExt;
    let f = Fixture::new();
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    // the push would wait for half of this
    options.work_timeout = Duration::from_secs(240);
    let memory = options.verifying;
    let server = Running::serve(factory, options, None, false);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    repository(&first, &f.token);
    let url = format!("{}/git/repository", server.url);
    let others = memory.take(memory.all()).unwrap();
    // git and what it runs for it, in a group of their own
    let auth = alt_base64::STANDARD.encode(format!("alt:{}", f.token));
    let pushing = Command::new("git")
        .args([
            "-c",
            &format!("http.extraHeader=Authorization: Basic {auth}"),
        ])
        .args(["push", &url, "main"])
        .current_dir(&first)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut pushing = Group(pushing);
    let until = |waiting: usize, within: Duration| {
        let began = std::time::Instant::now();
        while memory.waiting().0 != waiting {
            assert!(began.elapsed() < within, "{:?} wait", memory.waiting());
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    until(1, Duration::from_secs(60));
    assert!(pushing.end());
    pushing.0.wait().unwrap();
    until(0, Duration::from_secs(30));
    drop(others);
    assert_eq!(f.counts().0, 0);
    drop(memory.take(memory.all()).unwrap());
}

/// A pack of a few kilobytes that is long in verifying: a blob of eight megabytes
/// and nine hundred and ninety deltas of it, each of which makes another blob as
/// large, to be made and hashed one after the other.
fn long_in_verifying() -> (Vec<u8>, String) {
    const BYTES: usize = 8_000_000;
    let zlib = |data: &[u8]| alt_deflate::compress(data, 1, alt_deflate::Format::Zlib);
    let header = |kind: u8, mut size: usize| {
        let mut out = vec![(kind << 4) | (size as u8 & 15)];
        size >>= 4;
        while size > 0 {
            *out.last_mut().unwrap() |= 128;
            out.push(size as u8 & 127);
            size >>= 7;
        }
        out
    };
    let blob = vec![b'a'; BYTES];
    let mut whole = format!("blob {BYTES}\0").into_bytes();
    whole.extend_from_slice(&blob);
    let id = alt_hash::sha1(&whole);
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&991u32.to_be_bytes());
    pack.extend(header(3, BYTES));
    pack.extend(zlib(&blob));
    for n in 0..990u64 {
        // both sizes, all of the blob copied, eight bytes of its own behind it
        let mut delta = vec![0x80, 0xa4, 0xe8, 0x03, 0x88, 0xa4, 0xe8, 0x03];
        delta.extend_from_slice(&[0x80 | 0x10 | 0x20 | 0x40, 0x00, 0x12, 0x7a, 8]);
        delta.extend_from_slice(&n.to_be_bytes());
        pack.extend(header(7, delta.len()));
        pack.extend_from_slice(&id);
        pack.extend(zlib(&delta));
    }
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    let id = id.iter().map(|byte| format!("{byte:02x}")).collect();
    (pack, id)
}

/// A request has three seconds, and the pack of this push takes many times as long
/// to verify. The push is answered when its time is over, and its pack is verified
/// no further: the memory it was verified in is given back within seconds, long
/// before the last of its deltas would have been come to.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_pack_is_verified_no_further_once_the_time_of_its_request_is_over() {
    let f = Fixture::new();
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    let time = Duration::from_secs(3);
    options.work_timeout = time;
    let memory = options.verifying;
    let server = Running::serve(factory, options, None, false);
    let url = format!("{}/git/repository", server.url);
    let (pack, blob) = long_in_verifying();
    let mut body = Vec::new();
    let line = format!("{} {blob} refs/heads/long\0report-status\n", "0".repeat(40));
    alt_wire::pkt::write_data(&mut body, line.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&pack);
    let began = std::time::Instant::now();
    let (status, answered) = std::thread::scope(|scope| {
        let pushing = scope.spawn(|| super::raw_push::receive(&url, &f.token, &body));
        // the pack is being verified while its request still has time
        while memory.taken() == (0, 0) {
            assert!(began.elapsed() < time, "nothing verified in {time:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let (status, _) = pushing.join().unwrap();
        (status, began.elapsed())
    });
    assert_eq!(status, 503);
    assert!(
        answered >= time && answered < time + Duration::from_secs(5),
        "{answered:?}"
    );
    while memory.taken() != (0, 0) {
        let since = began.elapsed() - answered;
        assert!(
            since < Duration::from_secs(5),
            "{:?} still taken",
            memory.taken()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(f.counts().0, 0);
}

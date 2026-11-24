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
    let mut pushing = Command::new("git")
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
    let until = |waiting: usize, within: Duration| {
        let began = std::time::Instant::now();
        while memory.waiting().0 != waiting {
            assert!(began.elapsed() < within, "{:?} wait", memory.waiting());
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    until(1, Duration::from_secs(60));
    let group = format!("-{}", pushing.id());
    let killed = Command::new("kill").args(["-KILL", "--", &group]).status();
    assert!(killed.unwrap().success());
    pushing.wait().unwrap();
    until(0, Duration::from_secs(30));
    drop(others);
    assert_eq!(f.counts().0, 0);
    drop(memory.take(memory.all()).unwrap());
}

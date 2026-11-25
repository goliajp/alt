//! What a client that is not to be trusted can have `altd-server` do:
//! requests made by hand, as no Git makes them.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::altd_server::{alt, ok, spawn_server, spawn_server_with_env};

/// The status a request made with `curl` is answered with.
fn status(args: &[&str]) -> String {
    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null"])
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub(crate) fn repository(root: &Path) {
    fs::create_dir_all(root).unwrap();
    ok(alt(root, &["init", "."]));
    fs::write(root.join("seed.txt"), "seed\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "seed"]));
}

fn pkt(line: &str) -> Vec<u8> {
    format!("{:04x}{line}", line.len() + 4).into_bytes()
}

/// A root of repositories that lies inside a repository: a name of the
/// root that is no repository is not found in the one above.
#[test]
fn a_name_that_is_no_repository_of_the_root_reaches_none_above_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let outer = tmp.path().join("outer");
    repository(&outer);
    let root = outer.join("root");
    repository(&root.join("served"));
    fs::create_dir(root.join("plain")).unwrap();
    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let refs = |name: &str| {
        let url = format!(
            "http://{}/{name}/info/refs?service=git-upload-pack",
            server.addr
        );
        status(&["--path-as-is", &url])
    };
    assert_eq!(refs("served"), "200");
    for name in ["plain", ".", "..", ".alt", "absent"] {
        assert_eq!(refs(name), "404", "{name}");
    }
}

/// What the server wrote so far, to whatever file.
#[cfg(target_os = "linux")]
fn written(pid: u32) -> u64 {
    let io = fs::read_to_string(format!("/proc/{pid}/io")).unwrap();
    let written = io.lines().find_map(|line| line.strip_prefix("wchar: "));
    written.unwrap().parse().unwrap()
}

/// A body that is sixty megabytes once it is inflated and sixty thousand
/// bytes as it is sent: the cap of a push is on what it inflates to, and
/// no more than that is written out before the push is refused.
#[cfg(target_os = "linux")]
#[test]
fn a_gzip_body_is_counted_as_it_inflates() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_PUSH_BYTES", "65536"),
    ]);
    let zero = "0".repeat(40);
    let mut body = pkt(&format!(
        "{zero} {} refs/heads/x\0report-status\n",
        "1".repeat(40)
    ));
    body.extend_from_slice(b"0000");
    body.resize(60 << 20, 0);
    let plain = tmp.path().join("body");
    fs::write(&plain, &body).unwrap();
    let zipped = Command::new("gzip").arg("-9").arg(&plain).status().unwrap();
    assert!(zipped.success());
    let sent = tmp.path().join("body.gz");
    assert!(fs::metadata(&sent).unwrap().len() < 65536);

    let before = written(server.child.id());
    let answered = status(&[
        "-X",
        "POST",
        "-H",
        "Content-Encoding: gzip",
        "-H",
        "Content-Type: application/x-git-receive-pack-request",
        "--data-binary",
        &format!("@{}", sent.display()),
        &format!("http://{}/git-receive-pack", server.addr),
    ]);
    assert_eq!(answered, "413");
    let wrote = written(server.child.id()) - before;
    assert!(wrote < 1 << 20, "{wrote} bytes written");
}

/// Seventy megabytes of commands, of a push and of a fetch: they are
/// held in memory, and have a cap of their own, far below what the pack
/// of a push may bring.
#[test]
fn the_commands_of_a_request_have_a_cap_of_their_own() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server(&origin);
    let zero = "0".repeat(40);
    let post = |service: &str, body: &[u8]| {
        let sent = tmp.path().join(service);
        fs::write(&sent, body).unwrap();
        status(&[
            "-X",
            "POST",
            "-H",
            "Git-Protocol: version=2",
            "--data-binary",
            &format!("@{}", sent.display()),
            &format!("http://{}/{service}", server.addr),
        ])
    };
    let mut push = Vec::new();
    let mut n = 0;
    while push.len() < 70 << 20 {
        push.extend(pkt(&format!("{zero} {zero} refs/heads/branch-{n:09}\n")));
        n += 1;
    }
    push.extend_from_slice(b"0000");
    assert_eq!(post("git-receive-pack", &push), "400");

    let mut fetch = pkt("command=ls-refs\n");
    fetch.extend_from_slice(b"00010000");
    assert_eq!(post("git-upload-pack", &fetch), "200");
    fetch.resize(70 << 20, b'0');
    assert_eq!(post("git-upload-pack", &fetch), "413");
}

/// The most memory of its own the process `pid` had while `during` ran.
#[cfg(target_os = "linux")]
fn most_memory(pid: u32, during: impl FnOnce()) -> u64 {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (over, most) = (AtomicBool::new(false), std::sync::atomic::AtomicU64::new(0));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !over.load(Ordering::Relaxed) {
                let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
                let anon = status
                    .lines()
                    .find_map(|line| line.strip_prefix("RssAnon:"));
                let kb: u64 = anon
                    .unwrap()
                    .trim()
                    .trim_end_matches(" kB")
                    .parse()
                    .unwrap();
                most.fetch_max(kb << 10, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        });
        during();
        over.store(true, Ordering::Relaxed);
    });
    most.into_inner()
}

/// A clone of forty megabytes in forty files: the objects go into the
/// pack one at a time and the pack is sent from its file, so the server
/// holds a few of the megabytes at a time and not all of them twice.
#[cfg(target_os = "linux")]
#[test]
fn a_clone_takes_the_memory_of_its_largest_objects_not_of_the_repository() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for n in 0..40 {
        let noise: Vec<u8> = (0..1 << 20)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        fs::write(origin.join(format!("{n}.bin")), noise).unwrap();
    }
    ok(alt(&origin, &["add", "."]));
    ok(alt(&origin, &["commit", "-m", "forty megabytes"]));
    let server = spawn_server(&origin);
    let url = format!("http://{}/", server.addr);
    let into = tmp.path().join("clone");
    let most = most_memory(server.child.id(), || {
        let out = Command::new("git")
            .args(["clone", "-q", &url, into.to_str().unwrap()])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    });
    assert_eq!(fs::metadata(into.join("39.bin")).unwrap().len(), 1 << 20);
    assert!(most < 24 << 20, "{} MiB", most >> 20);
}

/// A repository whose store another process has locked is opened by the
/// first request for it, which waits: a request for another repository
/// of the root is answered meanwhile.
#[test]
fn a_repository_that_waits_to_be_opened_keeps_no_other_waiting() {
    use alt_os::sys::{LOCK_EX, LOCK_UN, flock};
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("locked"));
    repository(&root.join("free"));
    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let url = |name: &str| format!("http://{}/{name}/git-upload-pack", server.addr);
    let post = |name: &str| {
        let mut ls_refs = pkt("command=ls-refs\n");
        ls_refs.extend_from_slice(b"00010000");
        let sent = tmp.path().join(format!("{name}.request"));
        fs::write(&sent, ls_refs).unwrap();
        status(&[
            "-X",
            "POST",
            "--data-binary",
            &format!("@{}", sent.display()),
            &url(name),
        ])
    };
    let held = fs::File::open(root.join("locked/.alt/odb.lock")).unwrap();
    flock(&held, LOCK_EX).unwrap();
    std::thread::scope(|scope| {
        let waits = scope.spawn(|| post("locked"));
        let (done, answered) = std::sync::mpsc::channel();
        scope.spawn(move || done.send(post("free")));
        // the time limit is for a wait that ends only with the lock
        let free = answered.recv_timeout(std::time::Duration::from_secs(60));
        flock(&held, LOCK_UN).unwrap();
        assert_eq!(free.expect("the other repository waits as well"), "200");
        assert_eq!(waits.join().unwrap(), "200");
    });
}

/// A name that is no user and a token that is not the user's are answered
/// with the same words: which names there are is not told.
#[test]
fn a_name_that_is_no_user_is_told_what_a_user_with_another_token_is() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("served"));
    let hash = alt_hash::blake3::hash(b"hers").to_hex();
    fs::write(root.join("users"), format!("alice\t{hash}\n")).unwrap();
    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let told = |user: &str, token: &str| {
        let url = format!("http://{}/served/info/refs", server.addr);
        let out = Command::new("curl")
            .args([
                "-sS",
                "-w",
                "\n%{http_code}",
                "-u",
                &format!("{user}:{token}"),
            ])
            .arg(format!("{url}?service=git-upload-pack"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(told("alice", "hers").ends_with("\n200"));
    let refused = told("alice", "another");
    assert_eq!(refused, "bad credentials\n401");
    assert_eq!(told("nobody", "hers"), refused);
    assert_eq!(told("nobody", "another"), refused);
}

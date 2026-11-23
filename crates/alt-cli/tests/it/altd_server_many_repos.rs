//! A root of many repositories: what the server keeps open of them, and
//! what it does once it can open nothing more.

use std::fs;
use std::path::Path;

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_pack_limits::git;
use crate::altd_server_untrusted::repository;

/// A server started by hand, ended when the test lets go of it.
#[cfg(target_os = "linux")]
struct Ended(std::process::Child);

#[cfg(target_os = "linux")]
impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn listed(url: &str) -> String {
    let patient = ["-c", "http.lowSpeedLimit=1", "-c", "http.lowSpeedTime=120"];
    let out = git(
        Path::new("."),
        &[&patient[..], &["ls-remote", url]].concat(),
    );
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[cfg(target_os = "linux")]
fn open_files(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/fd")).unwrap().count()
}

/// Six repositories are asked for one after the other, and the server is
/// told to keep two open: it has no more files open after the sixth than
/// after the second, and one that was closed is opened again when it is
/// asked for.
#[cfg(target_os = "linux")]
#[test]
fn no_more_repositories_of_a_root_stay_open_than_the_server_is_told() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    let names = ["a", "b", "c", "d", "e", "f"];
    for name in names {
        repository(&root.join(name));
    }
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", root.to_str().unwrap()),
        ("ALT_SERVER_MAX_OPEN_REPOS", "2"),
    ]);
    let mut open = Vec::new();
    for name in names {
        assert!(listed(&format!("http://{}/{name}/", server.addr)).contains("refs/heads/main"));
        open.push(open_files(server.child.id()));
    }
    assert!(open[1] > open[0], "{open:?}");
    assert!(open[5] <= open[1], "{open:?}");
    assert!(listed(&format!("http://{}/a/", server.addr)).contains("refs/heads/main"));
}

/// A repository is removed and another is put under its name: what is
/// served from then on is the one that is there.
#[test]
fn a_repository_put_in_the_place_of_another_is_the_one_served() {
    put_in_place("ALT_SERVER_ROOT", "served/");
}

/// And so where the server serves the one repository it was started on.
#[test]
fn the_one_repository_of_a_server_put_in_the_place_of_another_is_the_one_served() {
    put_in_place("ALT_SERVER_REPO", "");
}

/// A server told of `root/served` through `told`, as a root or as its
/// repository, which it serves at `at`.
fn put_in_place(told: &str, at: &str) {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("served"));
    let serves = match at.is_empty() {
        true => root.join("served"),
        false => root.clone(),
    };
    let server = spawn_server_with_env(&[(told, serves.to_str().unwrap())]);
    let url = format!("http://{}/{at}", server.addr);
    let before = listed(&url);

    fs::remove_dir_all(root.join("served")).unwrap();
    repository(&root.join("served"));
    fs::write(root.join("served/more.txt"), "more\n").unwrap();
    ok(alt(&root.join("served"), &["add", "."]));
    ok(alt(&root.join("served"), &["commit", "-m", "more"]));
    let after = listed(&url);
    assert_ne!(after, before);
    let clone = tmp.path().join("clone");
    let cloned = git(tmp.path(), &["clone", "-q", &url, clone.to_str().unwrap()]);
    assert!(cloned.status.success(), "{cloned:?}");
    assert_eq!(
        fs::read_to_string(clone.join("more.txt")).unwrap(),
        "more\n"
    );
}

/// The processor time, in ticks, the process `pid` has used.
#[cfg(target_os = "linux")]
fn ticks(pid: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<&str> = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}

/// A server that may have forty-eight files open is sent more
/// connections than that. While it can take no more it waits, and does
/// not spend its time finding again and again that it cannot; once they
/// are gone it answers.
#[cfg(target_os = "linux")]
#[test]
fn a_server_that_can_take_no_more_connections_waits_for_one_to_end() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::Duration;
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let start = "ulimit -n 48 && exec \"$0\" --bind 127.0.0.1:0";
    let mut child = Command::new("sh")
        .args(["-c", start, env!("CARGO_BIN_EXE_altd-server")])
        .env("ALT_SERVER_REPO", &origin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut said = BufReader::new(child.stderr.take().unwrap());
    // ended with the test, however the test ends
    let child = Ended(child);
    let (tell, told) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while said.read_line(&mut line).is_ok_and(|read| read > 0) {
            if let Some(at) = line.find("listening on ") {
                let address = line[at + "listening on ".len()..].split_whitespace().next();
                let _ = tell.send(address.unwrap().to_owned());
            }
            line.clear();
        }
    });
    let address = told.recv_timeout(Duration::from_secs(60));
    let address = address.expect("the server did not say where it listens");
    let held: Vec<_> = (0..120)
        .map(|_| std::net::TcpStream::connect(&address).unwrap())
        .collect();
    // every file it may have is open by now
    std::thread::sleep(Duration::from_secs(1));
    let before = ticks(child.0.id());
    std::thread::sleep(Duration::from_secs(2));
    let spent = ticks(child.0.id()) - before;
    drop(held);
    let answered = listed(&format!("http://{address}/"));
    drop(child);
    // two seconds are two hundred ticks of one thread that does not wait
    assert!(spent < 40, "{spent} ticks in two seconds");
    assert!(answered.contains("refs/heads/main"), "{answered}");
}

/// A signed push is signed over a nonce the server gave it with the refs,
/// and comes with it a request later. The repository is closed between
/// the two, for another that is asked for all the while and the one
/// repository the server keeps open: the nonce is good all the same.
#[test]
fn the_nonce_of_a_signed_push_is_good_though_its_repository_was_closed_meanwhile() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("pushed"));
    repository(&root.join("other"));
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", root.to_str().unwrap()),
        ("ALT_SERVER_MAX_OPEN_REPOS", "1"),
    ]);
    let url = format!("http://{}/pushed/", server.addr);
    let other = format!("http://{}/other/", server.addr);
    let client = tmp.path().join("client");
    ok(alt(tmp.path(), &["clone", &url, client.to_str().unwrap()]));
    crate::altd_server::enable_signed_push(&client, &root.join("pushed"), "alice");
    // a push whose signature does not hold is one of nobody, and refused
    let policy = root.join("pushed/.alt/policy");
    fs::write(policy, "human:anonymous -> require-signed\n").unwrap();
    let over = AtomicBool::new(false);
    let pushes = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !over.load(Ordering::Acquire) {
                listed(&other);
            }
        });
        let pushes: Vec<_> = (0..8)
            .map(|n| {
                fs::write(client.join("file"), n.to_string()).unwrap();
                ok(alt(&client, &["add", "."]));
                ok(alt(&client, &["commit", "-m", "more"]));
                alt(
                    &client,
                    &["push", "origin", "refs/heads/main:refs/heads/main"],
                )
            })
            .collect();
        over.store(true, Ordering::Release);
        pushes
    });
    for push in pushes {
        let told = String::from_utf8_lossy(&push.stderr).into_owned();
        assert!(push.status.success(), "{told}");
    }
}

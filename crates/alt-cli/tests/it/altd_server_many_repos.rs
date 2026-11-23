//! A root of many repositories: what the server keeps open of them, and
//! what it does once it can open nothing more.

use std::fs;
use std::path::Path;

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_pack_limits::git;
use crate::altd_server_untrusted::repository;

fn listed(url: &str) -> String {
    let out = git(Path::new("."), &["ls-remote", url]);
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
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("served"));
    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let url = format!("http://{}/served/", server.addr);
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
    let address = loop {
        let mut line = String::new();
        assert_ne!(said.read_line(&mut line).unwrap(), 0, "the server ended");
        if let Some(at) = line.find("listening on ") {
            let address = line[at + "listening on ".len()..].split_whitespace().next();
            break address.unwrap().to_owned();
        }
    };
    let held: Vec<_> = (0..120)
        .map(|_| std::net::TcpStream::connect(&address).unwrap())
        .collect();
    // every file it may have is open by now
    std::thread::sleep(Duration::from_secs(1));
    let before = ticks(child.id());
    std::thread::sleep(Duration::from_secs(2));
    let spent = ticks(child.id()) - before;
    drop(held);
    let answered = listed(&format!("http://{address}/"));
    child.kill().unwrap();
    child.wait().unwrap();
    // two seconds are two hundred ticks of one thread that does not wait
    assert!(spent < 40, "{spent} ticks in two seconds");
    assert!(answered.contains("refs/heads/main"), "{answered}");
}

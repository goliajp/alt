//! A pack that holds an object twice, the first time as a delta of
//! itself: the server that is pushed it and the client that fetches it
//! take it, as Git does, and read what it holds.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

use crate::altd_server::{Server, ok};
use crate::altd_server_no_new_objects::{creation, push, seeded, tip};

const FILE: &[u8] = b"what the file holds, the same in both of its entries\n";

fn entry(kind: u8, before: &[u8], data: &[u8]) -> Vec<u8> {
    let mut size = data.len();
    let mut entry = vec![(kind << 4) | (size as u8 & 15)];
    size >>= 4;
    while size > 0 {
        *entry.last_mut().unwrap() |= 128;
        entry.push(size as u8 & 127);
        size >>= 7;
    }
    entry.extend_from_slice(before);
    entry.extend(alt_deflate::compress(data, 6, alt_deflate::Format::Zlib));
    entry
}

/// A commit on `parent`, where there is one, of a tree with the one file,
/// and the pack of them: the file as a delta of itself that copies all of
/// it, and stored behind that. The delta is the entry of the file an index
/// lists first: a read that took the first entry of an id for the object
/// would go from the delta to the delta.
fn repeated(parent: Option<&str>) -> (ObjectId, Vec<u8>) {
    let id = |kind, data: &[u8]| ObjectId::hash_object(HashAlgo::Sha1, kind, data);
    let file = id(ObjectKind::Blob, FILE);
    let tree = [&b"100644 file\0"[..], file.as_bytes()].concat();
    let parent = parent.map_or(String::new(), |parent| format!("parent {parent}\n"));
    let who = "tester <t@e> 1700000000 +0000";
    let commit = format!(
        "tree {}\n{parent}author {who}\ncommitter {who}\n\nrepeated\n",
        id(ObjectKind::Tree, &tree)
    );
    let size = FILE.len() as u8;
    let all_of_itself = [size, size, 0x80 | 0x10, size];
    let mut pack = [&b"PACK"[..], &2u32.to_be_bytes(), &4u32.to_be_bytes()].concat();
    pack.extend(entry(1, b"", commit.as_bytes()));
    pack.extend(entry(2, b"", &tree));
    pack.extend(entry(7, file.as_bytes(), &all_of_itself));
    pack.extend(entry(3, b"", FILE));
    let sum = alt_hash::sha1(&pack);
    pack.extend_from_slice(&sum);
    (id(ObjectKind::Commit, commit.as_bytes()), pack)
}

/// The address space a process of these tests is given, in kilobytes. A read
/// that goes round takes memory until there is none: with this it ends in a
/// moment, and the test with it.
const MOST: u64 = 4_000_000;

/// What runs `program` with that much address space and no more, where the
/// system has such a limit.
fn given(program: &std::ffi::OsStr) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "ulimit -v \"$0\" 2>/dev/null; exec \"$@\""]);
    cmd.arg(MOST.to_string()).arg(program);
    cmd
}

/// A server of the repository at `origin`, given so much.
fn served(origin: &std::path::Path) -> Server {
    let mut cmd = given(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_altd-server")));
    cmd.args(["--bind", "127.0.0.1:0"])
        .env("ALT_SERVER_REPO", origin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let addr = loop {
        line.clear();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0, "the server ended");
        if let Some((_, after)) = line.split_once("listening on ") {
            break after.split_whitespace().next().unwrap().to_owned();
        }
    };
    Server {
        child,
        addr,
        stderr: Some(stderr),
    }
}

/// What `work` comes to, or a failure where it has not ended in two minutes.
fn within<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, ended) = mpsc::channel();
    std::thread::spawn(move || done.send(work()));
    let ended = ended.recv_timeout(Duration::from_secs(120));
    ended.expect("ended within two minutes")
}

#[test]
fn a_server_pushed_an_object_again_as_a_delta_of_itself_takes_it_and_gives_it() {
    let (origin, seed) = seeded();
    let server = served(origin.path());
    let url = format!("http://{}/", server.addr);
    let (commit, pack) = repeated(Some(&seed));
    let name = commit.to_string();
    let pushed = url.clone();
    let report = within(move || push(&pushed, &[creation("refs/heads/again", &name)], &pack));
    assert_eq!(report.unpack, Ok(()));
    assert_eq!(tip(origin.path(), "refs/heads/again"), commit.to_string());

    let temp = alt_os::temp::tempdir().unwrap();
    let to = temp.path().join("clone");
    let cloned = within(move || {
        let args = [
            "clone",
            "-q",
            "--branch",
            "again",
            &url,
            to.to_str().unwrap(),
        ];
        let out = Command::new("git").args(args).output().unwrap();
        (out, std::fs::read(to.join("file")))
    });
    assert!(cloned.0.status.success(), "{:?}", cloned.0);
    assert_eq!(cloned.1.unwrap(), FILE);
}

/// A server of one branch that answers every fetch with `pack`.
fn serving(head: ObjectId, pack: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut first, mut length) = (String::new(), 0);
            reader.read_line(&mut first).unwrap();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end().to_ascii_lowercase();
                if let Some(said) = header.strip_prefix("content-length:") {
                    length = said.trim().parse().unwrap();
                }
                if header.is_empty() {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let asked = String::from_utf8_lossy(&body).into_owned();
            let mut answer = Vec::new();
            if first.starts_with("GET ") {
                let commands = [("ls-refs", None), ("fetch", None)];
                alt_wire::caps::encode_capability_advertisement(
                    &mut answer,
                    "git-upload-pack",
                    "test",
                    Some("sha1"),
                    &commands,
                )
                .unwrap();
            } else if asked.contains("command=ls-refs") {
                let named = |name: &str, symref: Option<&str>| alt_wire::ls_refs::RefRecord {
                    oid: head,
                    name: name.to_owned(),
                    symref_target: symref.map(str::to_owned),
                    peeled: None,
                    other: Default::default(),
                };
                let refs = [
                    named("HEAD", Some("refs/heads/main")),
                    named("refs/heads/main", None),
                ];
                alt_wire::ls_refs::encode_ls_refs_response(&mut answer, &refs).unwrap();
            } else {
                alt_wire::fetch::encode_fetch_response_packfile(&mut answer, &pack).unwrap();
            }
            let head = format!(
                "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                answer.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&answer).unwrap();
        }
    });
    url
}

#[test]
fn a_client_sent_an_object_again_as_a_delta_of_itself_takes_it_and_has_the_file() {
    let (commit, pack) = repeated(None);
    let url = serving(commit, pack);
    let temp = alt_os::temp::tempdir().unwrap();
    let (at, to) = (temp.path().to_owned(), temp.path().join("clone"));
    let cloned = to.clone();
    let out = within(move || {
        let plain = crate::alt_command();
        let mut cmd = given(plain.get_program());
        for (name, value) in plain.get_envs() {
            match value {
                Some(value) => cmd.env(name, value),
                None => cmd.env_remove(name),
            };
        }
        cmd.current_dir(&at).env("ALT_NO_DAEMON", "1");
        cmd.args(["clone", &url, cloned.to_str().unwrap()]);
        cmd.output().unwrap()
    });
    ok(out);
    assert_eq!(std::fs::read(to.join("file")).unwrap(), FILE);
    assert_eq!(tip(&to, "HEAD"), commit.to_string());
}

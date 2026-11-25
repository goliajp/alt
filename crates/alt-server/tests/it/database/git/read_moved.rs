//! A push is published only where the refs it was read with still lead to what
//! they led to then.

use super::led::packed;
use super::raw_push::receive;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The hook of the store is one for the process: the tests that set it take
/// turns.
static HOOKED: Mutex<()> = Mutex::new(());

/// A branch `back` is pushed to the commit only the branch `side` holds, with
/// a pack of nothing. While it is between its verification and its
/// publication, `side` is deleted: it is told that a ref changed and nothing
/// is published, and pushed again it is refused, as one is that names an
/// object no ref leads to.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_whose_refs_were_deleted_after_it_was_verified_is_told_so_and_not_published() {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let url = format!("{}/git/repository", server.url);
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    let commit = |name: &str| {
        std::fs::write(work.join(name), format!("{name}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", name]);
        git(&["rev-parse", "HEAD"])
    };
    let main = commit("first");
    git(&["checkout", "-q", "-b", "side"]);
    let side = commit("side");
    git(&["checkout", "-q", "main"]);
    git(&["push", "-q", &url, "main", "side"]);
    assert!(!(f.store.pack_view(&f.credential, "repository").unwrap()).unreachable);

    // the first push to be published is held there until `side` is deleted
    let (arrived, at) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let (arrived, gone) = (Mutex::new(arrived), Mutex::new(gone));
    let this = f.root.path().file_name().unwrap().to_owned();
    let held = AtomicBool::new(false);
    *alt_server_store::PUSH_VERIFIED.lock().unwrap() = Some(Arc::new(move |root: &Path| {
        let ours = root.iter().any(|part| part == this);
        if ours && !held.swap(true, SeqCst) {
            arrived.lock().unwrap().send(()).unwrap();
            gone.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
        }
    }));
    let zero = "0".repeat(40);
    let mut body = Vec::new();
    let command = format!("{zero} {side} refs/heads/back\0report-status\n");
    alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&packed(work, &token, &[]));
    let pushing = {
        let (url, token, body) = (url.clone(), token.clone(), body.clone());
        std::thread::spawn(move || receive(&url, &token, &body))
    };
    at.recv_timeout(Duration::from_secs(60)).unwrap();
    git(&["push", "-q", &url, ":side"]);
    go.send(()).unwrap();
    let (status, report) = pushing.join().unwrap();
    *alt_server_store::PUSH_VERIFIED.lock().unwrap() = None;
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains("ng refs/heads/back a ref changed since it was read"),
        "{report}"
    );
    let refs = git(&["ls-remote", "--refs", &url]);
    assert_eq!(refs, format!("{main}\trefs/heads/main"));

    let (status, report) = receive(&url, &token, &body);
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ng refs/heads/back"), "{report}");
    assert_eq!(git(&["ls-remote", "--refs", &url]), refs);
}

/// A thin pack of the commits `new` leads to beyond those `bases` lead to:
/// its deltas may be made of the objects of `bases`.
fn thin(directory: &Path, new: &str, bases: &[&str]) -> Vec<u8> {
    use std::io::Write;
    let mut revs = format!("{new}\n");
    for base in bases {
        revs += &format!("^{base}\n");
    }
    let mut packing = Command::new("git")
        .args(["pack-objects", "--stdout", "-q", "--thin", "--revs"])
        .current_dir(directory)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = packing.stdin.take().unwrap();
    let written = input.write_all(revs.as_bytes());
    drop(input);
    let packed = packing.wait_with_output().unwrap().stdout;
    written.unwrap();
    packed
}

/// A pack of the commit `commit` and its tree, whole, and of a file of the
/// tree as a delta of `base`, which it does not carry: the file is `base` and
/// the bytes `more` after it. Git makes its thin packs of what the
/// parents of the commits it sends hold; this one leans on what it is told.
fn leaning(directory: &Path, commit: &str, (base, more): (&str, &[u8])) -> Vec<u8> {
    let object = |kind: &str, name: &str| -> Vec<u8> {
        let mut shown = Command::new("git");
        shown.args(["cat-file", kind, name]).current_dir(directory);
        shown.output().unwrap().stdout
    };
    let size = |mut n: usize, out: &mut Vec<u8>| loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        out.push(if n > 0 { byte | 0x80 } else { byte });
        if n == 0 {
            return;
        }
    };
    let header = |kind: u8, len: usize, out: &mut Vec<u8>| {
        let mut byte = kind << 4 | (len & 0x0f) as u8;
        let mut rest = len >> 4;
        while rest > 0 {
            out.push(byte | 0x80);
            byte = (rest & 0x7f) as u8;
            rest >>= 7;
        }
        out.push(byte);
    };
    let base_bytes = object("blob", base);
    // copy all of the base, then put `more` after it
    let mut delta = Vec::new();
    size(base_bytes.len(), &mut delta);
    size(base_bytes.len() + more.len(), &mut delta);
    delta.push(0x80 | 0x10 | 0x20 | 0x40);
    delta.extend_from_slice(&(base_bytes.len() as u32).to_le_bytes()[..3]);
    delta.push(more.len() as u8);
    delta.extend_from_slice(more);
    let mut named = Command::new("git");
    named
        .args(["rev-parse", &format!("{commit}^{{tree}}")])
        .current_dir(directory);
    let tree = text(named.output().unwrap().stdout);
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&3u32.to_be_bytes());
    for (kind, data) in [(1, object("commit", commit)), (2, object("tree", &tree))] {
        header(kind, data.len(), &mut pack);
        pack.extend_from_slice(&alt_deflate::compress(&data, 1, alt_deflate::Format::Zlib));
    }
    header(7, delta.len(), &mut pack);
    let base_id = (0..20).map(|at| u8::from_str_radix(&base[at * 2..at * 2 + 2], 16).unwrap());
    pack.extend(base_id);
    pack.extend_from_slice(&alt_deflate::compress(&delta, 1, alt_deflate::Format::Zlib));
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

/// The push of `body` held between its verification and its publication
/// while `meanwhile` runs, and what it was answered.
fn held_while(
    f: &Fixture,
    url: &str,
    token: &str,
    body: Vec<u8>,
    meanwhile: impl FnOnce(),
) -> (u16, String) {
    let (arrived, at) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let (arrived, gone) = (Mutex::new(arrived), Mutex::new(gone));
    let this = f.root.path().file_name().unwrap().to_owned();
    let held = AtomicBool::new(false);
    *alt_server_store::PUSH_VERIFIED.lock().unwrap() = Some(Arc::new(move |root: &Path| {
        let ours = root.iter().any(|part| part == this);
        if ours && !held.swap(true, SeqCst) {
            arrived.lock().unwrap().send(()).unwrap();
            gone.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
        }
    }));
    let pushing = {
        let (url, token) = (url.to_owned(), token.to_owned());
        std::thread::spawn(move || receive(&url, &token, &body))
    };
    at.recv_timeout(Duration::from_secs(60)).unwrap();
    meanwhile();
    go.send(()).unwrap();
    let answered = pushing.join().unwrap();
    *alt_server_store::PUSH_VERIFIED.lock().unwrap() = None;
    answered
}

/// A push to `main` with a thin pack made of what `main` holds is published
/// though a branch that led to other objects is deleted between its
/// verification and its publication: it leans on nothing of that branch.
/// One whose delta is made of a file only the deleted branch held is told
/// that a ref changed, and nothing is published.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_is_told_a_ref_changed_only_where_it_leans_on_what_the_ref_led_to() {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let url = format!("{}/git/repository", server.url);
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    // lines no other file of the history is like
    let lines = |seed: u64, count: u64| -> String {
        (0..count)
            .map(|n| {
                format!(
                    "{:016x}\n",
                    (seed * 1_000_003 + n).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                )
            })
            .collect()
    };
    let commit = |file: &str, content: &str, message: &str| {
        std::fs::write(work.join(file), content).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", message]);
        git(&["rev-parse", "HEAD"])
    };
    let main = commit("main.txt", &lines(1, 400), "first");
    git(&["checkout", "-q", "-b", "other"]);
    commit("other.txt", &lines(2, 400), "other");
    git(&["checkout", "-q", "main"]);
    git(&["checkout", "-q", "-b", "gone"]);
    let only_gone = lines(3, 400);
    commit("gone.txt", &only_gone, "gone");
    git(&["checkout", "-q", "main"]);
    git(&["push", "-q", &url, "main", "other", "gone"]);

    // a change to the file of `main`, its pack made of what `main` holds
    let next = commit("main.txt", &(lines(1, 400) + "more\n"), "next");
    let command = format!("{main} {next} refs/heads/main\0report-status\n");
    let mut body = Vec::new();
    alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&thin(work, "main", &[&main]));
    let (status, report) = held_while(&f, &url, &token, body, || {
        git(&["push", "-q", &url, ":other"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ok refs/heads/main"), "{report}");

    // a file like the one only `gone` held, its delta made of that one
    let gone_file = git(&["rev-parse", "gone:gone.txt"]);
    let last = commit("copy.txt", &(only_gone + "more\n"), "last");
    let command = format!("{next} {last} refs/heads/main\0report-status\n");
    let mut body = Vec::new();
    alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&leaning(work, &last, (&gone_file, b"more\n")));
    let (status, report) = held_while(&f, &url, &token, body, || {
        git(&["push", "-q", &url, ":gone"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains("ng refs/heads/main a ref changed since it was read"),
        "{report}"
    );
    let refs = git(&["ls-remote", "--refs", &url]);
    assert_eq!(refs, format!("{next}\trefs/heads/main"));
}

//! What a push whose refs moved may have lost is found before the row of its
//! repository is held, within steps of its own, and the row is held only to
//! tell that the refs are still where they were then, or moved on from there.

use super::led::packed;
use super::raw_push::receive;
use super::read_moved::{HOOKED, held_while};
use super::read_moved_lost::{Pushed, body, pushed};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};

/// a commit on `base` with a file of its own, not pushed, and a pack of it
fn next(f: &Fixture, p: &Pushed, base: &str, file: &str) -> (String, Vec<u8>) {
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    git(&["checkout", "-q", "--detach", base]);
    std::fs::write(p.work.join(file), format!("{file}\n")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", file]);
    let next = git(&["rev-parse", "HEAD"]);
    let (tree, blob) = (format!("{next}^{{tree}}"), format!("{next}:{file}"));
    let pack = packed(&p.work, &f.token, &[&next, &tree, &blob]);
    git(&["checkout", "-q", "main"]);
    (next, pack)
}

type Hook = Mutex<Option<alt_server_store::PushVerified>>;

/// Holds the first push to the repository of `f` that comes to `hook`: it is
/// told on the first channel when the push is there, and the push goes on once
/// it is told on the second.
fn stop_at(f: &Fixture, hook: &Hook) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (arrived, at) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let (arrived, gone) = (Mutex::new(arrived), Mutex::new(gone));
    let this = f.root.path().file_name().unwrap().to_owned();
    let held = AtomicBool::new(false);
    *hook.lock().unwrap() = Some(Arc::new(move |root: &Path| {
        let ours = root.iter().any(|part| part == this);
        if ours && !held.swap(true, SeqCst) {
            arrived.lock().unwrap().send(()).unwrap();
            gone.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
        }
    }));
    (at, go)
}

/// The push of `body`, held once what it may have lost was found, while
/// `meanwhile` is done: what it is answered.
fn found_while(f: &Fixture, url: &str, body: Vec<u8>, meanwhile: impl FnOnce()) -> (u16, String) {
    let (at, go) = stop_at(f, &alt_server_store::PUSH_FOUND);
    let pushing = {
        let (url, token) = (url.to_owned(), f.token.clone());
        std::thread::spawn(move || receive(&url, &token, &body))
    };
    at.recv_timeout(Duration::from_secs(60)).unwrap();
    meanwhile();
    go.send(()).unwrap();
    let answered = pushing.join().unwrap();
    *alt_server_store::PUSH_FOUND.lock().unwrap() = None;
    answered
}

/// Once what the push may have lost was found, the row of the repository is
/// not held: another push, to another branch, is published meanwhile, and the
/// push is published after it, the branch having moved on. A branch deleted
/// meanwhile may lose what was not looked for: the push is told a ref changed.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_is_published_where_a_ref_moved_on_after_its_losses_were_found_and_refused_where_one_was_deleted()
 {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    git(&["push", "-q", &p.url, "main:refs/heads/other"]);
    let (first, pack) = next(&f, &p, &p.main, "first.txt");
    let pushed_first = body(&[(&p.main, &first, "refs/heads/main")], &pack);
    let (status, report) = found_while(&f, &p.url, pushed_first, || {
        git(&["checkout", "-q", "-b", "other", &p.main]);
        std::fs::write(p.work.join("other.txt"), "other\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "other"]);
        git(&["push", "-q", &p.url, "other"]);
        git(&["checkout", "-q", "main"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ok refs/heads/main"), "{report}");
    let other = git(&["rev-parse", "other"]);
    let listed = git(&["ls-remote", &p.url, "refs/heads/other"]);
    assert!(listed.starts_with(&other), "{listed}");

    let (second, pack) = next(&f, &p, &first, "second.txt");
    let pushed_second = body(&[(&first, &second, "refs/heads/main")], &pack);
    let (status, report) = found_while(&f, &p.url, pushed_second, || {
        git(&["push", "-q", &p.url, ":gone"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains("ng refs/heads/main a ref changed since it was read"),
        "{report}"
    );
    let listed = git(&["ls-remote", &p.url, "refs/heads/main"]);
    assert!(listed.starts_with(&first), "{listed}");
}

/// What finding the losses of a push may take, lowered for a test, and put
/// back as it was when dropped.
struct Walk(usize);

impl Walk {
    fn lowered(steps: usize) -> Self {
        Walk(alt_server_store::LOST_WALK.swap(steps, SeqCst))
    }
}

impl Drop for Walk {
    fn drop(&mut self) {
        alt_server_store::LOST_WALK.store(self.0, SeqCst);
    }
}

/// A branch is deleted while a push to `main` waits to be published. Finding
/// what the push may have lost takes more steps than it may: the push is
/// refused, and told so; sent again it is published.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_whose_losses_take_more_to_find_than_it_may_is_refused_and_told_so() {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    let (first, pack) = next(&f, &p, &p.main, "first.txt");
    let pushed_first = body(&[(&p.main, &first, "refs/heads/main")], &pack);
    let walk = Walk::lowered(2);
    let (status, report) = held_while(&f, &p.url, &f.token, pushed_first.clone(), || {
        git(&["push", "-q", &p.url, ":gone"]);
    });
    drop(walk);
    assert_eq!(status, 200, "{report}");
    let told = "ng refs/heads/main a ref changed since it was read, and what it led to is more than is looked through";
    assert!(report.contains(told), "{report}");
    let listed = git(&["ls-remote", &p.url, "refs/heads/main"]);
    assert!(listed.starts_with(&p.main), "{listed}");
    let (status, report) = receive(&p.url, &f.token, &pushed_first);
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ok refs/heads/main"), "{report}");
}

/// A push to `main` names a file only `gone` holds. Before what the push may
/// have lost is found, `main` is moved to `gone` and `gone` is deleted: the
/// file is led to by `main` only. Once that is found, `main` is moved back to
/// where the push read it, and the push is published there, where the file is
/// led to by nothing. The push is refused: `main` is taken where the push read
/// it, which is where it is when the push is published.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_is_refused_where_only_the_ref_it_updates_led_to_what_it_names_while_its_losses_were_found()
 {
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    // a commit on `main` of the file of `gone`, without the file in its pack
    git(&["checkout", "-q", "--detach", &p.main]);
    std::fs::write(p.work.join("gone.txt"), "only gone has this\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "named"]);
    let named = git(&["rev-parse", "HEAD"]);
    let pack = packed(&p.work, &f.token, &[&named, &format!("{named}^{{tree}}")]);
    git(&["checkout", "-q", "main"]);
    let pushed_named = body(&[(&p.main, &named, "refs/heads/main")], &pack);

    let (verified, go_verified) = stop_at(&f, &alt_server_store::PUSH_VERIFIED);
    let pushing = {
        let (url, token) = (p.url.clone(), f.token.clone());
        std::thread::spawn(move || receive(&url, &token, &pushed_named))
    };
    verified.recv_timeout(Duration::from_secs(60)).unwrap();
    git(&["push", "-q", &p.url, "gone:refs/heads/main"]);
    git(&["push", "-q", &p.url, ":gone"]);
    // stopped only now, so that neither push above stops there
    let (found, go_found) = stop_at(&f, &alt_server_store::PUSH_FOUND);
    go_verified.send(()).unwrap();
    // a push refused once its losses are found does not come to the second stop
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if found.recv_timeout(Duration::from_millis(50)).is_ok() {
            let back = format!("{}:refs/heads/main", p.main);
            git(&["push", "-q", "-f", &p.url, &back]);
            go_found.send(()).unwrap();
            break;
        }
        if pushing.is_finished() {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
    }
    let (status, report) = pushing.join().unwrap();
    *alt_server_store::PUSH_VERIFIED.lock().unwrap() = None;
    *alt_server_store::PUSH_FOUND.lock().unwrap() = None;
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains("ng refs/heads/main a ref changed since it was read"),
        "{report}"
    );
    let listed = git(&["ls-remote", &p.url, "refs/heads/main"]);
    assert!(!listed.starts_with(&named), "{listed}");
}

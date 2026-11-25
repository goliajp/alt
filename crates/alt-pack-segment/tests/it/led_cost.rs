//! What a push costs of the walk that tells what the refs lead to, by the size
//! of the history under it.

use super::{git, oid, text};
use alt_pack_segment::{Error, Limits, Segment, SegmentSet, ingest};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

/// Commits `from..to` in a line onto `main`, written by `git fast-import`:
/// each changes one file of one of 64 directories, so that a commit brings a
/// blob and two trees with it. Gives the pack they were written to.
fn imported(source: &Path, from: usize, to: usize) -> PathBuf {
    let before: Vec<PathBuf> = packs(source);
    let mut import = Command::new("git")
        .current_dir(source)
        // a pack of its own for what is written, however little that is
        .args(["-c", "fastimport.unpackLimit=1"])
        .args(["fast-import", "--quiet", "--date-format=raw"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut to_git = std::io::BufWriter::new(import.stdin.take().unwrap());
    for n in from..to {
        let content = format!("revision {n} of this file\n");
        write!(to_git, "blob\nmark :1\ndata {}\n{content}", content.len()).unwrap();
        let message = format!("revision {n}\n");
        let who = format!("t <t@example.test> {} +0000", 1_700_000_000 + n);
        write!(to_git, "commit refs/heads/main\ncommitter {who}\n").unwrap();
        write!(to_git, "data {}\n{message}", message.len()).unwrap();
        if n == from && from > 0 {
            writeln!(to_git, "from refs/heads/main^0").unwrap();
        }
        writeln!(to_git, "M 100644 :1 d{}/f{}\n", n % 64, n / 64 % 64).unwrap();
    }
    drop(to_git);
    assert!(import.wait().unwrap().success());
    let new = |pack: &PathBuf| !before.contains(pack);
    let mut new: Vec<PathBuf> = packs(source).into_iter().filter(new).collect();
    assert_eq!(new.len(), 1);
    new.remove(0)
}

fn packs(source: &Path) -> Vec<PathBuf> {
    let read = std::fs::read_dir(source.join(".git/objects/pack")).unwrap();
    let paths = read.map(|entry| entry.unwrap().path());
    paths
        .filter(|path| path.extension().is_some_and(|is| is == "pack"))
        .collect()
}

fn segment_of(work: &Path, name: &str, pack: &Path, behind: &SegmentSet) -> Arc<Segment> {
    let limits = Limits::of(1 << 26, 1 << 30, 1 << 40);
    let pack = std::fs::read(pack).unwrap();
    let ingested = ingest(&pack, behind, &limits).unwrap();
    let path = work.join(name);
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&pack).unwrap();
    ingested.write_index(&mut file).unwrap();
    Arc::new(Segment::open(&path).unwrap())
}

/// A history of `ALT_LED_COMMITS` commits in a line, 20,000 where nothing
/// says how many, and a push of three commits onto it. What the tip led to
/// before the push is found from nothing, which reads every tree; what it
/// leads to after it is found from that by reading the six trees the push
/// brought, however long the history under them is. The times it took are
/// written out, for whoever runs this with a longer history.
#[test]
#[ignore = "a measurement that writes a history with git fast-import: run by name, in release for its times"]
fn the_walk_of_a_push_reads_what_the_push_brought_however_long_the_history_is() {
    let commits = std::env::var("ALT_LED_COMMITS").map_or(20_000, |told| told.parse().unwrap());
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "--initial-branch=main"]);
    let began = Instant::now();
    let history = imported(&source, 0, commits);
    let old = oid(&text(git(&source, &["rev-parse", "main"])));
    let root = oid(&text(git(
        &source,
        &["rev-list", "--max-parents=0", "main"],
    )));
    let pushed = imported(&source, commits, commits + 3);
    let new = oid(&text(git(&source, &["rev-parse", "main"])));
    let written = began.elapsed();
    let began = Instant::now();
    let under = SegmentSet::new(vec![segment_of(
        work.path(),
        "one",
        &history,
        &SegmentSet::default(),
    )]);
    let brought = segment_of(work.path(), "two", &pushed, &under);
    let set = SegmentSet::new(vec![under.segments()[0].clone(), brought]);
    let ingested = began.elapsed();

    let began = Instant::now();
    let before = set.led_to(&[old], usize::MAX, None).unwrap();
    let from_nothing = began.elapsed();
    // a commit, its file and the two trees above it
    assert_eq!(before.len(), commits * 4);
    assert!(matches!(set.led_to(&[old], 6, None), Err(Error::Limit(_))));

    let began = Instant::now();
    assert!(set.lead_to_all(&[new], &[old], usize::MAX).unwrap());
    let below = began.elapsed();
    let began = Instant::now();
    let (after, ended) = set.led_beyond(before.clone(), &[new], 6, None);
    let of_the_push = began.elapsed();
    ended.unwrap();
    assert_eq!(after.len(), (commits + 3) * 4);
    assert!(after.to(&new) && !before.to(&new));

    // a branch at the first commit is deleted: every commit is gone through
    // to find that the tip leads to it, and no tree is read
    let began = Instant::now();
    assert!(set.lead_to_all(&[new], &[root], usize::MAX).unwrap());
    let to_the_first = began.elapsed();
    let began = Instant::now();
    assert!(set.all_led(&after).unwrap() && !set.all_led(&before).unwrap());
    let all = began.elapsed();
    println!(
        "led cost: commits {commits} objects {} kept_bytes {} written_ms {} ingested_ms {} from_nothing_ms {} push_below_us {} push_walk_us {} to_the_first_commit_ms {} all_led_ms {}",
        after.len(),
        after.len() * 20,
        written.as_millis(),
        ingested.as_millis(),
        from_nothing.as_millis(),
        below.as_micros(),
        of_the_push.as_micros(),
        to_the_first.as_millis(),
        all.as_millis(),
    );
}

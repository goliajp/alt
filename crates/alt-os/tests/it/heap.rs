//! Whether freed memory stays with a process as `alt_os::heap` sets it
//! up. What tells is how much of the memory a round of blocks holds is
//! resident no longer once they are freed: the same whatever the size of
//! a page, and whether or not the kernel backs the blocks with huge
//! pages, which the number of page faults is not. Each way of setting the
//! heap up is measured in a process of its own.

use std::io::Read;
use std::process::Command;

use super::CHILD;

const ROUNDS: u64 = 30;

/// Minor page faults of this process so far.
fn faults() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    // the field after the command name's closing parenthesis is number 3
    let rest = &stat[stat.rfind(')').unwrap() + 2..];
    rest.split(' ').nth(7).unwrap().parse().unwrap()
}

/// The number that follows `before` in the file at `path`. Read without
/// touching the heap: a small block allocated above the ones being
/// measured would hold them there.
fn told(path: &str, before: &str) -> u64 {
    let mut buf = [0u8; 4096];
    let mut file = std::fs::File::open(path).unwrap();
    let len = file.read(&mut buf).unwrap();
    let text = std::str::from_utf8(&buf[..len]).unwrap();
    let after = &text[text.find(before).unwrap() + before.len()..];
    let digits = after.trim_start();
    let end = digits.find(|c: char| !c.is_ascii_digit());
    digits[..end.unwrap_or(digits.len())].parse().unwrap()
}

/// Resident pages of this process.
fn resident() -> u64 {
    told("/proc/self/statm", " ")
}

/// Allocates and frees the same `blocks` blocks of `size` bytes round
/// after round, which is what a daemon does request after request. In
/// how many rounds freeing them gave back more than half of what they
/// held, the page faults all of it took, and the most of the process
/// that lay in huge pages, in kibibytes.
fn churn(blocks: usize, size: usize) -> [u64; 3] {
    let (base, before) = (resident(), faults());
    let (mut gave, mut huge) = (0, 0);
    for _ in 0..ROUNDS {
        // never zero: a zeroed block may be handed out without being touched
        let blocks: Vec<Vec<u8>> = (0..blocks).map(|n| vec![n as u8 | 1; size]).collect();
        std::hint::black_box(&blocks);
        let held = resident();
        huge = huge.max(told("/proc/self/smaps_rollup", "AnonHugePages:"));
        drop(blocks);
        let back = held.saturating_sub(resident());
        gave += u64::from(back > held.saturating_sub(base) / 2);
    }
    [gave, faults() - before, huge]
}

/// Fills `blocks` blocks of `size` bytes and frees them: how many pages
/// that gave back to the system.
fn given_back(blocks: usize, size: usize) -> u64 {
    let filled: Vec<Vec<u8>> = (0..blocks).map(|n| vec![n as u8 | 1; size]).collect();
    std::hint::black_box(&filled);
    let held = resident();
    drop(filled);
    held.saturating_sub(resident())
}

/// For a mode that measures the heap: sets the heap up as the mode says,
/// prints what it measures and ends the process.
pub fn measure(mode: &str) {
    match mode {
        "retain" | "kept-heap" => alt_os::heap::keep_freed_memory(),
        // the limits altd sets
        m if m.starts_with("daemon") && m != "daemon-control" => {
            alt_os::heap::keep_freed_up_to(4 << 20, 32 << 20)
        }
        _ => {}
    }
    let measured = match mode {
        "retain" | "release" => churn(256, 64 << 10).to_vec(),
        "daemon" | "daemon-control" => churn(8, 2 << 20).to_vec(),
        "daemon-heap" | "kept-heap" => vec![given_back(24, 2 << 20)],
        "daemon-block" => vec![given_back(1, 48 << 20)],
        _ => return,
    };
    let measured: Vec<String> = measured.iter().map(u64::to_string).collect();
    println!("{}", measured.join(" "));
    std::process::exit(0);
}

/// What a process of this binary measures in `mode`.
fn measured(mode: &str) -> Vec<u64> {
    let out = Command::new(std::env::current_exe().unwrap())
        .env(CHILD, mode)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .map(|number| number.parse().unwrap())
        .collect()
}

pub fn check() {
    let huge_pages = std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled");
    let huge_pages = huge_pages.unwrap_or_default();
    // 16 MiB a round: kept, no round gives them back when they are freed;
    // with the limits where they start, every round does; the second
    // pair is the daemon's limits, 4 MiB a block and 32 MiB of free heap,
    // under which eight blocks of 2 MiB stay like the 16 MiB of the first
    for (keeps, control) in [("retain", "release"), ("daemon", "daemon-control")] {
        let (kept, released) = (measured(keeps), measured(control));
        let numbers = format!(
            "{keeps} {kept:?} against {control} {released:?}: of {ROUNDS} rounds those that \
             gave memory back, page faults, KiB in huge pages (transparent_hugepage: {})",
            huge_pages.trim()
        );
        assert!(kept[0] == 0, "freed memory was not kept: {numbers}");
        assert!(
            released[0] == ROUNDS,
            "the control kept its memory: {numbers}"
        );
        println!("freed memory kept: {numbers}");
    }
    // 48 MiB are 12288 pages, and more than the daemon keeps: freed from
    // the heap or as one mapped block, they go back at once, where the
    // limits of a short-lived command keep them
    let given = |mode: &str| measured(mode)[0];
    let (heap, block, held) = (
        given("daemon-heap"),
        given("daemon-block"),
        given("kept-heap"),
    );
    assert!(heap > 9000, "a large free heap stayed: {heap} pages back");
    assert!(block > 9000, "a large block stayed: {block} pages back");
    assert!(held < 3000, "the control gave its heap back: {held} pages");
    println!("daemon limits: {heap} and {block} pages given back against {held}");
}

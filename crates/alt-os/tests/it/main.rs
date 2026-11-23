//! Single integration-test binary for alt-os. It runs without the test
//! harness and is entered through the C `main`, like the program the stack
//! overflow report is for: the standard library's own handler, which would
//! print the same message, is never installed here.
#![no_main]

/// Lists the tests of a module: there is no harness to collect `#[test]`
/// functions, so `main` calls the ones listed. A test left out of its
/// module's list is dead code, which the lints refuse.
macro_rules! tests {
    ($($(#[$cfg:meta])* $name:ident),* $(,)?) => {
        pub const TESTS: &[(&str, fn())] = &[$($(#[$cfg])* (stringify!($name), $name)),*];
    };
}

mod holes;
mod layout;
mod map;
mod random;
mod sys;
mod temp;

use std::process::{Command, Stdio};

const CHILD: &str = "ALT_OS_TEST_CHILD";

#[inline(never)]
fn recurse(depth: u64) -> u64 {
    let pad = [depth; 64];
    if std::hint::black_box(depth) == u64::MAX {
        return 0;
    }
    recurse(depth + 1) + std::hint::black_box(pad)[7]
}

/// Minor page faults of this process so far.
#[cfg(target_os = "linux")]
fn faults() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    // the field after the command name's closing parenthesis is number 3
    let rest = &stat[stat.rfind(')').unwrap() + 2..];
    rest.split(' ').nth(7).unwrap().parse().unwrap()
}

/// Allocates and frees the same `blocks` blocks of `size` bytes, `rounds`
/// times over, and reports the page faults that took. This is what a
/// daemon does request after request.
#[cfg(target_os = "linux")]
fn churn(rounds: usize, blocks: usize, size: usize) -> u64 {
    let before = faults();
    for _ in 0..rounds {
        // never zero: a zeroed block may be handed out without being touched
        let blocks: Vec<Vec<u8>> = (0..blocks).map(|n| vec![n as u8 | 1; size]).collect();
        std::hint::black_box(&blocks);
    }
    faults() - before
}

/// Resident pages of this process. Read without touching the heap: a small
/// block allocated above the ones being measured would hold them there.
#[cfg(target_os = "linux")]
fn resident() -> u64 {
    use std::io::Read;
    let mut buf = [0u8; 128];
    let mut statm = std::fs::File::open("/proc/self/statm").unwrap();
    let len = statm.read(&mut buf).unwrap();
    let text = std::str::from_utf8(&buf[..len]).unwrap();
    text.split(' ').nth(1).unwrap().parse().unwrap()
}

/// Fills `blocks` blocks of `size` bytes and frees them: how many pages
/// that gave back to the system.
#[cfg(target_os = "linux")]
fn given_back(blocks: usize, size: usize) -> u64 {
    let filled: Vec<Vec<u8>> = (0..blocks).map(|n| vec![n as u8 | 1; size]).collect();
    std::hint::black_box(&filled);
    let held = resident();
    drop(filled);
    held.saturating_sub(resident())
}

/// For a mode that measures the heap: sets the heap up as the mode says,
/// prints what it measures and ends the process.
#[cfg(target_os = "linux")]
fn measure_heap(mode: &str) {
    match mode {
        "retain" | "kept-heap" => alt_os::heap::keep_freed_memory(),
        // the limits altd sets
        m if m.starts_with("daemon") && m != "daemon-control" => {
            alt_os::heap::keep_freed_up_to(4 << 20, 32 << 20)
        }
        _ => {}
    }
    let measured = match mode {
        "retain" | "release" => Some(churn(30, 256, 64 << 10)),
        "daemon" | "daemon-control" => Some(churn(30, 8, 2 << 20)),
        "daemon-heap" | "kept-heap" => Some(given_back(24, 2 << 20)),
        "daemon-block" => Some(given_back(1, 48 << 20)),
        _ => None,
    };
    if let Some(measured) = measured {
        println!("{measured}");
        std::process::exit(0);
    }
}

fn child(mode: &str) -> ! {
    if mode == "setrlimit" {
        sys::setrlimit_child();
        std::process::exit(0);
    }
    #[cfg(target_os = "linux")]
    measure_heap(mode);
    if mode == "bare" {
        std::hint::black_box(recurse(0));
    }
    alt_os::stack::report_overflow();
    match mode {
        "overflow" => {
            std::hint::black_box(recurse(0));
        }
        "worker" | "uncovered" => {
            let covered = mode == "worker";
            let worker = std::thread::Builder::new().name("worker".into());
            let run = move || {
                if covered {
                    alt_os::stack::cover_thread();
                }
                std::hint::black_box(recurse(0))
            };
            let _ = worker.spawn(run).unwrap().join();
        }
        "ended" => {
            // a covered thread that ends leaves the process as it was
            for _ in 0..8 {
                let thread = std::thread::spawn(alt_os::stack::cover_thread);
                thread.join().unwrap();
            }
        }
        "wild" => {
            // a fault far from the stack keeps its ordinary meaning
            let nowhere = std::hint::black_box(8usize) as *mut u8;
            unsafe { nowhere.write_volatile(1) };
        }
        _ => {}
    }
    std::process::exit(0)
}

fn run(mode: &str) -> (Option<i32>, String) {
    finish(Command::new(std::env::current_exe().unwrap()).env(CHILD, mode))
}

/// The main thread overflowing a stack with no size limit, in an address
/// space small enough for the stack to run into its end.
#[cfg(target_os = "linux")]
fn run_unlimited() -> (Option<i32>, String) {
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn setrlimit(resource: std::ffi::c_int, limit: *const [u64; 2]) -> std::ffi::c_int;
    }
    const RLIMIT_STACK: std::ffi::c_int = 3;
    const RLIMIT_AS: std::ffi::c_int = 9;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.env(CHILD, "overflow");
    // SAFETY: setrlimit is async-signal-safe and the arrays are constants
    unsafe {
        command.pre_exec(|| {
            setrlimit(RLIMIT_STACK, &[u64::MAX; 2]);
            setrlimit(RLIMIT_AS, &[512 << 20; 2]);
            Ok(())
        });
    }
    finish(&mut command)
}

fn finish(command: &mut Command) -> (Option<i32>, String) {
    use std::os::unix::process::ExitStatusExt;
    let out = command.stdin(Stdio::null()).output().unwrap();
    (
        out.status.signal(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn main(
    _argc: std::ffi::c_int,
    _argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    if let Ok(mode) = std::env::var(CHILD) {
        child(&mode);
    }
    let suites = [
        ("holes", holes::TESTS),
        ("layout", layout::TESTS),
        ("map", map::TESTS),
        ("random", random::TESTS),
        ("sys", sys::TESTS),
        ("temp", temp::TESTS),
    ];
    for (module, tests) in suites {
        for (name, test) in tests {
            // a test that fails panics, which ends the process here
            test();
            println!("test {module}::{name} ... ok");
        }
    }
    let (signal, stderr) = run("overflow");
    assert!(
        stderr.contains("thread 'main' has overflowed its stack"),
        "no overflow report: {stderr:?}"
    );
    assert_eq!(signal, Some(6), "an overflow ends in an abort");

    let (signal, stderr) = run("worker");
    assert!(
        stderr.contains("thread 'worker' has overflowed its stack"),
        "no overflow report for a worker thread: {stderr:?}"
    );
    assert_eq!(signal, Some(6), "an overflow ends in an abort");
    // the control: a thread that was not given a signal stack just dies
    let (signal, stderr) = run("uncovered");
    assert!(!stderr.contains("overflowed"), "{stderr:?}");
    assert!(
        matches!(signal, Some(11) | Some(10) | Some(7) | Some(4)),
        "got {signal:?}"
    );
    let (signal, _) = run("ended");
    assert_eq!(signal, None, "covered threads come and go");

    #[cfg(target_os = "linux")]
    {
        let (signal, stderr) = run_unlimited();
        assert!(
            stderr.contains("thread 'main' has overflowed its stack"),
            "no overflow report with an unlimited stack: {stderr:?}"
        );
        assert_eq!(signal, Some(6));
    }

    let (signal, stderr) = run("wild");
    assert!(!stderr.contains("overflowed"), "{stderr:?}");
    assert!(
        matches!(signal, Some(11) | Some(10) | Some(7)),
        "an unrelated fault keeps its signal, got {signal:?}"
    );

    let (signal, stderr) = run("bare");
    assert!(!stderr.contains("overflowed"), "{stderr:?}");
    assert!(
        matches!(signal, Some(11) | Some(10) | Some(7)),
        "got {signal:?}"
    );

    let (signal, _) = run("quiet");
    assert_eq!(signal, None, "installing the handler disturbs nothing");
    println!("stack overflow report: ok");

    // 16 MiB a round is 4096 pages: kept, they are faulted in once;
    // released at the end of each round, thirty times
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let faults = |mode: &str| -> u64 {
            let out = Command::new(std::env::current_exe().unwrap())
                .env(CHILD, mode)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
        };
        let (kept, released) = (faults("retain"), faults("release"));
        assert!(kept < 2 * 4096, "freed memory was not kept: {kept} faults");
        assert!(
            released > 10 * 4096,
            "the control kept its memory: {released} faults"
        );
        println!("freed memory kept: {kept} faults against {released}");

        // the daemon's limits, 4 MiB a block and 32 MiB of free heap: eight
        // blocks of 2 MiB stay like the 16 MiB above
        let (kept, released) = (faults("daemon"), faults("daemon-control"));
        assert!(kept < 2 * 4096, "freed blocks were not kept: {kept} faults");
        assert!(
            released > 10 * 4096,
            "the control kept its blocks: {released} faults"
        );
        // 48 MiB are 12288 pages, and more than the daemon keeps: freed
        // from the heap or as one mapped block, they go back at once, where
        // the limits of a short-lived command keep them
        let given = |mode: &str| faults(mode);
        let (heap, block, held) = (
            given("daemon-heap"),
            given("daemon-block"),
            given("kept-heap"),
        );
        assert!(heap > 9000, "a large free heap stayed: {heap} pages back");
        assert!(block > 9000, "a large block stayed: {block} pages back");
        assert!(held < 3000, "the control gave its heap back: {held} pages");
        println!(
            "daemon limits: {kept} faults against {released}; {heap} and {block} pages given back against {held}"
        );
    }
    0
}

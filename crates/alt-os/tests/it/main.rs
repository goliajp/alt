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

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod heap;
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

fn child(mode: &str) -> ! {
    if mode == "setrlimit" {
        sys::setrlimit_child();
        std::process::exit(0);
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    heap::measure(mode);
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

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    heap::check();
    0
}

//! Single integration-test binary for alt-os. It runs without the test
//! harness and is entered through the C `main`, like the program the stack
//! overflow report is for: the standard library's own handler, which would
//! print the same message, is never installed here.
#![no_main]

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

/// Allocates and frees the same few megabytes, in blocks below the size
/// the allocator maps one by one, `rounds` times over, and reports the
/// page faults that took. This is what a daemon does request after request.
#[cfg(target_os = "linux")]
fn churn(rounds: usize) -> u64 {
    let before = faults();
    for _ in 0..rounds {
        let blocks: Vec<Vec<u8>> = (0..256).map(|n| vec![n as u8; 64 << 10]).collect();
        std::hint::black_box(&blocks);
    }
    faults() - before
}

fn child(mode: &str) -> ! {
    #[cfg(target_os = "linux")]
    if mode == "retain" || mode == "release" {
        if mode == "retain" {
            alt_os::heap::keep_freed_memory();
        }
        println!("{}", churn(30));
        std::process::exit(0);
    }
    if mode == "bare" {
        std::hint::black_box(recurse(0));
    }
    alt_os::stack::report_overflow();
    match mode {
        "overflow" => {
            std::hint::black_box(recurse(0));
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
    use std::os::unix::process::ExitStatusExt;
    let out = Command::new(std::env::current_exe().unwrap())
        .env(CHILD, mode)
        .stdin(Stdio::null())
        .output()
        .unwrap();
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
    let (signal, stderr) = run("overflow");
    assert!(
        stderr.contains("thread 'main' has overflowed its stack"),
        "no overflow report: {stderr:?}"
    );
    assert_eq!(signal, Some(6), "an overflow ends in an abort");

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
    }
    0
}

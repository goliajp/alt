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

fn child(mode: &str) -> ! {
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
    0
}

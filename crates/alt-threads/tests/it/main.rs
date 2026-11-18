//! Single integration-test binary for alt-threads. It runs without the
//! test harness and is entered through the C `main`, like the programs
//! whose pool threads are covered against a stack overflow: under the
//! standard runtime every thread is covered already, and a pool that left
//! its threads uncovered would look the same.
#![no_main]

/// Lists the tests of a module: there is no harness to collect `#[test]`
/// functions, so `main` calls the ones listed.
macro_rules! tests {
    ($($name:ident),* $(,)?) => {
        pub const TESTS: &[(&str, fn())] = &[$((stringify!($name), $name)),*];
    };
}

mod slices;

use alt_threads::join;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Set for the copy of this program that overflows a pool thread.
const CHILD: &str = "ALT_THREADS_TEST_CHILD";

fn sum(values: &[u64]) -> u64 {
    if values.len() <= 1000 {
        return values.iter().sum();
    }
    let (left, right) = values.split_at(values.len() / 2);
    let (left, right) = join(|| sum(left), || sum(right));
    left + right
}

fn nested_joins_compute_the_same_result() {
    let values: Vec<u64> = (0..2_000_000).collect();
    for _ in 0..20 {
        assert_eq!(sum(&values), 1_999_999 * 2_000_000 / 2);
    }
}

fn many_threads_can_join_at_once() {
    let done = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..32 {
            scope.spawn(|| {
                let values: Vec<u64> = (0..100_000).collect();
                for _ in 0..50 {
                    assert_eq!(sum(&values), 99_999 * 100_000 / 2);
                }
                done.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    assert_eq!(done.load(Ordering::Relaxed), 32);
}

fn a_panic_in_either_half_comes_back_after_both_ran() {
    let ran = AtomicUsize::new(0);
    let caught = std::panic::catch_unwind(|| {
        join(
            || {
                ran.fetch_add(1, Ordering::Relaxed);
            },
            || {
                ran.fetch_add(1, Ordering::Relaxed);
                panic!("second half");
            },
        )
    });
    assert_eq!(
        *caught.unwrap_err().downcast::<&str>().unwrap(),
        "second half"
    );
    let caught = std::panic::catch_unwind(|| {
        join(
            || panic!("first half"),
            || {
                ran.fetch_add(1, Ordering::Relaxed);
            },
        )
    });
    assert_eq!(
        *caught.unwrap_err().downcast::<&str>().unwrap(),
        "first half"
    );
    assert_eq!(ran.load(Ordering::Relaxed), 3);
    // the pool still works afterwards
    assert_eq!(join(|| 1, || 2), (1, 2));
}

tests!(
    nested_joins_compute_the_same_result,
    many_threads_can_join_at_once,
    a_panic_in_either_half_comes_back_after_both_ran,
);

#[inline(never)]
fn recurse(depth: u64) -> u64 {
    let pad = [depth; 64];
    if std::hint::black_box(depth) == u64::MAX {
        return 0;
    }
    recurse(depth + 1) + std::hint::black_box(pad)[7]
}

/// The second half of a join never returns from its recursion, on the
/// pool thread that picks it up while the first half keeps this one.
fn overflow_a_pool_thread() -> ! {
    alt_os::stack::report_overflow();
    join(
        || std::thread::sleep(std::time::Duration::from_secs(30)),
        || std::hint::black_box(recurse(0)),
    );
    std::process::exit(0)
}

/// A pool thread that overflows its stack says so and aborts, as the
/// main thread does.
fn a_pool_thread_that_overflows_its_stack_says_so() {
    use std::os::unix::process::ExitStatusExt;
    let mut child = Command::new(std::env::current_exe().unwrap());
    let out = child.env(CHILD, "1").stdin(Stdio::null()).output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("thread 'alt-worker-") && said.contains("has overflowed its stack"),
        "no overflow report for a pool thread: {said:?}"
    );
    assert_eq!(out.status.signal(), Some(6), "an overflow ends in an abort");
}

#[unsafe(no_mangle)]
pub extern "C" fn main(
    _argc: std::ffi::c_int,
    _argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    if std::env::var_os(CHILD).is_some() {
        overflow_a_pool_thread();
    }
    for (module, tests) in [("main", TESTS), ("slices", slices::TESTS)] {
        for (name, test) in tests {
            // a test that fails panics, which ends the process here
            test();
            println!("test {module}::{name} ... ok");
        }
    }
    // with one processor the pool has no thread besides this one
    if std::thread::available_parallelism().is_ok_and(|count| count.get() > 1) {
        a_pool_thread_that_overflows_its_stack_says_so();
        println!("test main::a_pool_thread_that_overflows_its_stack_says_so ... ok");
    }
    0
}

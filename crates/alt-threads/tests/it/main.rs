use alt_threads::join;
use std::sync::atomic::{AtomicUsize, Ordering};

fn sum(values: &[u64]) -> u64 {
    if values.len() <= 1000 {
        return values.iter().sum();
    }
    let (left, right) = values.split_at(values.len() / 2);
    let (left, right) = join(|| sum(left), || sum(right));
    left + right
}

#[test]
fn nested_joins_compute_the_same_result() {
    let values: Vec<u64> = (0..2_000_000).collect();
    for _ in 0..20 {
        assert_eq!(sum(&values), 1_999_999 * 2_000_000 / 2);
    }
}

#[test]
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

#[test]
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

use alt_threads::{any, for_each, map, map_owned, try_for_each};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn map_keeps_the_order_of_the_elements() {
    let items: Vec<u32> = (0..100_003).collect();
    for grain in [0, 1, 7, 1000, 1 << 20] {
        let doubled = map(&items, grain, |item| u64::from(*item) * 2);
        assert_eq!(doubled.len(), items.len());
        assert!(
            doubled
                .iter()
                .enumerate()
                .all(|(at, value)| *value == at as u64 * 2)
        );
    }
    assert!(map(&[] as &[u32], 1, |item| *item).is_empty());
}

#[test]
fn map_owned_hands_over_every_element_once() {
    let live = Arc::new(());
    let items: Vec<(usize, Arc<()>)> = (0..10_001).map(|at| (at, live.clone())).collect();
    let indexes = map_owned(items, 1, |(at, handle)| {
        drop(handle);
        at
    });
    assert!(indexes.iter().copied().eq(0..10_001));
    // every clone was dropped exactly once: a double drop would have aborted
    assert_eq!(Arc::strong_count(&live), 1);
    assert!(map_owned(Vec::<String>::new(), 1, |text| text).is_empty());
}

#[test]
fn map_results_with_destructors_are_whole() {
    let items: Vec<usize> = (0..5000).collect();
    let texts = map(&items, 3, |item| format!("item {item}"));
    assert!(
        texts
            .iter()
            .enumerate()
            .all(|(at, text)| *text == format!("item {at}"))
    );
}

#[test]
fn for_each_visits_every_element_once() {
    let items: Vec<usize> = (0..50_000).collect();
    let visits: Vec<AtomicUsize> = items.iter().map(|_| AtomicUsize::new(0)).collect();
    for_each(&items, 16, |item| {
        visits[*item].fetch_add(1, Ordering::Relaxed);
    });
    assert!(
        visits
            .iter()
            .all(|count| count.load(Ordering::Relaxed) == 1)
    );
}

#[test]
fn any_finds_a_single_match_and_reports_none() {
    let items: Vec<u32> = (0..100_000).collect();
    for wanted in [0, 1, 49_999, 99_999] {
        assert!(any(&items, 64, |item| *item == wanted));
    }
    assert!(!any(&items, 64, |item| *item == 100_000));
    assert!(!any(&[] as &[u32], 1, |_| true));
}

#[test]
fn a_panic_in_the_work_reaches_the_caller() {
    let items: Vec<u32> = (0..1000).collect();
    let outcome = std::panic::catch_unwind(|| {
        map(&items, 1, |item| {
            assert_ne!(*item, 777, "the element that fails");
            item.to_string()
        })
    });
    assert!(outcome.is_err());
}

#[test]
fn try_for_each_returns_a_failure_or_visits_everything() {
    let items: Vec<u32> = (0..100_000).collect();
    let visited = AtomicUsize::new(0);
    let outcome: Result<(), u32> = try_for_each(&items, 32, |_| {
        visited.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    assert_eq!(outcome, Ok(()));
    assert_eq!(visited.load(Ordering::Relaxed), items.len());
    for bad in [0, 31_337, 99_999] {
        let outcome = try_for_each(
            &items,
            32,
            |item| if *item == bad { Err(*item) } else { Ok(()) },
        );
        assert_eq!(outcome, Err(bad));
    }
}

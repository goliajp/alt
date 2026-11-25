//! The bytes each thread holds, for the tests that bound what a read takes,
//! and an end for a test that would otherwise go on: one that came back to
//! reading round in a circle ends the tests, and takes no memory with it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct Counting;

thread_local! {
    static HELD: Cell<usize> = const { Cell::new(0) };
    static MOST: Cell<usize> = const { Cell::new(0) };
    static MAY: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// whether the thread may take `bytes` more
fn took(bytes: usize) -> bool {
    HELD.try_with(|held| {
        held.set(held.get() + bytes);
        let _ = MOST.try_with(|most| most.set(most.get().max(held.get())));
        MAY.try_with(|may| held.get() <= may.get()).unwrap_or(true)
    })
    .unwrap_or(true)
}

fn gave(bytes: usize) {
    let _ = HELD.try_with(|held| held.set(held.get().saturating_sub(bytes)));
}

// SAFETY: a request is passed to the system allocator unchanged, or refused
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !took(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller's layout
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        gave(layout.size());
        // SAFETY: the caller's block and layout
        unsafe { System.dealloc(block, layout) }
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        gave(layout.size());
        if !took(size) {
            took(layout.size());
            return std::ptr::null_mut();
        }
        // SAFETY: the caller's block, layout and size
        unsafe { System.realloc(block, layout, size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The most bytes this thread held at once while `run` ran, above what it
/// held before.
pub(crate) fn most_held<T>(run: impl FnOnce() -> T) -> (usize, T) {
    let before = HELD.get();
    MOST.set(before);
    let ran = run();
    (MOST.get().saturating_sub(before), ran)
}

/// Until it is dropped, this thread is given no more than `bytes` above
/// what it holds now: an allocation beyond that fails, which ends the tests.
pub(crate) struct Bound(usize);

pub(crate) fn no_more_than(bytes: usize) -> Bound {
    Bound(MAY.replace(HELD.get() + bytes))
}

impl Drop for Bound {
    fn drop(&mut self) {
        MAY.set(self.0);
    }
}

/// Ends the tests where the test that holds this has not ended in `seconds`.
pub(crate) struct Ended(Arc<AtomicBool>);

pub(crate) fn ended_within(seconds: u64) -> Ended {
    let ended = Arc::new(AtomicBool::new(false));
    let watched = Arc::clone(&ended);
    std::thread::spawn(move || {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < until {
            if watched.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        eprintln!("a test did not end in {seconds} seconds");
        std::process::abort();
    });
    Ended(ended)
}

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

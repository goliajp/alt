use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod hostile;
mod limits;
mod pathological;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod regcomp;

/// Counts the bytes each thread holds, for the tests that bound memory.
struct Counting;

thread_local! {
    static HELD: Cell<usize> = const { Cell::new(0) };
    static MOST: Cell<usize> = const { Cell::new(0) };
}

fn took(bytes: usize) {
    let _ = HELD.try_with(|held| {
        held.set(held.get() + bytes);
        let _ = MOST.try_with(|most| most.set(most.get().max(held.get())));
    });
}

fn gave(bytes: usize) {
    let _ = HELD.try_with(|held| held.set(held.get().saturating_sub(bytes)));
}

// SAFETY: every request is passed to the system allocator unchanged
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        took(layout.size());
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
        took(size);
        // SAFETY: the caller's block, layout and size
        unsafe { System.realloc(block, layout, size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The most bytes this thread held at once while `run` ran, above what
/// it held before.
fn most_held(run: impl FnOnce()) -> usize {
    let before = HELD.get();
    MOST.set(before);
    run();
    MOST.get().saturating_sub(before)
}

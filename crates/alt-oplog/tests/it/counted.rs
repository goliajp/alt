//! How much memory a thread holds at once while it runs a piece of code.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static HELD: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn add(bytes: isize) {
    let _ = HELD.try_with(|held| {
        held.set(held.get() + bytes);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(held.get())));
    });
}

// SAFETY: every call is passed on to the system allocator unchanged
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size() as isize);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        add(-(layout.size() as isize));
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        add(new_size as isize - layout.size() as isize);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Runs `run` and tells the most memory this thread held at once while
/// it ran, beyond what it held before.
pub fn peak<T>(run: impl FnOnce() -> T) -> (T, usize) {
    HELD.set(0);
    PEAK.set(0);
    let out = run();
    (out, PEAK.get().max(0) as usize)
}

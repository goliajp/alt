use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod differences;
mod held;
mod hostile;
mod ideal;
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

/// The processor time this thread has used so far. A limit on what a
/// search or a compilation may take is held against this: the time on
/// the clock is also that of whatever else the machine is doing.
fn thread_time() -> std::time::Duration {
    // as the c library lays it out where a pointer has 64 bits
    #[repr(C)]
    struct Timespec {
        seconds: i64,
        nanoseconds: std::ffi::c_long,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
    }
    const THREAD_CPUTIME: i32 = if cfg!(target_os = "macos") { 16 } else { 3 };
    let mut time = Timespec {
        seconds: 0,
        nanoseconds: 0,
    };
    // SAFETY: the call writes one timespec, and `time` is one
    let failed = unsafe { clock_gettime(THREAD_CPUTIME, &mut time) };
    assert_eq!(failed, 0, "the clock of this thread cannot be read");
    std::time::Duration::new(time.seconds as u64, time.nanoseconds as u32)
}

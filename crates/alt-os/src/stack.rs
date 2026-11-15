//! Telling a stack overflow of the main thread apart from any other fault.
//!
//! A program entered through the C `main` gets no report from the standard
//! library when its main thread runs out of stack: it just dies of SIGSEGV.
//! [`report_overflow`] installs a handler, on a signal stack of its own,
//! that prints the usual message and aborts when the faulting address lies
//! where that thread's stack ends, and steps aside for every other fault.
//! It costs three or four system calls and reads no files.

use core::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(target_os = "linux")]
mod os {
    use core::ffi::{c_int, c_ulong, c_void};

    pub const SIGBUS: c_int = 7;
    pub const SA_SIGINFO: c_int = 4;
    pub const SA_ONSTACK: c_int = 0x0800_0000;
    pub const RLIMIT_STACK: u32 = 3;
    pub const RLIM_INFINITY: u64 = u64::MAX;
    /// offset of `si_addr` in `siginfo_t`
    pub const SI_ADDR: usize = 16;

    #[repr(C)]
    pub struct sigaction {
        pub sa_sigaction: usize,
        pub sa_mask: [c_ulong; 16],
        pub sa_flags: c_int,
        pub sa_restorer: usize,
    }

    #[repr(C)]
    pub struct stack_t {
        pub ss_sp: *mut c_void,
        pub ss_flags: c_int,
        pub ss_size: usize,
    }

    pub fn action(handler: usize) -> sigaction {
        sigaction {
            sa_sigaction: handler,
            sa_mask: [0; 16],
            sa_flags: SA_SIGINFO | SA_ONSTACK,
            sa_restorer: 0,
        }
    }

    pub fn signal_stack(sp: *mut c_void, size: usize) -> stack_t {
        stack_t {
            ss_sp: sp,
            ss_flags: 0,
            ss_size: size,
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use core::ffi::{c_int, c_void};

    pub const SIGBUS: c_int = 10;
    pub const SA_SIGINFO: c_int = 0x40;
    pub const SA_ONSTACK: c_int = 0x1;
    /// offset of `si_addr` in `siginfo_t`
    pub const SI_ADDR: usize = 24;

    #[repr(C)]
    pub struct sigaction {
        pub sa_sigaction: usize,
        pub sa_mask: u32,
        pub sa_flags: c_int,
    }

    #[repr(C)]
    pub struct stack_t {
        pub ss_sp: *mut c_void,
        pub ss_size: usize,
        pub ss_flags: c_int,
    }

    pub fn action(handler: usize) -> sigaction {
        sigaction {
            sa_sigaction: handler,
            sa_mask: 0,
            sa_flags: SA_SIGINFO | SA_ONSTACK,
        }
    }

    pub fn signal_stack(sp: *mut c_void, size: usize) -> stack_t {
        stack_t {
            ss_sp: sp,
            ss_size: size,
            ss_flags: 0,
        }
    }
}

const SIGSEGV: c_int = 11;
const SIGNAL_STACK: usize = 64 << 10;

unsafe extern "C" {
    fn sigaction(signum: c_int, act: *const os::sigaction, old: *mut os::sigaction) -> c_int;
    fn sigaltstack(ss: *const os::stack_t, old: *mut os::stack_t) -> c_int;
    fn signal(signum: c_int, handler: usize) -> usize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn abort() -> !;
    #[cfg(target_os = "linux")]
    fn getrlimit(resource: u32, rlim: *mut [u64; 2]) -> c_int;
    #[cfg(target_os = "macos")]
    fn pthread_self() -> usize;
    #[cfg(target_os = "macos")]
    fn pthread_get_stackaddr_np(thread: usize) -> *mut c_void;
    #[cfg(target_os = "macos")]
    fn pthread_get_stacksize_np(thread: usize) -> usize;
}

/// Addresses a fault lands on when the main thread's stack is exhausted.
static LOW: AtomicUsize = AtomicUsize::new(0);
static HIGH: AtomicUsize = AtomicUsize::new(0);
static mut STACK: [u8; SIGNAL_STACK] = [0; SIGNAL_STACK];

const MESSAGE: &[u8] =
    b"\nthread 'main' has overflowed its stack\nfatal runtime error: stack overflow\n";

/// The address window `[low, high)` just past the end of the calling (main)
/// thread's stack, or `None` when the stack has no fixed end.
fn overflow_window() -> Option<(usize, usize)> {
    #[cfg(target_os = "linux")]
    {
        // The stack may grow down to its resource limit below its top. The
        // top itself is not known without reading /proc: it lies above this
        // frame by the arguments and environment, a few megabytes at most.
        let mut limit = [0u64; 2];
        // SAFETY: `limit` is the two-word structure getrlimit fills in
        if unsafe { getrlimit(os::RLIMIT_STACK, &mut limit) } != 0 || limit[0] == os::RLIM_INFINITY
        {
            return None;
        }
        let here = &limit as *const _ as usize;
        let end = here.checked_sub(limit[0] as usize)?;
        Some((end.saturating_sub(2 << 20), end + (4 << 20)))
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: both calls only read the calling thread's descriptor
        let (top, size) = unsafe {
            let me = pthread_self();
            (
                pthread_get_stackaddr_np(me) as usize,
                pthread_get_stacksize_np(me),
            )
        };
        let end = top.checked_sub(size)?;
        Some((end.saturating_sub(1 << 20), end + (64 << 10)))
    }
}

extern "C" fn on_fault(signum: c_int, info: *const u8, _context: *mut c_void) {
    // SAFETY: the kernel passes a valid siginfo for SA_SIGINFO handlers, and
    // only async-signal-safe functions are called below
    unsafe {
        let address = info.add(os::SI_ADDR).cast::<usize>().read_unaligned();
        if address >= LOW.load(Ordering::Relaxed) && address < HIGH.load(Ordering::Relaxed) {
            write(2, MESSAGE.as_ptr().cast(), MESSAGE.len());
            abort();
        }
        // any other fault: back to the default action, which the faulting
        // instruction triggers again on return
        signal(signum, 0);
    }
}

/// Makes a stack overflow of the calling thread — the main thread, called
/// once at start-up — end the process with the standard library's message
/// and an abort instead of a bare SIGSEGV.
pub fn report_overflow() {
    let Some((low, high)) = overflow_window() else {
        return;
    };
    LOW.store(low, Ordering::Relaxed);
    HIGH.store(high, Ordering::Relaxed);
    let stack = os::signal_stack((&raw mut STACK).cast(), SIGNAL_STACK);
    let action = os::action(on_fault as *const () as usize);
    // SAFETY: the signal stack is a static used for nothing else, and the
    // handler stays valid for the life of the process
    unsafe {
        if sigaltstack(&stack, core::ptr::null_mut()) != 0 {
            return;
        }
        sigaction(SIGSEGV, &action, core::ptr::null_mut());
        sigaction(os::SIGBUS, &action, core::ptr::null_mut());
    }
}

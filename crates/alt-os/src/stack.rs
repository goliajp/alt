//! Telling a stack overflow apart from any other fault.
//!
//! A program entered through the C `main` gets no report from the standard
//! library when a thread runs out of stack: it just dies of SIGSEGV.
//! [`report_overflow`] installs a handler that prints the usual message and
//! aborts when the faulting address lies at the faulting thread's stack
//! pointer, and steps aside for every other fault. A handler cannot run on
//! the stack that ran out, so each thread needs a signal stack of its own:
//! the main thread gets it from `report_overflow` (three system calls, no
//! files read), every other thread from [`cover_thread`].

use core::ffi::{c_int, c_void};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "linux")]
mod os {
    use core::ffi::{c_int, c_ulong, c_void};

    pub const SIGBUS: c_int = 7;
    pub const SA_SIGINFO: c_int = 4;
    pub const SA_ONSTACK: c_int = 0x0800_0000;
    pub const SS_DISABLE: c_int = 2;
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

    /// The stack pointer of the interrupted code, from the `ucontext_t` a
    /// handler is given.
    ///
    /// # Safety
    /// `context` is the third argument of an `SA_SIGINFO` handler.
    pub unsafe fn stack_pointer(context: *const u8) -> Option<usize> {
        // uc_mcontext.gregs[REG_RSP], and uc_mcontext.sp
        #[cfg(target_arch = "x86_64")]
        const SP: usize = 160;
        #[cfg(target_arch = "aarch64")]
        const SP: usize = 432;
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        return Some(unsafe { context.add(SP).cast::<usize>().read_unaligned() });
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = context;
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use core::ffi::{c_int, c_void};

    pub const SIGBUS: c_int = 10;
    pub const SA_SIGINFO: c_int = 0x40;
    pub const SA_ONSTACK: c_int = 0x1;
    pub const SS_DISABLE: c_int = 4;
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

    /// The stack pointer of the interrupted code, from the `ucontext_t` a
    /// handler is given.
    ///
    /// # Safety
    /// `context` is the third argument of an `SA_SIGINFO` handler.
    pub unsafe fn stack_pointer(context: *const u8) -> Option<usize> {
        // __ss.__rsp and __ss.__sp, behind the exception state
        #[cfg(target_arch = "x86_64")]
        const SP: usize = 72;
        #[cfg(target_arch = "aarch64")]
        const SP: usize = 264;
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        // SAFETY: uc_mcontext, at offset 48, points at the machine context
        return Some(unsafe {
            let machine = context.add(48).cast::<*const u8>().read_unaligned();
            machine.add(SP).cast::<usize>().read_unaligned()
        });
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = context;
            None
        }
    }
}

const SIGSEGV: c_int = 11;
const SIGNAL_STACK: usize = 64 << 10;
/// How far from the stack pointer a fault still counts as the stack
/// running out. Compiled code touches each page of a frame as it claims
/// it, so the fault of an overflow lands within a page of the pointer;
/// the rest allows for frames claimed in one step by code that does not.
const NEAR: usize = 256 << 10;

unsafe extern "C" {
    fn sigaction(signum: c_int, act: *const os::sigaction, old: *mut os::sigaction) -> c_int;
    fn sigaltstack(ss: *const os::stack_t, old: *mut os::stack_t) -> c_int;
    fn signal(signum: c_int, handler: usize) -> usize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn abort() -> !;
}

static INSTALLED: AtomicBool = AtomicBool::new(false);
static mut STACK: [u8; SIGNAL_STACK] = [0; SIGNAL_STACK];

thread_local! {
    /// the name the handler reports this thread under
    static NAME: Cell<&'static [u8]> = const { Cell::new(b"<unnamed>") };
    static COVER: Cover = Cover::install();
}

/// A thread's signal stack and the name it is reported under, for as long
/// as the thread runs.
struct Cover {
    stack: *mut [u8],
    name: *mut [u8],
}

impl Cover {
    fn install() -> Self {
        let stack = Box::into_raw(vec![0u8; SIGNAL_STACK].into_boxed_slice());
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>").as_bytes();
        let name = Box::into_raw(name.to_vec().into_boxed_slice());
        // SAFETY: both blocks stay allocated until `drop`, which takes them
        // out of use first
        unsafe {
            sigaltstack(
                &os::signal_stack(stack.cast(), SIGNAL_STACK),
                core::ptr::null_mut(),
            );
            NAME.set(&*name);
        }
        Self { stack, name }
    }
}

impl Drop for Cover {
    fn drop(&mut self) {
        let mut off = os::signal_stack(core::ptr::null_mut(), SIGNAL_STACK);
        off.ss_flags = os::SS_DISABLE;
        // SAFETY: the thread stops using both blocks before they are freed
        unsafe {
            sigaltstack(&off, core::ptr::null_mut());
            let _ = NAME.try_with(|name| name.set(b"<unnamed>"));
            drop(Box::from_raw(self.stack));
            drop(Box::from_raw(self.name));
        }
    }
}

extern "C" fn on_fault(signum: c_int, info: *const u8, context: *mut c_void) {
    // SAFETY: the kernel passes a valid siginfo and context to SA_SIGINFO
    // handlers, and only async-signal-safe functions are called below
    unsafe {
        let address = info.add(os::SI_ADDR).cast::<usize>().read_unaligned();
        let at_stack = os::stack_pointer(context.cast())
            .is_some_and(|sp| address.wrapping_sub(sp.wrapping_sub(NEAR)) < 2 * NEAR);
        if at_stack {
            let name = NAME.get();
            for part in [
                &b"\nthread '"[..],
                name,
                b"' has overflowed its stack\nfatal runtime error: stack overflow\n",
            ] {
                write(2, part.as_ptr().cast(), part.len());
            }
            abort();
        }
        // any other fault: back to the default action, which the faulting
        // instruction triggers again on return
        signal(signum, 0);
    }
}

/// Makes a stack overflow end the process with the standard library's
/// message and an abort instead of a bare SIGSEGV. Called once at start-up
/// on the main thread, which it also covers.
pub fn report_overflow() {
    let stack = os::signal_stack((&raw mut STACK).cast(), SIGNAL_STACK);
    let action = os::action(on_fault as *const () as usize);
    // SAFETY: the signal stack is a static used for nothing else, and the
    // handler stays valid for the life of the process
    unsafe {
        if sigaltstack(&stack, core::ptr::null_mut()) != 0 {
            return;
        }
        NAME.set(b"main");
        sigaction(SIGSEGV, &action, core::ptr::null_mut());
        sigaction(os::SIGBUS, &action, core::ptr::null_mut());
    }
    INSTALLED.store(true, Ordering::Relaxed);
}

/// Gives the calling thread the signal stack the report needs, until the
/// thread ends. Called first thing by every thread a program that called
/// [`report_overflow`] starts; without that call it does nothing, as the
/// standard library's own handler then covers its threads.
pub fn cover_thread() {
    if INSTALLED.load(Ordering::Relaxed) {
        COVER.with(|_| ());
    }
}

/// `run`, for a new thread to start with: covered by [`cover_thread`]
/// before anything else happens on it.
pub fn covered<T>(run: impl FnOnce() -> T) -> impl FnOnce() -> T {
    move || {
        cover_thread();
        run()
    }
}

//! The operating system interface alt uses, written against the kernel and
//! the system C library directly.
//!
//! - [`durability`]: whether a thread's writes are fsynced
//! - [`env`]: the variables of the process in one pass
//! - [`locale`]: the locales the C library has installed
//! - [`random`]: bytes from the operating system's generator
//! - [`heap`]: keeping freed memory with the process
//! - [`map`]: read-only file mappings
//! - [`stack`]: reporting a stack overflow of the main thread
//! - [`stdio`]: `/dev/null` on standard descriptors that start out closed
//! - [`sys`]: the constants, structures and functions of the C interface that
//!   the workspace calls
//! - [`temp`]: temporary files and directories
//! - [`users`]: the users the system knows
//!
//! Constant values and structure layouts are written out for four targets:
//! Linux with glibc and macOS, each on x86_64 and aarch64.

#[cfg(not(all(
    any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64"
)))]
compile_error!("alt-os supports Linux (glibc) and macOS on x86_64 and aarch64 only");

pub mod durability;
pub mod env;
pub mod heap;
pub mod locale;
pub mod map;
pub mod random;
pub mod stack;
pub mod stdio;
pub mod sys;
pub mod temp;
pub mod users;

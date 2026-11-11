//! The operating system interface alt uses, written against the kernel and
//! the system C library directly.
//!
//! - [`random`]: bytes from the operating system's generator
//! - [`map`]: read-only file mappings
//! - [`sys`]: the constants, structures and functions of the C interface that
//!   the workspace calls
//! - [`temp`]: temporary files and directories
//!
//! Constant values and structure layouts are written out for four targets:
//! Linux with glibc and macOS, each on x86_64 and aarch64.

#[cfg(not(all(
    any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64"
)))]
compile_error!("alt-os supports Linux (glibc) and macOS on x86_64 and aarch64 only");

pub mod map;
pub mod random;
pub mod sys;
pub mod temp;

//! How the C library's allocator gives freed memory back to the system.
//!
//! glibc returns the top of the heap to the kernel as soon as more than
//! 128 KiB of it is free, and serves every allocation above 128 KiB from a
//! mapping of its own that it unmaps on free. Both limits move up by
//! themselves, but only when a large mapped block is freed: a process that
//! happens to do that early keeps its memory from then on, one that does
//! not pays for fresh pages again and again. A daemon serving the same
//! request repeatedly allocates and frees the same few megabytes each
//! time; with the limits where they start, every request faults all of
//! them in again.

/// Fixes the two limits high, so that freed memory stays with the process
/// and is reused: blocks up to 32 MiB come from the heap, and the heap is
/// trimmed only once 256 MiB of it are free. Call once, before threads
/// are started. Does nothing where the allocator is not glibc's.
pub fn keep_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        const M_TRIM_THRESHOLD: core::ffi::c_int = -1;
        const M_MMAP_THRESHOLD: core::ffi::c_int = -3;
        unsafe extern "C" {
            fn mallopt(param: core::ffi::c_int, value: core::ffi::c_int) -> core::ffi::c_int;
        }
        // SAFETY: mallopt takes two integers and only sets allocator
        // parameters; a value it refuses leaves the default in place
        unsafe {
            mallopt(M_MMAP_THRESHOLD, 32 << 20);
            mallopt(M_TRIM_THRESHOLD, 256 << 20);
        }
    }
}

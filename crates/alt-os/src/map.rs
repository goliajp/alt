//! Read-only file mappings.

use core::ffi::{c_int, c_void};
use std::{fs::File, io, ops::Deref, os::fd::AsRawFd, ptr::NonNull};

const PROT_READ: c_int = 1;
const MAP_SHARED: c_int = 1;
const MAP_FAILED: *mut c_void = !0 as *mut c_void;

unsafe extern "C" {
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
}

/// A file, or the start of one, mapped read-only and shared.
///
/// Dereferences to the bytes mapped. The mapping is removed when the value is
/// dropped; the file itself may be closed earlier.
pub struct Mmap {
    start: NonNull<u8>,
    length: usize,
}

impl Mmap {
    /// Maps the current contents of `file`. An empty file gives an empty
    /// slice.
    ///
    /// # Safety
    ///
    /// The bytes are shared with the file: if the file is truncated or
    /// written while the mapping is alive, reads through the mapping fault or
    /// observe the change. The caller must make sure no process does that.
    pub unsafe fn map(file: &File) -> io::Result<Self> {
        let length = usize::try_from(file.metadata()?.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "memory map length overflows usize",
            )
        })?;
        // SAFETY: the file is that long, and the rest is the caller's
        unsafe { Self::map_len(file, length) }
    }

    /// Maps the first `length` bytes of `file`.
    ///
    /// # Safety
    ///
    /// As for [`Mmap::map`], and the file must be at least `length` bytes
    /// long for as long as the mapping is alive. What a process writes
    /// past the bytes that are read through the mapping does no harm when
    /// those are reached through [`Mmap::first`] alone.
    pub unsafe fn map_len(file: &File, length: usize) -> io::Result<Self> {
        if length == 0 {
            // the kernel rejects a zero-length mapping
            return Ok(Self {
                start: NonNull::dangling(),
                length: 0,
            });
        }
        // SAFETY: a fresh mapping at an address the kernel chooses touches no
        // existing memory; the descriptor is open for the duration of the call
        let address = unsafe {
            mmap(
                std::ptr::null_mut(),
                length,
                PROT_READ,
                MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            // a successful mapping is never at address zero
            start: NonNull::new(address.cast()).expect("mapping address"),
            length,
        })
    }
}

impl Mmap {
    /// The first `length` of the bytes mapped, with no reference made to
    /// those after them.
    pub fn first(&self, length: usize) -> &[u8] {
        assert!(length <= self.length, "more than is mapped");
        // SAFETY: `start` is valid for reads of that many bytes until drop
        // (or dangling with length zero), and the mapping is never writable
        unsafe { std::slice::from_raw_parts(self.start.as_ptr(), length) }
    }
}

impl Deref for Mmap {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `start` is valid for reads of `length` bytes until drop
        // (or dangling with length zero), and the mapping is never writable
        unsafe { std::slice::from_raw_parts(self.start.as_ptr(), self.length) }
    }
}

impl AsRef<[u8]> for Mmap {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        if self.length != 0 {
            // SAFETY: exactly the range mmap returned, unmapped once
            unsafe { munmap(self.start.as_ptr().cast(), self.length) };
        }
    }
}

impl std::fmt::Debug for Mmap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Mmap")
            .field("start", &self.start)
            .field("length", &self.length)
            .finish()
    }
}

// SAFETY: the mapping is read-only memory owned by this value; it has no
// thread affinity and shared reads need no synchronization
unsafe impl Send for Mmap {}
// SAFETY: as above
unsafe impl Sync for Mmap {}

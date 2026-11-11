//! Random bytes from the operating system's generator.

use std::io;

/// Fills `buffer` with random bytes.
///
/// # Panics
///
/// Panics if the operating system refuses to provide random bytes.
pub fn fill(buffer: &mut [u8]) {
    if let Err(error) = try_fill(buffer) {
        panic!("operating system random generator failed: {error}");
    }
}

/// Fills `buffer` with random bytes, reporting a failure of the operating
/// system's generator instead of panicking.
pub fn try_fill(mut buffer: &mut [u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        match fill_some(buffer) {
            Ok(filled) => buffer = &mut buffer[filled..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// One `getrandom` system call; returns how many leading bytes were filled.
#[cfg(target_os = "linux")]
fn fill_some(buffer: &mut [u8]) -> io::Result<usize> {
    #[cfg(target_arch = "x86_64")]
    const GETRANDOM: usize = 318;
    #[cfg(target_arch = "aarch64")]
    const GETRANDOM: usize = 278;

    let result: isize;
    // SAFETY: getrandom(buf, buflen, flags) writes at most `buffer.len()`
    // bytes into `buffer`, which is valid for writes of that length; the
    // registers named are the only ones the kernel changes
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") GETRANDOM => result,
            in("rdi") buffer.as_mut_ptr(),
            in("rsi") buffer.len(),
            in("rdx") 0usize,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    // SAFETY: as above
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "svc 0",
            in("x8") GETRANDOM,
            inlateout("x0") buffer.as_mut_ptr() => result,
            in("x1") buffer.len(),
            in("x2") 0usize,
            options(nostack),
        );
    }
    // the kernel returns a negated errno in place of a count
    if result < 0 {
        return Err(io::Error::from_raw_os_error(-result as i32));
    }
    Ok(result as usize)
}

/// One `getentropy` call; returns how many leading bytes were filled.
#[cfg(target_os = "macos")]
fn fill_some(buffer: &mut [u8]) -> io::Result<usize> {
    unsafe extern "C" {
        fn getentropy(buffer: *mut core::ffi::c_void, length: usize) -> core::ffi::c_int;
    }
    // getentropy rejects requests longer than 256 bytes
    let length = buffer.len().min(256);
    // SAFETY: `buffer` is valid for writes of `length` bytes
    if unsafe { getentropy(buffer.as_mut_ptr().cast(), length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(length)
}

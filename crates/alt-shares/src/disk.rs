/// Half of what is free on the disk `directory` is on.
pub fn half_of_what_is_free(directory: &std::path::Path) -> std::io::Result<u64> {
    use std::os::fd::AsRawFd;
    let directory = std::fs::File::open(directory)?;
    let mut value = std::mem::MaybeUninit::<alt_os::sys::statvfs>::uninit();
    // SAFETY: fstatvfs writes the structure it is pointed to where it succeeds,
    // and the descriptor is open through the call
    if unsafe { alt_os::sys::fstatvfs(directory.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the call succeeded and filled every field
    let value = unsafe { value.assume_init() };
    Ok(((value.f_frsize as u128 * value.f_bavail as u128) / 2).min(u64::MAX as u128) as u64)
}

//! what the operating system reports about the processor's caches

/// Size of the first-level data cache in bytes; 32 KiB when the system does not say.
/// On a processor with several kinds of cores this is the one of the fastest kind.
#[cfg(target_arch = "aarch64")]
pub(crate) fn first_level_data_bytes() -> usize {
    static BYTES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BYTES.get_or_init(|| query().unwrap_or(32 << 10))
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn query() -> Option<usize> {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            old: *mut std::ffi::c_void,
            old_len: *mut usize,
            new: *const std::ffi::c_void,
            new_len: usize,
        ) -> std::ffi::c_int;
    }
    // performance cores first; machines with one kind of core only have the plain name
    [c"hw.perflevel0.l1dcachesize", c"hw.l1dcachesize"]
        .into_iter()
        .find_map(|name| {
            let mut value = 0u64;
            let mut len = size_of::<u64>();
            // the name is a NUL-terminated string and the buffer is as long as `len` says
            let status = unsafe {
                sysctlbyname(
                    name.as_ptr(),
                    (&raw mut value).cast(),
                    &raw mut len,
                    std::ptr::null(),
                    0,
                )
            };
            (status == 0 && value != 0).then_some(value as usize)
        })
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
fn query() -> Option<usize> {
    let cache = std::path::Path::new("/sys/devices/system/cpu/cpu0/cache");
    for index in 0..8 {
        let entry = cache.join(format!("index{index}"));
        let read = |name: &str| std::fs::read_to_string(entry.join(name)).ok();
        if read("level")?.trim() == "1" && read("type")?.trim() == "Data" {
            let size = read("size")?;
            let size = size.trim();
            let (digits, unit) = match size.strip_suffix('K') {
                Some(digits) => (digits, 1 << 10),
                None => (size.strip_suffix('M')?, 1 << 20),
            };
            return Some(digits.parse::<usize>().ok()? * unit);
        }
    }
    None
}

#[cfg(all(
    target_arch = "aarch64",
    not(any(target_os = "macos", target_os = "linux"))
))]
fn query() -> Option<usize> {
    None
}

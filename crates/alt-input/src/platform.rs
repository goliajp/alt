use crate::{Error, Result};
use std::path::Path;

/// Execution support is narrower than the portable manifest format. This is
/// an explicit entry gate; a filesystem that merely resembles ext4 is not
/// silently accepted as the supported runner configuration.
pub fn check_execution_platform(root: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root)?;
    let bytes = root.as_os_str().as_encoded_bytes();
    let display = std::str::from_utf8(bytes).map_err(|_| Error::Path {
        reason: "non-UTF-8 workspace root",
        bytes_hex: crate::hex(bytes),
    })?;
    if display
        .split('/')
        .any(|part| part.trim() != part || part.contains(['\0', '\n', '\r']))
    {
        return Err(Error::Path {
            reason: "unsupported workspace root",
            bytes_hex: crate::hex(bytes),
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open(&root)?;
        let mut stat = std::mem::MaybeUninit::<alt_os::sys::statfs>::uninit();
        if unsafe { alt_os::sys::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { stat.assume_init() }.f_type != 0xef53 {
            return Err(Error::Unsupported(
                "task execution requires an ext4 filesystem".into(),
            ));
        }
        fn decode(value: &str) -> Result<String> {
            let mut bytes = Vec::new();
            let mut i = 0;
            while i < value.len() {
                if value.as_bytes()[i] == b'\\' {
                    let digits = value
                        .get(i + 1..i + 4)
                        .ok_or_else(|| Error::Unsupported("invalid mountinfo escape".into()))?;
                    bytes.push(
                        u8::from_str_radix(digits, 8)
                            .map_err(|_| Error::Unsupported("invalid mountinfo escape".into()))?,
                    );
                    i += 4;
                } else {
                    bytes.push(value.as_bytes()[i]);
                    i += 1;
                }
            }
            String::from_utf8(bytes).map_err(|_| Error::Unsupported("non-UTF-8 mount point".into()))
        }
        let info = std::fs::read_to_string("/proc/self/mountinfo")?;
        let mut best: Option<(usize, String)> = None;
        for line in info.lines() {
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let Some(point) = left.split_whitespace().nth(4) else {
                continue;
            };
            let point = decode(point)?;
            if root.starts_with(Path::new(&point))
                && best
                    .as_ref()
                    .is_none_or(|(length, _)| point.len() >= *length)
            {
                best = Some((
                    point.len(),
                    right.split_whitespace().next().unwrap_or("").into(),
                ));
            }
        }
        if best.as_ref().is_none_or(|(_, kind)| kind != "ext4") {
            return Err(Error::Unsupported(format!(
                "task execution requires local ext4 on Linux x86_64; detected {:?}",
                best.map(|(_, kind)| kind)
            )));
        }
        return Ok(());
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open(&root)?;
        let mut stat = std::mem::MaybeUninit::<alt_os::sys::statfs>::uninit();
        if unsafe { alt_os::sys::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let stat = unsafe { stat.assume_init() };
        let kind = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) }.to_bytes();
        if kind != b"apfs" || stat.f_flags & alt_os::sys::MNT_LOCAL as u32 == 0 {
            return Err(Error::Unsupported(
                "task execution requires local APFS on macOS arm64".into(),
            ));
        }
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err(Error::Unsupported(
        "task execution supports macOS arm64/APFS and Linux x86_64/ext4".into(),
    ))
}

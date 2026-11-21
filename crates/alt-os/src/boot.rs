//! Which boot of the machine the process runs in.
//!
//! What a process wrote and did not sync is in the kernel's cache until
//! the machine goes down, and may be gone or cut short after. Whoever
//! keeps a note that a file was whole compares the boot it was noted in
//! with the running one.

use std::sync::OnceLock;

/// The identity the kernel gave the running boot: sixteen bytes that no
/// other boot has. `None` where the kernel tells none.
pub fn id() -> Option<[u8; 16]> {
    static ID: OnceLock<Option<[u8; 16]>> = OnceLock::new();
    *ID.get_or_init(|| parse(&told()?))
}

/// A UUID as the kernel writes it: 32 hexadecimal digits in groups set
/// apart by `-`, then what ends the text.
fn parse(text: &[u8]) -> Option<[u8; 16]> {
    let mut digits = text
        .iter()
        .take_while(|b| b.is_ascii_hexdigit() || **b == b'-')
        .filter(|b| **b != b'-')
        .map(|b| (*b as char).to_digit(16).unwrap() as u8);
    let mut id = [0; 16];
    for byte in &mut id {
        *byte = digits.next()? << 4 | digits.next()?;
    }
    digits.next().is_none().then_some(id)
}

#[cfg(target_os = "linux")]
fn told() -> Option<Vec<u8>> {
    std::fs::read("/proc/sys/kernel/random/boot_id").ok()
}

#[cfg(target_os = "macos")]
fn told() -> Option<Vec<u8>> {
    use std::ffi::{c_char, c_int, c_void};
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }
    let mut text = [0u8; 64];
    let mut len = text.len();
    // SAFETY: the name is NUL-terminated, `text` is valid for writes of
    // `len` bytes, and the kernel sets `len` to what it wrote; nothing
    // new is given
    let rc = unsafe {
        sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            text.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then(|| text[..len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uuid_reads_as_its_sixteen_bytes_and_nothing_else_does() {
        let id = parse(b"01234567-89ab-cdef-0123-456789ABCDEF\n").unwrap();
        assert_eq!(id[..4], [0x01, 0x23, 0x45, 0x67]);
        assert_eq!(id[12..], [0x89, 0xab, 0xcd, 0xef]);
        assert_eq!(parse(b"0123456789abcdef0123456789abcdef\0"), Some(id));
        for other in [&b""[..], b"0123", b"01234567-89ab-cdef-0123-456789abcdef0"] {
            assert_eq!(parse(other), None);
        }
    }

    #[test]
    fn the_running_boot_has_one_identity() {
        let id = id().expect("this kernel tells its boot");
        assert_eq!(super::id(), Some(id));
        assert_ne!(id, [0; 16]);
    }
}

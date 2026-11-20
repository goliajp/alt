//! The variables of the process, read in one pass.
//!
//! The standard library answers for one name at a time, and each answer
//! walks the whole list; a program that wants twenty of them walks it
//! once here.

use core::ffi::{CStr, c_char};

#[cfg(target_os = "linux")]
unsafe extern "C" {
    static environ: *const *const c_char;
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn _NSGetEnviron() -> *const *const *const c_char;
}

fn list() -> *const *const c_char {
    // SAFETY: the C library sets the variable up before `main` and the
    // function answers with its address
    #[cfg(target_os = "linux")]
    return unsafe { environ };
    #[cfg(target_os = "macos")]
    return unsafe { *_NSGetEnviron() };
}

/// Calls `each` with the name and the value of every variable, in the
/// order the process holds them. An entry without `=` is passed over.
///
/// The list is read without the lock the standard library reads it
/// under. Changing a variable while another thread may read one is
/// already what `std::env::set_var` requires its callers to rule out.
pub fn each(mut each: impl FnMut(&[u8], &[u8])) {
    let mut entry = list();
    if entry.is_null() {
        return;
    }
    // SAFETY: the list ends with a null pointer and every entry before it
    // is a NUL-terminated string that stays where it is while no variable
    // is changed
    unsafe {
        while !(*entry).is_null() {
            let bytes = CStr::from_ptr(*entry).to_bytes();
            if let Some(at) = bytes.iter().position(|byte| *byte == b'=') {
                each(&bytes[..at], &bytes[at + 1..]);
            }
            entry = entry.add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn every_variable_is_seen_once_with_the_value_the_standard_library_has() {
        let mut seen = Vec::new();
        super::each(|name, value| seen.push((name.to_vec(), value.to_vec())));
        let mut known: Vec<_> = std::env::vars_os()
            .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect();
        assert!(!known.is_empty());
        seen.sort();
        known.sort();
        assert_eq!(seen, known);
    }
}

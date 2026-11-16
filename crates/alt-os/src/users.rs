//! The users the system knows.

use std::ffi::{CStr, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use crate::sys;

const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const ESRCH: i32 = 3;
const EBADF: i32 = 9;
const ERANGE: i32 = 34;

/// The home directory of the user `name`; `None` when the system knows no
/// such user.
pub fn home_of(name: &CStr) -> io::Result<Option<PathBuf>> {
    let mut strings = vec![0u8; 1024];
    loop {
        let mut entry = MaybeUninit::<sys::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated, the buffers are writable for
        // the lengths given, and all of them outlive the call
        let code = unsafe {
            sys::getpwnam_r(
                name.as_ptr(),
                entry.as_mut_ptr(),
                strings.as_mut_ptr().cast(),
                strings.len(),
                &mut found,
            )
        };
        if code == ERANGE {
            strings.resize(strings.len() * 2, 0);
            continue;
        }
        if found.is_null() {
            // the codes systems answer with for a name they do not have
            return match code {
                0 | EPERM | ENOENT | ESRCH | EBADF => Ok(None),
                code => Err(io::Error::from_raw_os_error(code)),
            };
        }
        // SAFETY: the call filled the entry `found` points to, whose
        // strings lie in `strings` and are NUL-terminated
        let home = unsafe { CStr::from_ptr((*found).pw_dir) };
        return Ok(Some(PathBuf::from(OsStr::from_bytes(home.to_bytes()))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_home_of_a_user_is_what_the_system_says_and_none_for_no_user() {
        let root = home_of(c"root").unwrap().expect("there is a root");
        assert!(root.is_absolute(), "{root:?}");
        assert_eq!(home_of(c"no-such-user-of-this-system").unwrap(), None);
    }
}

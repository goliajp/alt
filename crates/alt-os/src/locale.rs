//! The locales the system C library has installed.

use std::ffi::CStr;

use crate::sys::{CODESET, LC_CTYPE_MASK, c_char, c_int, c_void};

unsafe extern "C" {
    fn newlocale(mask: c_int, locale: *const c_char, base: *mut c_void) -> *mut c_void;
    fn freelocale(locale: *mut c_void);
    fn nl_langinfo_l(item: c_int, locale: *mut c_void) -> *const c_char;
}

/// The character set of the locale `name` as the C library has it, or
/// `None` when it has no such locale: where `setlocale` would fail and
/// leave a program in the locale `C`. The locale of the process is
/// neither read nor changed.
pub fn charset(name: &CStr) -> Option<Vec<u8>> {
    // SAFETY: the name ends in NUL, and no base locale is given
    let locale = unsafe { newlocale(LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut()) };
    if locale.is_null() {
        return None;
    }
    // SAFETY: the locale was just made; the string it returns ends in
    // NUL and lives until the locale is freed, after the copy
    let charset = unsafe { CStr::from_ptr(nl_langinfo_l(CODESET, locale)) }
        .to_bytes()
        .to_vec();
    // SAFETY: made by newlocale and freed once
    unsafe { freelocale(locale) };
    Some(charset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locale_has_a_character_set_only_when_it_is_installed() {
        assert!(charset(c"C").is_some_and(|set| set != b"UTF-8"));
        assert!(charset(c"POSIX").is_some_and(|set| set != b"UTF-8"));
        assert_eq!(charset(c"xx_XX.UTF-8"), None);
        assert_eq!(charset(c"/nowhere"), None);
        // the one UTF-8 locale every system of its kind has
        let utf8 = match cfg!(target_os = "linux") {
            true => c"C.UTF-8",
            false => c"en_US.UTF-8",
        };
        assert_eq!(charset(utf8).as_deref(), Some(&b"UTF-8"[..]));
    }
}

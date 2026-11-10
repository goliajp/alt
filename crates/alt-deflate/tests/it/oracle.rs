//! libz as the reference, called the way the callers of this crate did.

use std::ffi::c_void;

use alt_deflate::Format;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

unsafe extern "C" fn zalloc(
    _opaque: libz_sys::voidpf,
    items: libz_sys::uInt,
    size: libz_sys::uInt,
) -> libz_sys::voidpf {
    unsafe { malloc(items as usize * size as usize) }
}

unsafe extern "C" fn zfree(_opaque: libz_sys::voidpf, address: libz_sys::voidpf) {
    unsafe { free(address) }
}

pub fn version() -> String {
    unsafe { std::ffi::CStr::from_ptr(libz_sys::zlibVersion()) }
        .to_string_lossy()
        .into_owned()
}

fn zlib(data: &[u8], level: i32) -> Vec<u8> {
    unsafe {
        let mut dest_len = libz_sys::compressBound(data.len() as libz_sys::uLong);
        let mut dest = vec![0u8; dest_len as usize];
        let rc = libz_sys::compress2(
            dest.as_mut_ptr(),
            &mut dest_len,
            data.as_ptr(),
            data.len() as libz_sys::uLong,
            level,
        );
        assert_eq!(rc, libz_sys::Z_OK);
        dest.truncate(dest_len as usize);
        dest
    }
}

fn raw(data: &[u8], level: i32) -> Vec<u8> {
    unsafe {
        let mut strm = libz_sys::z_stream {
            next_in: std::ptr::null_mut(),
            avail_in: 0,
            total_in: 0,
            next_out: std::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: std::ptr::null_mut(),
            state: std::ptr::null_mut(),
            zalloc,
            zfree,
            opaque: std::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        };
        let rc = libz_sys::deflateInit2_(
            &mut strm,
            level,
            libz_sys::Z_DEFLATED,
            -15,
            8,
            libz_sys::Z_DEFAULT_STRATEGY,
            libz_sys::zlibVersion(),
            size_of::<libz_sys::z_stream>() as i32,
        );
        assert_eq!(rc, libz_sys::Z_OK);
        let bound = libz_sys::deflateBound(&mut strm, data.len() as libz_sys::uLong) as usize;
        let mut out = vec![0u8; bound.max(1)];
        strm.next_in = data.as_ptr() as *mut _;
        strm.avail_in = data.len() as libz_sys::uInt;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as libz_sys::uInt;
        let rc = libz_sys::deflate(&mut strm, libz_sys::Z_FINISH);
        assert_eq!(rc, libz_sys::Z_STREAM_END);
        out.truncate(strm.total_out as usize);
        libz_sys::deflateEnd(&mut strm);
        out
    }
}

/// Compares every level in both framings against libz and returns the
/// number of comparisons made.
pub fn check(what: &str, data: &[u8]) -> usize {
    let mut compared = 0;
    for level in 0..=9u8 {
        for format in [Format::Raw, Format::Zlib] {
            let expected = match format {
                Format::Raw => raw(data, i32::from(level)),
                Format::Zlib => zlib(data, i32::from(level)),
                Format::Gzip => unreachable!("gzip is not compared with libz"),
            };
            let actual = alt_deflate::compress(data, level, format);
            compared += 1;
            if actual != expected {
                let first = actual
                    .iter()
                    .zip(&expected)
                    .position(|(a, e)| a != e)
                    .unwrap_or(actual.len().min(expected.len()));
                panic!(
                    "level {level} {format:?}: {what} ({} bytes): output differs at offset \
                     {first}; ours {} bytes, libz {} bytes",
                    data.len(),
                    actual.len(),
                    expected.len()
                );
            }
        }
    }
    compared
}

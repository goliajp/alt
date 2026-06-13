use crate::PackError;

/// Upper bound on how much we pre-allocate from a delta's declared target
/// size. The size comes from an untrusted header (an imported pack can be
/// adversarial); reserving it verbatim is a decompression-bomb vector.
const MAX_PREALLOC: usize = 64 << 20;

/// Applies a git delta (the inflated payload of an ofs/ref-delta entry)
/// to `base`: `<src size varint><tgt size varint><copy|insert ops…>`.
pub fn apply(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, PackError> {
    let mut pos = 0;
    let src_size = varint(delta, &mut pos)?;
    if src_size != base.len() as u64 {
        return Err(PackError::Format("delta source size mismatch"));
    }
    let tgt_size = varint(delta, &mut pos)?;

    // never reserve the untrusted tgt_size unbounded (fuzz found a 32 GiB
    // reservation OOM); grow the vec naturally and let the final length
    // check validate the real result against tgt_size
    let mut out = Vec::with_capacity((tgt_size as usize).min(MAX_PREALLOC));
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            // copy from base: optional little-endian offset/size bytes,
            // presence controlled by the low 7 op bits
            let mut off: u64 = 0;
            for (i, bit) in [0x01u8, 0x02, 0x04, 0x08].into_iter().enumerate() {
                if op & bit != 0 {
                    off |= u64::from(next(delta, &mut pos)?) << (8 * i);
                }
            }
            let mut len: u64 = 0;
            for (i, bit) in [0x10u8, 0x20, 0x40].into_iter().enumerate() {
                if op & bit != 0 {
                    len |= u64::from(next(delta, &mut pos)?) << (8 * i);
                }
            }
            if len == 0 {
                len = 0x10000;
            }
            let chunk = base
                .get(off as usize..(off + len) as usize)
                .ok_or(PackError::Format("delta copy out of base bounds"))?;
            out.extend_from_slice(chunk);
        } else if op != 0 {
            // insert the next `op` literal bytes
            let n = op as usize;
            let chunk = delta
                .get(pos..pos + n)
                .ok_or(PackError::Format("truncated delta insert"))?;
            out.extend_from_slice(chunk);
            pos += n;
        } else {
            return Err(PackError::Format("reserved delta opcode 0"));
        }
    }
    if out.len() as u64 != tgt_size {
        return Err(PackError::Format("delta target size mismatch"));
    }
    Ok(out)
}

fn next(d: &[u8], pos: &mut usize) -> Result<u8, PackError> {
    let b = *d
        .get(*pos)
        .ok_or(PackError::Format("truncated delta copy operands"))?;
    *pos += 1;
    Ok(b)
}

fn varint(d: &[u8], pos: &mut usize) -> Result<u64, PackError> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *d
            .get(*pos)
            .ok_or(PackError::Format("truncated delta size varint"))?;
        *pos += 1;
        // checked_shl rejects a varint with more continuation bytes than a
        // u64 can hold (shift >= 64) — fuzz found this shift-overflow panic
        v |= u64::from(b & 0x7f)
            .checked_shl(shift)
            .ok_or(PackError::Format("delta size varint too long"))?;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_only_delta() {
        // src 0, tgt 5, insert "hello"
        let delta = b"\x00\x05\x05hello";
        assert_eq!(apply(b"", delta).unwrap(), b"hello");
    }

    #[test]
    fn copy_and_insert() {
        let base = b"hello world";
        // src 11, tgt 9: copy off=6 len=5 ("world"), insert " ho",
        // copy off=0 len=1 ("h") => "world hoh"... keep simple:
        // copy(6,5) + insert(" ") + copy(0,3) => "world hel"
        let delta = b"\x0b\x09\x91\x06\x05\x01 \x90\x03";
        assert_eq!(apply(base, delta).unwrap(), b"world hel");
    }

    #[test]
    fn rejects_corrupt_deltas() {
        assert!(apply(b"x", b"\x00\x05\x05hello").is_err()); // src size lies
        assert!(apply(b"", b"\x00\x05\x05he").is_err()); // truncated insert
        assert!(apply(b"", b"\x00\x01\x00").is_err()); // reserved opcode
        assert!(apply(b"ab", b"\x02\x05\x91\x00\x05").is_err()); // copy oob
        assert!(apply(b"", b"\x00\x02\x01x").is_err()); // tgt size mismatch
    }

    #[test]
    fn huge_target_size_does_not_preallocate() {
        // empty base, a declared target of ~34 GiB, and no ops: fuzz found
        // this OOM'd by reserving the declared size. It must Err on the
        // length check instead, capping the reservation at MAX_PREALLOC.
        let delta = b"\x00\x80\x80\x80\x80\x80\x01";
        assert!(apply(b"", delta).is_err());
    }

    #[test]
    fn rejects_overlong_size_varint() {
        // 10 continuation bytes push the shift past 63: must Err, not panic
        let delta = b"\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x00";
        assert!(apply(b"", delta).is_err());
    }
}

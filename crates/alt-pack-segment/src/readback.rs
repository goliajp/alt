//! Reading back the entries a build has just written.
//!
//! A built segment becomes the only copy of its objects: whoever handed them over
//! lets go of its own once the segment is published. A compressor that writes a
//! stream no decoder accepts, or one that decodes to other bytes, would otherwise be
//! found out when the object is next asked for. So every entry is read from where it
//! was written, checked against the checksum readers hold it to, inflated, and its
//! content compared by length and digest with what was handed over.

use crate::{Error, Record, send::entry_header};
use std::io::Read;

const CHUNK: usize = 64 << 10;

/// what `Error::Corrupt` says when a built segment does not return an object as it
/// was handed over
pub const UNREADABLE: &str = "written object does not read back";

/// Checks the entries of `records` as `source` returns them, one after the other in
/// the order they were written. An object is in memory a chunk at a time.
pub(crate) fn entries(source: &mut impl Read, records: &[Record]) -> Result<(), Error> {
    let mut inflater = alt_inflate::Inflater::new(alt_inflate::Format::Zlib);
    let mut input = Vec::with_capacity(CHUNK);
    let mut output = vec![0u8; CHUNK];
    let mut content = alt_hash::blake3::Hasher::new();
    // the longest entry header: a kind and a 64-bit size
    let mut header = [0u8; 10];
    for record in records {
        let bad = || Error::Corrupt(UNREADABLE);
        let expected = entry_header(record.kind as u8, record.size);
        let mut left = (record.len as usize)
            .checked_sub(expected.len())
            .ok_or_else(bad)?;
        let header = &mut header[..expected.len()];
        source.read_exact(header)?;
        let mut crc = alt_hash::Crc32::new();
        crc.update(header);
        content.reset();
        let mut size = 0u64;
        inflater.reset();
        input.clear();
        let mut at = 0;
        loop {
            let step = inflater
                .inflate(&input[at..], &mut output)
                .map_err(|_| bad())?;
            at += step.consumed;
            content.update(&output[..step.produced]);
            size += step.produced as u64;
            if step.done {
                break;
            }
            if step.consumed == 0 && step.produced == 0 {
                if left == 0 {
                    return Err(bad());
                }
                // what the decoder left unconsumed has to come again, with more after it
                input.drain(..at);
                at = 0;
                let held = input.len();
                let more = left.min(CHUNK);
                input.resize(held + more, 0);
                source.read_exact(&mut input[held..])?;
                crc.update(&input[held..]);
                left -= more;
            }
        }
        if header != expected.as_slice()
            || left != 0
            || at != input.len()
            || size != record.size
            || crc.finalize() != record.crc
            || content.finalize().as_bytes() != &record.blake3
        {
            return Err(bad());
        }
    }
    Ok(())
}

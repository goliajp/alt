//! The weights of a Huffman tree description, stored either as nibbles or
//! FSE-coded.

use crate::Error;
use crate::bits::BackReader;
use crate::fse::{Entry, build_table, read_distribution};

/// Reads the weights of all symbols but the last. Returns how many there are
/// and the bytes consumed.
#[inline(always)]
pub(crate) fn read_weights(src: &[u8], weights: &mut [u8; 256]) -> Result<(usize, usize), Error> {
    let header = *src.first().ok_or(Error::Corrupt)? as usize;
    if header >= 128 {
        // four bits per weight
        let count = header - 127;
        let bytes = count.div_ceil(2);
        let body = src.get(1..1 + bytes).ok_or(Error::Corrupt)?;
        for (i, weight) in weights[..count].iter_mut().enumerate() {
            let pair = body[i / 2];
            *weight = if i & 1 == 0 { pair >> 4 } else { pair & 15 };
        }
        Ok((count, 1 + bytes))
    } else {
        let body = src.get(1..1 + header).ok_or(Error::Corrupt)?;
        Ok((fse_weights(body, weights)?, 1 + header))
    }
}

/// Decodes weights coded with two interleaved FSE states. The stream holds
/// no count: it ends when a state update runs out of bits.
#[inline(always)]
fn fse_weights(src: &[u8], weights: &mut [u8; 256]) -> Result<usize, Error> {
    const MAX_LOG: u32 = 6;
    let mut norm = [0i16; 256];
    let (log, symbols, used) = read_distribution(src, &mut norm, MAX_LOG)?;
    let mut table = [Entry::default(); 1 << MAX_LOG];
    build_table::<256>(&norm[..symbols], log, &mut table, 0, |symbol| {
        (symbol as u64) << 32
    });

    let mut reader = BackReader::new(&src[used..])?;
    let mut state = reader.read(log) as usize;
    let mut other = reader.read(log) as usize;
    reader.refill();
    if reader.overflowed() {
        return Err(Error::Corrupt);
    }
    let mut count = 0;
    // a whole window holds four state updates, of six bits at most each,
    // so between two refills that take whole bytes the stream cannot run
    // out, and the two states advance without waiting for each other
    while reader.roomy() && count <= 250 {
        for pair in weights[count..count + 4].as_chunks_mut::<2>().0 {
            let entry = table[state];
            pair[0] = entry.base() as u8;
            state = entry.next() + reader.read(entry.bits()) as usize;
            let entry = table[other];
            pair[1] = entry.base() as u8;
            other = entry.next() + reader.read(entry.bits()) as usize;
        }
        count += 4;
        reader.refill();
    }
    loop {
        // the last weight is never stored, so 255 is the most there can be
        if count > 253 {
            return Err(Error::Corrupt);
        }
        let entry = table[state];
        weights[count] = entry.base() as u8;
        count += 1;
        state = entry.next() + reader.read(entry.bits()) as usize;
        reader.refill();
        if reader.overflowed() {
            weights[count] = table[other].base() as u8;
            return Ok(count + 1);
        }
        (state, other) = (other, state);
    }
}

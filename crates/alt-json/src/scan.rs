const ONES: u64 = u64::MAX / 255;
const HIGHS: u64 = ONES << 7;

/// Index of the first byte at or after `from` that is a control character
/// (below 0x20), a double quote or a backslash; `bytes.len()` when there is
/// none. These are the bytes that end a plain run inside a JSON string, both
/// when reading and when writing.
pub(crate) fn find_special(bytes: &[u8], from: usize) -> usize {
    let mut index = from;
    let (words, rest) = bytes[from..].as_chunks::<8>();
    for word in words {
        let word = u64::from_le_bytes(*word);
        // a borrow can only flag bytes above a real match, so the lowest
        // flagged byte is exact
        let control = word.wrapping_sub(ONES * 0x20) & !word;
        let quote = word ^ (ONES * u64::from(b'"'));
        let quote = quote.wrapping_sub(ONES) & !quote;
        let backslash = word ^ (ONES * u64::from(b'\\'));
        let backslash = backslash.wrapping_sub(ONES) & !backslash;
        let hits = (control | quote | backslash) & HIGHS;
        if hits != 0 {
            return index + hits.trailing_zeros() as usize / 8;
        }
        index += 8;
    }
    for &byte in rest {
        if byte < 0x20 || byte == b'"' || byte == b'\\' {
            return index;
        }
        index += 1;
    }
    index
}

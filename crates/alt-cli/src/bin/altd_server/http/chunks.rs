//! The framing of a body sent in chunks: the lines around its chunks.

use std::io;

use super::paced::Source;

/// Where the reading of a chunked body is.
pub(super) enum Chunks {
    /// before the size of a chunk; `true` behind the bytes of one
    Size(bool),
    /// this many bytes of a chunk are still to come
    Within(u64),
    Over,
}

/// The next line, without its end.
pub(super) fn line(source: &mut Source) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        match source.byte()? {
            b'\n' => break,
            byte if line.len() < 4096 => line.push(byte),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "a chunk line")),
        }
    }
    if line.pop() != Some(b'\r') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a chunk line"));
    }
    Ok(line)
}

/// The size a chunk line gives: hex digits and nothing else, before any
/// extensions.
pub(super) fn chunk_size(line: &[u8]) -> Option<u64> {
    let digits = line.iter().take_while(|b| b.is_ascii_hexdigit()).count();
    let behind = line[digits..].trim_ascii_start();
    let extended = behind.is_empty() == (digits == line.len());
    if digits == 0 || digits > 16 || !extended || !(behind.is_empty() || behind[0] == b';') {
        return None;
    }
    u64::from_str_radix(std::str::from_utf8(&line[..digits]).ok()?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::chunk_size;

    #[test]
    fn the_size_of_a_chunk_is_hex_digits_and_nothing_before_or_around_them() {
        for (line, size) in [
            ("0", 0),
            ("1a", 26),
            ("A;x=y", 10),
            ("3 ;x", 3),
            ("ffffffffffffffff", u64::MAX),
        ] {
            assert_eq!(chunk_size(line.as_bytes()), Some(size), "{line:?}");
        }
        for line in [
            "",
            "+3",
            " 3",
            "\t3",
            "3 ",
            "3\t",
            "0x3",
            "3 x",
            ";x",
            "-1",
            "10000000000000000",
        ] {
            assert_eq!(chunk_size(line.as_bytes()), None, "{line:?}");
        }
    }
}

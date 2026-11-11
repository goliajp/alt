//! Fixture packs: frames made by the reference implementation, stored as
//! `key length, key, frame length, frame` records.

/// The (key, bytes) records of a pack.
pub fn parse(mut pack: &'static [u8]) -> Vec<(&'static str, &'static [u8])> {
    let mut records = Vec::new();
    while !pack.is_empty() {
        let key_len = u16::from_le_bytes([pack[0], pack[1]]) as usize;
        let key = std::str::from_utf8(&pack[2..2 + key_len]).unwrap();
        pack = &pack[2 + key_len..];
        let len = u32::from_le_bytes(pack[..4].try_into().unwrap()) as usize;
        records.push((key, &pack[4..4 + len]));
        pack = &pack[4 + len..];
    }
    records
}

#[cfg(alt_zstd_regenerate)]
pub fn append(pack: &mut Vec<u8>, key: &str, bytes: &[u8]) {
    pack.extend_from_slice(&(key.len() as u16).to_le_bytes());
    pack.extend_from_slice(key.as_bytes());
    pack.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    pack.extend_from_slice(bytes);
}

pub static CORPUS: &[u8] = include_bytes!("frames/corpus.pack");
pub static SIZED: &[u8] = include_bytes!("frames/sized.pack");
pub static SMALL: &[u8] = include_bytes!("frames/small.pack");
pub static PREFIXED: &[u8] = include_bytes!("frames/prefixed.pack");
pub static CHECKED: &[u8] = include_bytes!("frames/checked.pack");
pub static DAMAGED: &[u8] = include_bytes!("frames/damaged.pack");

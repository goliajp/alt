use crate::{Error, Result};
use std::ops::Range;

pub(crate) const BLOCK: usize = 512;
pub(crate) type Block = [u8; BLOCK];

pub(crate) const NAME: Range<usize> = 0..100;
const MODE: Range<usize> = 100..108;
const UID: Range<usize> = 108..116;
const GID: Range<usize> = 116..124;
const SIZE: Range<usize> = 124..136;
const MTIME: Range<usize> = 136..148;
const CHECKSUM: Range<usize> = 148..156;
pub(crate) const KIND: usize = 156;
const MAGIC: Range<usize> = 257..265;
const PREFIX: Range<usize> = 345..500;

const GNU_MAGIC: &[u8; 8] = b"ustar  \0";
const USTAR_MAGIC: &[u8; 6] = b"ustar\0";
// octal fields hold 11 digits; larger values are stored in base 256
const OCTAL_LIMIT: u64 = 1 << 33;

pub(crate) fn padding(size: u64) -> usize {
    (BLOCK - (size % BLOCK as u64) as usize) % BLOCK
}

fn checksum(block: &Block) -> u32 {
    let sum = |bytes: &[u8]| bytes.iter().map(|&b| u32::from(b)).sum::<u32>();
    sum(&block[..CHECKSUM.start]) + 8 * u32::from(b' ') + sum(&block[CHECKSUM.end..])
}

fn octal(field: &[u8]) -> Result<u64> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let digits = field[..end].trim_ascii();
    if digits.is_empty() {
        return Err(Error::Number);
    }
    digits.iter().try_fold(0u64, |value, &digit| match digit {
        b'0'..=b'7' => Ok(value << 3 | u64::from(digit - b'0')),
        _ => Err(Error::Number),
    })
}

fn number(field: &[u8]) -> Result<u64> {
    if field[0] & 0x80 == 0 {
        return octal(field);
    }
    let (high, low) = field.split_at(field.len() - 8);
    if high[0] != 0x80 || high[1..].iter().any(|&b| b != 0) {
        return Err(Error::Number);
    }
    Ok(u64::from_be_bytes(low.try_into().expect("eight bytes")))
}

pub(crate) fn verify(block: &Block) -> Result<()> {
    if octal(&block[CHECKSUM])? != u64::from(checksum(block)) {
        return Err(Error::Checksum);
    }
    Ok(())
}

pub(crate) fn size(block: &Block) -> Result<u64> {
    number(&block[SIZE])
}

fn terminated(field: &[u8]) -> &[u8] {
    &field[..field.iter().position(|&b| b == 0).unwrap_or(field.len())]
}

pub(crate) fn path(block: &Block) -> Vec<u8> {
    let name = terminated(&block[NAME]);
    let prefix = terminated(&block[PREFIX]);
    if &block[MAGIC][..6] != USTAR_MAGIC || prefix.is_empty() {
        return name.to_vec();
    }
    [prefix, b"/", name].concat()
}

fn put_octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    for (index, slot) in field[..digits].iter_mut().enumerate() {
        *slot = b'0' + (value >> (3 * (digits - 1 - index)) & 7) as u8;
    }
    field[digits] = 0;
}

fn put_number(field: &mut [u8], value: u64) {
    if value < OCTAL_LIMIT {
        return put_octal(field, value);
    }
    let split = field.len() - 8;
    field[..split].fill(0);
    field[split..].copy_from_slice(&value.to_be_bytes());
    field[0] |= 0x80;
}

fn seal(block: &mut Block) {
    block[MAGIC].copy_from_slice(GNU_MAGIC);
    let sum = checksum(block);
    put_octal(&mut block[CHECKSUM], u64::from(sum));
}

/// `name` is at most 100 bytes
pub(crate) fn file(name: &[u8], mode: u32, mtime: u64, size: u64) -> Block {
    let mut block = [0; BLOCK];
    block[..name.len()].copy_from_slice(name);
    put_octal(&mut block[MODE], u64::from(mode));
    put_number(&mut block[SIZE], size);
    put_number(&mut block[MTIME], mtime);
    seal(&mut block);
    block
}

pub(crate) fn long_name(size: u64) -> Block {
    let mut block = [0; BLOCK];
    block[..13].copy_from_slice(b"././@LongLink");
    put_octal(&mut block[MODE], 0o644);
    put_octal(&mut block[UID], 0);
    put_octal(&mut block[GID], 0);
    put_number(&mut block[SIZE], size);
    put_octal(&mut block[MTIME], 0);
    block[KIND] = b'L';
    seal(&mut block);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_from_eight_gibibytes_use_base_256() {
        let block = file(b"file", 0o600, 0, OCTAL_LIMIT - 1);
        assert_eq!(&block[SIZE], b"77777777777\0");
        assert_eq!(size(&block).unwrap(), OCTAL_LIMIT - 1);
        for value in [OCTAL_LIMIT, u64::MAX] {
            let block = file(b"file", 0o600, 0, value);
            assert_eq!(block[SIZE][..4], [0x80, 0, 0, 0]);
            assert_eq!(block[SIZE][4..], value.to_be_bytes());
            verify(&block).unwrap();
            assert_eq!(size(&block).unwrap(), value);
        }
    }

    #[test]
    fn padding_completes_the_block() {
        assert_eq!(padding(0), 0);
        assert_eq!(padding(1), 511);
        assert_eq!(padding(512), 0);
        assert_eq!(padding(u64::MAX), 1);
    }
}

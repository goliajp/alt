//! CRC-32 as zlib, gzip, PNG and Git pack indexes use it (polynomial 0x04c11db7,
//! bits reflected, initial value and final inversion of all ones).

/// streaming CRC-32
#[derive(Clone, Default)]
pub struct Crc32 {
    /// the register without the final inversion
    register: u32,
}

impl Crc32 {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn update(&mut self, data: &[u8]) {
        self.register = !fold(!self.register, data);
    }

    pub fn finalize(&self) -> u32 {
        self.register
    }
}

/// CRC-32 of one buffer
#[inline]
pub fn crc32(data: &[u8]) -> u32 {
    !fold(!0, data)
}

/// the polynomial with its bits reflected
const POLYNOMIAL: u32 = 0xedb8_8320;

/// x^n modulo the polynomial, bits reflected
const fn power(n: u32) -> u32 {
    let mut value = 0x8000_0000u32;
    let mut step = 0;
    while step < n {
        value = (value >> 1) ^ if value & 1 != 0 { POLYNOMIAL } else { 0 };
        step += 1;
    }
    value
}

/// The pair of multipliers that moves a 128-bit remainder `distance` bits ahead in the
/// message: the low half is multiplied by x^(distance+32) and the high half by
/// x^(distance-32), both as carry-less multiplication operands.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const fn ahead(distance: u32) -> [u64; 2] {
    [
        (power(distance + 32) as u64) << 1,
        (power(distance - 32) as u64) << 1,
    ]
}

const TABLES: [[u32; 256]; 8] = {
    let mut tables = [[0u32; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut value = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ if value & 1 != 0 { POLYNOMIAL } else { 0 };
            bit += 1;
        }
        tables[0][byte] = value;
        byte += 1;
    }
    let mut table = 1;
    while table < 8 {
        let mut byte = 0;
        while byte < 256 {
            let previous = tables[table - 1][byte];
            tables[table][byte] = (previous >> 8) ^ tables[0][(previous & 0xff) as usize];
            byte += 1;
        }
        table += 1;
    }
    tables
};

/// The reference implementation every accelerated one is compared with: eight bytes
/// per step through eight tables. Takes and returns the register without inversion.
pub fn portable(mut register: u32, data: &[u8]) -> u32 {
    let (words, rest) = data.as_chunks::<8>();
    for word in words {
        let low = u32::from_le_bytes([word[0], word[1], word[2], word[3]]) ^ register;
        let high = u32::from_le_bytes([word[4], word[5], word[6], word[7]]);
        register = TABLES[7][(low & 0xff) as usize]
            ^ TABLES[6][(low >> 8 & 0xff) as usize]
            ^ TABLES[5][(low >> 16 & 0xff) as usize]
            ^ TABLES[4][(low >> 24) as usize]
            ^ TABLES[3][(high & 0xff) as usize]
            ^ TABLES[2][(high >> 8 & 0xff) as usize]
            ^ TABLES[1][(high >> 16 & 0xff) as usize]
            ^ TABLES[0][(high >> 24) as usize];
    }
    for byte in rest {
        register = (register >> 8) ^ TABLES[0][((register ^ *byte as u32) & 0xff) as usize];
    }
    register
}

/// which implementation runs on this CPU
pub fn implementation() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    if arm::supported() {
        return "carry-less multiplication and crc instructions";
    }
    #[cfg(target_arch = "x86_64")]
    if x86::supported() {
        return "carry-less multiplication";
    }
    "portable"
}

/// Advances the register (without inversion) over `data`. Direct calls rather than a
/// function pointer: a checksum of a few dozen bytes costs about as much as a call.
#[inline]
fn fold(register: u32, data: &[u8]) -> u32 {
    #[cfg(target_arch = "aarch64")]
    if arm::supported() {
        // the features were just detected on this CPU
        return unsafe { arm::fold_bytes(register, data) };
    }
    #[cfg(target_arch = "x86_64")]
    if x86::supported() {
        // the features were just detected on this CPU
        return unsafe { x86::fold(register, data) };
    }
    portable(register, data)
}

#[cfg(target_arch = "aarch64")]
mod arm;
#[cfg(target_arch = "x86_64")]
mod x86;

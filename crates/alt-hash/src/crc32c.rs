//! CRC-32C (Castagnoli: polynomial 0x1edc6f41, bits reflected, initial value and
//! final inversion of all ones), the checksum x86-64 and ARMv8 each have an
//! instruction for.

/// streaming CRC-32C
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Crc32c {
    /// the register without the final inversion
    register: u32,
}

impl Crc32c {
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

/// CRC-32C of one buffer
#[inline]
pub fn crc32c(data: &[u8]) -> u32 {
    !fold(!0, data)
}

/// the polynomial with its bits reflected
const POLYNOMIAL: u32 = 0x82f6_3b78;

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

/// The reference implementation the instructions are compared with: eight bytes per
/// step through eight tables. Takes and returns the register without inversion.
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
    if supported() {
        "crc instructions"
    } else {
        "portable"
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn supported() -> bool {
    std::arch::is_x86_feature_detected!("sse4.2")
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn supported() -> bool {
    std::arch::is_aarch64_feature_detected!("crc")
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn supported() -> bool {
    false
}

/// Advances the register (without inversion) over `data`. Direct calls rather than a
/// function pointer: a checksum of a few dozen bytes costs about as much as a call.
#[inline]
fn fold(register: u32, data: &[u8]) -> u32 {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    if supported() {
        // the feature was just detected on this CPU
        return unsafe { instructions(register, data) };
    }
    portable(register, data)
}

/// one instruction per eight bytes, then one per byte left
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.2")]
fn instructions(register: u32, data: &[u8]) -> u32 {
    use std::arch::x86_64::{_mm_crc32_u8, _mm_crc32_u64};
    let (words, rest) = data.as_chunks::<8>();
    let mut wide = register as u64;
    for word in words {
        wide = _mm_crc32_u64(wide, u64::from_le_bytes(*word));
    }
    let mut register = wide as u32;
    for byte in rest {
        register = _mm_crc32_u8(register, *byte);
    }
    register
}

/// one instruction per eight bytes, then one per byte left
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "crc")]
fn instructions(mut register: u32, data: &[u8]) -> u32 {
    use std::arch::aarch64::{__crc32cb, __crc32cd};
    let (words, rest) = data.as_chunks::<8>();
    for word in words {
        register = __crc32cd(register, u64::from_le_bytes(*word));
    }
    for byte in rest {
        register = __crc32cb(register, *byte);
    }
    register
}

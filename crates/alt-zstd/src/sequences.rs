//! The decoding tables of the sequences section of a compressed block.

use std::sync::OnceLock;

use crate::Error;
use crate::fse::{
    Entry, LL_DEFAULT, LL_DEFAULT_LOG, LL_FIXED, LL_MAX_LOG, LL_SYMBOLS, MAX_SYMBOLS, ML_DEFAULT,
    ML_DEFAULT_LOG, ML_FIXED, ML_MAX_LOG, ML_SYMBOLS, OF_DEFAULT, OF_DEFAULT_LOG, OF_FIXED,
    OF_MAX_LOG, OF_SYMBOLS, build_table, read_distribution,
};

#[derive(Clone, Copy)]
enum Field {
    LiteralLength,
    Offset,
    MatchLength,
}

impl Field {
    fn symbols(self) -> usize {
        match self {
            Self::LiteralLength => LL_SYMBOLS,
            Self::Offset => OF_SYMBOLS,
            Self::MatchLength => ML_SYMBOLS,
        }
    }

    fn max_log(self) -> u32 {
        match self {
            Self::LiteralLength => LL_MAX_LOG,
            Self::Offset => OF_MAX_LOG,
            Self::MatchLength => ML_MAX_LOG,
        }
    }

    /// Per code, the baseline and the extra-bit count as an entry holds them.
    fn fixed(self) -> &'static [u64; 64] {
        match self {
            Self::LiteralLength => &LL_FIXED,
            Self::Offset => &OF_FIXED,
            Self::MatchLength => &ML_FIXED,
        }
    }
}

/// Where each field's table starts inside the shared array.
const SLOT: [usize; 3] = [0, 1 << LL_MAX_LOG, 2 << LL_MAX_LOG];

const FIELDS: [Field; 3] = [Field::LiteralLength, Field::Offset, Field::MatchLength];

fn predefined() -> &'static SequenceDecoder {
    static TABLES: OnceLock<SequenceDecoder> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut tables = SequenceDecoder::new();
        tables.build(0, &LL_DEFAULT, LL_DEFAULT_LOG);
        tables.build(1, &OF_DEFAULT, OF_DEFAULT_LOG);
        tables.build(2, &ML_DEFAULT, ML_DEFAULT_LOG);
        tables
    })
}

/// The three tables in stream order (literal length, offset, match length)
/// as left by the last block that had sequences. They share one array, and
/// states are indices into it, so that the hot loop needs one base pointer.
pub(crate) struct SequenceDecoder {
    pub(crate) entries: [Entry; 3 << LL_MAX_LOG],
    pub(crate) logs: [u32; 3],
    valid: bool,
}

impl SequenceDecoder {
    pub(crate) fn new() -> Self {
        Self {
            entries: [Entry::default(); 3 << LL_MAX_LOG],
            logs: [0; 3],
            valid: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.valid = false;
    }

    #[inline(always)]
    fn build(&mut self, i: usize, norm: &[i16], log: u32) {
        let slot = &mut self.entries[SLOT[i]..];
        let fixed = FIELDS[i].fixed();
        build_table::<64>(norm, log, slot, SLOT[i] as u16, |code| fixed[code % 64]);
        self.logs[i] = log;
    }

    /// Reads the compression modes byte and the table descriptions that
    /// follow it. Returns the bytes consumed.
    pub(crate) fn read_tables(&mut self, src: &[u8]) -> Result<usize, Error> {
        #[cfg(target_arch = "x86_64")]
        if crate::has_bmi2() {
            // safety: the processor was just found to have the features
            return unsafe { self.read_tables_bmi2(src) };
        }
        self.read_tables_any(src)
    }

    /// The same code compiled for processors with BMI2: building tables is
    /// mostly shifts by variable counts and bit scans.
    ///
    /// # Safety
    /// The processor must have BMI1, BMI2 and LZCNT.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi1,bmi2,lzcnt")]
    unsafe fn read_tables_bmi2(&mut self, src: &[u8]) -> Result<usize, Error> {
        self.read_tables_any(src)
    }

    #[inline(always)]
    fn read_tables_any(&mut self, src: &[u8]) -> Result<usize, Error> {
        let modes = *src.first().ok_or(Error::Corrupt)?;
        if modes & 3 != 0 {
            return Err(Error::Corrupt);
        }
        let mut at = 1;
        for (i, field) in FIELDS.into_iter().enumerate() {
            let rest = &src[at..];
            match (modes >> (6 - 2 * i)) & 3 {
                0 => {
                    let log = predefined().logs[i];
                    let range = SLOT[i]..SLOT[i] + (1 << log);
                    self.entries[range.clone()].copy_from_slice(&predefined().entries[range]);
                    self.logs[i] = log;
                }
                1 => {
                    let code = *rest.first().ok_or(Error::Corrupt)? as usize;
                    if code >= field.symbols() {
                        return Err(Error::Corrupt);
                    }
                    self.entries[SLOT[i]] = Entry::with(field.fixed()[code], SLOT[i] as u16, 0);
                    self.logs[i] = 0;
                    at += 1;
                }
                2 => {
                    let mut norm = [0i16; MAX_SYMBOLS];
                    let norm = &mut norm[..field.symbols()];
                    let (log, symbols, used) = read_distribution(rest, norm, field.max_log())?;
                    self.build(i, &norm[..symbols], log);
                    at += used;
                }
                _ => {
                    if !self.valid {
                        return Err(Error::Corrupt);
                    }
                }
            }
        }
        self.valid = true;
        Ok(at)
    }
}

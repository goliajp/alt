//! The decoding tables of the sequences section of a compressed block.

use std::sync::OnceLock;

use crate::Error;
use crate::fse::{
    Entry, LL_BASE, LL_DEFAULT, LL_DEFAULT_LOG, LL_EXTRA, LL_MAX_LOG, LL_SYMBOLS, MAX_SYMBOLS,
    ML_BASE, ML_DEFAULT, ML_DEFAULT_LOG, ML_EXTRA, ML_MAX_LOG, ML_SYMBOLS, OF_DEFAULT,
    OF_DEFAULT_LOG, OF_MAX_LOG, OF_SYMBOLS, build_table, read_distribution,
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

    /// Baseline and extra-bit count of a code.
    fn value(self, code: usize) -> (u32, u8) {
        match self {
            Self::LiteralLength => (LL_BASE[code], LL_EXTRA[code]),
            Self::MatchLength => (ML_BASE[code], ML_EXTRA[code]),
            // codes 0 and 1 are repeat offsets and never use the baseline;
            // the others stand for (1 << code) + bits - 3
            Self::Offset => ((1u32 << code).wrapping_sub(3), code as u8),
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

    fn build(&mut self, i: usize, norm: &[i16], log: u32) {
        let slot = &mut self.entries[SLOT[i]..];
        build_table(norm, log, slot, SLOT[i] as u16, |code| {
            FIELDS[i].value(code)
        });
        self.logs[i] = log;
    }

    /// Reads the compression modes byte and the table descriptions that
    /// follow it. Returns the bytes consumed.
    pub(crate) fn read_tables(&mut self, src: &[u8]) -> Result<usize, Error> {
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
                    let (base, extra) = field.value(code);
                    self.entries[SLOT[i]] = Entry::new(base, SLOT[i] as u16, extra, 0);
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

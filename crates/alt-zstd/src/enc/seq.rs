//! The sequences section of a compressed block.

use std::sync::OnceLock;

use super::fse::{CTable, cost, normalize, table_log, write_distribution};
use super::matcher::Sequence;
use crate::bits::BitWriter;
use crate::fse::{
    LL_BASE, LL_DEFAULT, LL_DEFAULT_LOG, LL_EXTRA, LL_MAX_LOG, MAX_SYMBOLS, ML_BASE, ML_DEFAULT,
    ML_DEFAULT_LOG, ML_EXTRA, ML_MAX_LOG, OF_DEFAULT, OF_DEFAULT_LOG, OF_MAX_LOG,
};

/// Smallest match length the format can express.
const MIN_MATCH: u32 = 3;

/// Code of every literal length below 64 and every match length (less
/// `MIN_MATCH`) below 128; longer ones follow from the bit length.
const fn small_codes<const N: usize>(base: &[u32], bias: u32) -> [u8; N] {
    let mut codes = [0u8; N];
    let mut value = 0;
    let mut code = 0;
    while value < N {
        while code + 1 < base.len() && base[code + 1] <= value as u32 + bias {
            code += 1;
        }
        codes[value] = code as u8;
        value += 1;
    }
    codes
}
static LL_CODES: [u8; 64] = small_codes(&LL_BASE, 0);
static ML_CODES: [u8; 128] = small_codes(&ML_BASE, MIN_MATCH);

#[inline(always)]
fn ll_code(len: u32) -> usize {
    if len < 64 {
        LL_CODES[len as usize] as usize
    } else {
        len.ilog2() as usize + 19
    }
}

#[inline(always)]
fn ml_code(len: u32) -> usize {
    let biased = len - MIN_MATCH;
    if biased < 128 {
        ML_CODES[biased as usize] as usize
    } else {
        biased.ilog2() as usize + 36
    }
}

/// The table of one sequence field as the decoder currently holds it.
#[derive(Clone)]
struct FieldCoder {
    table: CTable,
    norm: [i16; MAX_SYMBOLS],
    symbols: usize,
}

impl FieldCoder {
    fn from_distribution(norm: &[i16], log: u32) -> Self {
        let mut coder = Self {
            table: CTable::new(),
            norm: [0; MAX_SYMBOLS],
            symbols: norm.len(),
        };
        coder.norm[..norm.len()].copy_from_slice(norm);
        coder.table.build(norm, log);
        coder
    }
}

struct Field {
    max_log: u32,
    default_norm: &'static [i16],
    default_log: u32,
}

const FIELDS: [Field; 3] = [
    Field {
        max_log: LL_MAX_LOG,
        default_norm: &LL_DEFAULT,
        default_log: LL_DEFAULT_LOG,
    },
    Field {
        max_log: OF_MAX_LOG,
        default_norm: &OF_DEFAULT,
        default_log: OF_DEFAULT_LOG,
    },
    Field {
        max_log: ML_MAX_LOG,
        default_norm: &ML_DEFAULT,
        default_log: ML_DEFAULT_LOG,
    },
];

fn predefined() -> &'static [FieldCoder; 3] {
    static CODERS: OnceLock<[FieldCoder; 3]> = OnceLock::new();
    CODERS.get_or_init(|| {
        FIELDS.map(|field| FieldCoder::from_distribution(field.default_norm, field.default_log))
    })
}

/// Tables in stream order (literal length, offset, match length) and the
/// scratch space of the section writer.
pub(crate) struct SequenceCoder {
    fields: [FieldCoder; 3],
    valid: bool,
    codes: [Vec<u8>; 3],
    header: Vec<u8>,
}

/// What a block may change in a [`SequenceCoder`].
pub(crate) struct SequenceState([FieldCoder; 3], bool);

impl SequenceCoder {
    pub(crate) fn new() -> Self {
        Self {
            fields: predefined().clone(),
            valid: false,
            codes: [Vec::new(), Vec::new(), Vec::new()],
            header: Vec::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.valid = false;
    }

    pub(crate) fn save(&self) -> SequenceState {
        SequenceState(self.fields.clone(), self.valid)
    }

    pub(crate) fn restore(&mut self, state: SequenceState) {
        (self.fields, self.valid) = (state.0, state.1);
    }

    /// Picks the cheapest way to code field `i` for the codes gathered,
    /// writes what the decoder needs to rebuild the table, and returns the
    /// mode bits.
    fn choose(&mut self, i: usize, counts: &[u32; MAX_SYMBOLS], out: &mut Vec<u8>) -> u8 {
        let field = &FIELDS[i];
        let symbols = counts.iter().rposition(|&count| count != 0).unwrap() + 1;
        let counts = &counts[..symbols];
        let total = self.codes[i].len() as u32;
        let most = *counts.iter().max().unwrap();

        // each candidate pays for its description and for the final state
        let mut best = (u32::MAX, 0u8);
        let mut consider = |bits: Option<u32>, mode: u8| {
            if let Some(bits) = bits
                && bits < best.0
            {
                best = (bits, mode);
            }
        };
        let kept = &self.fields[i];
        if self.valid {
            let bits = cost(counts, &kept.norm[..kept.symbols], kept.table.log);
            consider(bits.map(|bits| bits + kept.table.log), 3);
        }
        let bits = cost(counts, field.default_norm, field.default_log);
        consider(bits.map(|bits| bits + field.default_log), 0);
        let mut norm = [0i16; MAX_SYMBOLS];
        let mut log = 0;
        if most == total {
            consider(Some(8), 1);
        } else {
            log = table_log(field.max_log, total, symbols - 1);
            normalize(counts, total, log, &mut norm[..symbols]);
            self.header.clear();
            write_distribution(&norm[..symbols], log, &mut self.header);
            let bits = cost(counts, &norm[..symbols], log);
            consider(
                bits.map(|bits| bits + log + 8 * self.header.len() as u32),
                2,
            );
        }

        let coder = &mut self.fields[i];
        match best.1 {
            0 => coder.clone_from(&predefined()[i]),
            1 => {
                let symbol = symbols - 1;
                out.push(symbol as u8);
                coder.table.build_single(symbol);
                coder.norm = [0; MAX_SYMBOLS];
                coder.norm[symbol] = 1;
                coder.symbols = symbols;
            }
            2 => {
                out.extend_from_slice(&self.header);
                coder.table.build(&norm[..symbols], log);
                coder.norm = norm;
                coder.symbols = symbols;
            }
            _ => {}
        }
        best.1
    }

    /// Appends the sequences section for `sequences`.
    pub(crate) fn encode(&mut self, sequences: &[Sequence], out: &mut Vec<u8>) {
        let count = sequences.len();
        match count {
            0..128 => out.push(count as u8),
            128..0x7F00 => out.extend_from_slice(&[(count >> 8) as u8 + 128, count as u8]),
            _ => {
                out.push(255);
                out.extend_from_slice(&((count - 0x7F00) as u16).to_le_bytes());
            }
        }
        if count == 0 {
            return;
        }
        for codes in &mut self.codes {
            codes.clear();
            codes.resize(count, 0);
        }
        // two histograms per field, filled alternately: a run of one code
        // would otherwise make each increment wait for the one before
        let mut counts = [[[0u32; MAX_SYMBOLS]; 2]; 3];
        let [ll_codes, of_codes, ml_codes] = &mut self.codes;
        let codes = ll_codes.iter_mut().zip(of_codes).zip(ml_codes);
        for (index, (seq, ((ll, of), ml))) in sequences.iter().zip(codes).enumerate() {
            let half = index & 1;
            *ll = ll_code(seq.lit_len) as u8;
            *of = seq.offset.ilog2() as u8;
            *ml = ml_code(seq.match_len) as u8;
            counts[0][half][*ll as usize] += 1;
            counts[1][half][*of as usize] += 1;
            counts[2][half][*ml as usize] += 1;
        }
        let modes = out.len();
        out.push(0);
        for (i, [even, odd]) in counts.iter().enumerate() {
            let both: [u32; MAX_SYMBOLS] = std::array::from_fn(|code| even[code] + odd[code]);
            let mode = self.choose(i, &both, out);
            out[modes] |= mode << (6 - 2 * i);
        }
        self.valid = true;
        self.write_stream(sequences, out);
    }

    fn write_stream(&self, sequences: &[Sequence], out: &mut Vec<u8>) {
        let [ll, of, ml] = [0, 1, 2].map(|i| &self.fields[i].table);
        let [ll_codes, of_codes, ml_codes] = [0, 1, 2].map(|i| &self.codes[i][..]);
        out.reserve(sequences.len() * 4 + 16);
        let mut writer = BitWriter::new(out);

        // the decoder walks the stream from its end, so the last sequence
        // is written first and each field in the reverse of reading order
        let last = sequences.len() - 1;
        let mut ml_state = ml.start(ml_codes[last] as usize);
        let mut of_state = of.start(of_codes[last] as usize);
        let mut ll_state = ll.start(ll_codes[last] as usize);
        for i in (0..sequences.len()).rev() {
            let seq = sequences[i];
            let (ll_code, of_code, ml_code) = (
                ll_codes[i] as usize,
                of_codes[i] as usize,
                ml_codes[i] as usize,
            );
            let (ll_bits, ml_bits) = (LL_EXTRA[ll_code] as u32, ML_EXTRA[ml_code] as u32);
            // up to 26 bits of state changes, then the extra bits; the
            // writer takes 56 bits between flushes
            if i != last {
                of.encode(&mut writer, &mut of_state, of_code);
                ml.encode(&mut writer, &mut ml_state, ml_code);
                ll.encode(&mut writer, &mut ll_state, ll_code);
            }
            if ll_bits + ml_bits + of_code as u32 > 30 {
                writer.flush();
                writer.put((seq.lit_len - LL_BASE[ll_code]) as u64, ll_bits);
                writer.put((seq.match_len - ML_BASE[ml_code]) as u64, ml_bits);
                writer.flush();
            } else {
                writer.put((seq.lit_len - LL_BASE[ll_code]) as u64, ll_bits);
                writer.put((seq.match_len - ML_BASE[ml_code]) as u64, ml_bits);
            }
            writer.put((seq.offset - (1 << of_code)) as u64, of_code as u32);
            writer.flush();
        }
        for (table, state) in [(ml, ml_state), (of, of_state), (ll, ll_state)] {
            let (bits, width) = table.last(state);
            writer.put(bits, width);
        }
        writer.finish_marked();
    }
}

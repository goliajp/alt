//! Generated text with the statistics of source code: a skewed vocabulary, indented
//! lines, and lines and blocks that come back at short and long distances.

const WORDS: &str = "\
    ; { } ( ) , = let self . :: fn & -> : mut pub if return use for in ] [ ! ? => as \
    else impl match usize u8 len data &self Vec None Some Ok < > + * - == != && || += \
    -= .. /// // # const struct u32 u64 String &str &mut Result Err true false while \
    loop break continue enum mod type where trait static move ref dyn crate super i32 \
    i64 u16 bool char Option Self Box value index offset count size start end next \
    state input output buffer bytes path name key item entry node list map result \
    error level format stream block table code bits length total first last from into \
    with new push pop get set read write open close find insert remove clear copy \
    clone iter collect unwrap expect assert assert_eq format! vec! println! panic! \
    #[test] #[derive(Debug)] std::io std::fs std::path Path PathBuf File HashMap \
    BTreeMap Default Debug Clone Copy PartialEq Eq Hash Ord Iterator IntoIterator \
    AsRef From Into Read Write Seek Send Sync Sized Drop Fn FnMut FnOnce \
    config context request response handler session token header body status message \
    version object commit tree blob branch remote local merge delta chunk segment \
    window match_length distance literal symbol frequency checksum cursor position \
    capacity reserve extend truncate resize swap sort dedup contains starts_with \
    ends_with split join trim parse to_string to_vec to_owned as_ref as_mut as_slice \
    is_empty is_some is_none is_ok is_err map_err ok_or and_then unwrap_or filter \
    enumerate zip rev skip take chain fold any all min max sum position cycle \
    copied cloned flat_map chunks windows wrapping_add wrapping_sub wrapping_mul \
    saturating_sub checked_add leading_zeros trailing_zeros to_le_bytes from_le_bytes \
    to_be_bytes from_be_bytes u128 f32 f64 isize i8 i16 0 1 2 3 4 8 16 32 64 255 256 \
    1024 4096 32768 65535 0x0f 0xff 0x80 0xffff \"\" \"{}\" \"{:?}\" '\\n' b'0' '_ 'a \
    'static unsafe extern async await try union macro_rules! todo! unreachable! \
    debug_assert! matches! write! concat! env! include_bytes! cfg test feature \
    inline must_use allow deprecated repr transparent packed align thread spawn \
    channel sender receiver mutex lock guard atomic ordering arc rc cell borrow \
    lifetime generic bound param field method module package workspace target build \
    release profile bench example binary library source dest left right upper lower \
    head tail prev current parent child root leaf depth height width limit budget \
    threshold ratio rate delay timeout retry attempt queue stack heap slot bucket \
    page frame record batch group range span line column word text number digit \
    letter space tab newline quote escape prefix suffix pattern needle haystack \
    encode decode compress inflate deflate hash digest sign verify seal pack unpack \
    load store flush sync fetch send receive connect bind listen accept shutdown";

/// xorshift64*
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        ((self.next() >> 16) % bound as u64) as usize
    }

    /// below `bound`, small values far more likely than large ones
    fn skewed(&mut self, bound: usize) -> usize {
        let a = self.below(bound);
        let b = self.below(bound);
        let c = self.below(bound);
        a * b / bound * c / bound
    }
}

fn line(rng: &mut Rng, words: &[&str], depth: usize, out: &mut Vec<u8>) {
    out.extend(std::iter::repeat_n(b' ', depth * 4));
    let tokens = 1 + rng.skewed(14) + rng.below(4);
    for token in 0..tokens {
        if token > 0 {
            out.push(b' ');
        }
        match rng.below(24) {
            0 | 1 => {
                // a compound name
                out.extend_from_slice(words[60 + rng.below(words.len() - 60)].as_bytes());
                out.push(b'_');
                out.extend_from_slice(words[60 + rng.below(words.len() - 60)].as_bytes());
            }
            2 => out.extend_from_slice(rng.below(100_000).to_string().as_bytes()),
            3 => out.extend_from_slice(format!("0x{:08x}", rng.next() as u32).as_bytes()),
            _ => out.extend_from_slice(words[rng.skewed(words.len())].as_bytes()),
        }
    }
    out.push(b'\n');
}

/// At least `min_len` bytes of text, the same for the same `seed`.
pub fn generate(seed: u64, min_len: usize) -> Vec<u8> {
    let words: Vec<&str> = WORDS.split_ascii_whitespace().collect();
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity(min_len + 4096);
    // where each line written so far starts and ends
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0usize;
    while out.len() < min_len {
        let start = out.len();
        match rng.below(100) {
            0..=6 if !lines.is_empty() => {
                // one earlier line again, usually a recent one
                let back = 1 + rng.skewed(lines.len().min(4000));
                let (from, to) = lines[lines.len() - back];
                out.extend_from_within(from..to);
                lines.push((start, out.len()));
            }
            7 | 8 if lines.len() > 16 => {
                // a run of earlier lines again, from anywhere before
                let count = 2 + rng.below(10);
                let first = rng.below(lines.len() - count);
                for at in first..first + count {
                    let start = out.len();
                    let (from, to) = lines[at];
                    out.extend_from_within(from..to);
                    lines.push((start, out.len()));
                }
            }
            9..=14 => out.push(b'\n'),
            _ => {
                line(&mut rng, &words, depth, &mut out);
                lines.push((start, out.len()));
                match rng.below(6) {
                    0 if depth < 6 => depth += 1,
                    1 if depth > 0 => depth -= 1,
                    _ => {}
                }
            }
        }
    }
    out
}

//! The multi-byte indexes (§5) the CJK decoders read, one module per
//! index file, each its own statics.
//!
//! A stored entry is its code point, 0 where the index has no entry:
//! every index but Big5 stays in the BMP, and Big5's entries past it
//! are all in plane 2, each stored as a surrogate that indexes a side
//! table of their low 16 bits.

pub(crate) mod big5;
pub(crate) mod euc_kr;
pub(crate) mod gb18030;
pub(crate) mod jis0208;
pub(crate) mod jis0212;

/// The index code point (§5) of `pointer` in a BMP index stored from
/// pointer `first` on. `code_points` is one of the generated index
/// tables, which hold no surrogate.
#[inline(always)]
pub(crate) fn bmp(code_points: &[u16], first: usize, pointer: usize) -> Option<char> {
    match *code_points.get(pointer.checked_sub(first)?)? {
        0 => None,
        // SAFETY: the generator admits no surrogate into an index
        unit => Some(unsafe { char::from_u32_unchecked(u32::from(unit)) }),
    }
}

/// The index code point of `pointer` in index Big5.
#[inline(always)]
pub(crate) fn big5(pointer: usize) -> Option<char> {
    let p = pointer.checked_sub(big5::FIRST)?;
    let unit = match *big5::CODE_POINTS.get(p)? {
        0 => return None,
        unit @ 0xD800..=0xDFFF => plane_2(unit),
        unit => u32::from(unit),
    };
    // SAFETY: a BMP entry is no surrogate (the generator admits none),
    // and a plane 2 one is U+2xxxx
    Some(unsafe { char::from_u32_unchecked(unit) })
}

/// The Big5 plane 2 code point the stored surrogate `unit` stands for.
#[cold]
fn plane_2(unit: u16) -> u32 {
    0x2_0000 | u32::from(big5::PLANE_2[usize::from(unit - 0xD800)])
}

/// The index code point of `pointer` in index jis0208.
#[inline(always)]
pub(crate) fn jis0208(pointer: usize) -> Option<char> {
    bmp(&jis0208::CODE_POINTS, jis0208::FIRST, pointer)
}

/// The index gb18030 ranges code point (§5) of `pointer`.
pub(crate) fn gb18030_ranges(pointer: u32) -> Option<char> {
    if (pointer > 39419 && pointer < 189000) || pointer > 1237575 {
        return None;
    }
    if pointer == 7457 {
        return Some('\u{E7C7}');
    }
    // the first range starts at pointer 0
    let i = gb18030::RANGES.partition_point(|&(p, _)| p <= pointer) - 1;
    let (offset, code_point) = gb18030::RANGES[i];
    char::from_u32(code_point + pointer - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_read_the_standards_entries() {
        // the first and last entries of each index file
        assert_eq!(big5(941), None);
        assert_eq!(big5(942), Some('\u{43F0}'));
        assert_eq!(big5(19781), Some('\u{79D4}'));
        assert_eq!(big5(19782), None);
        // the pointers Big5's decoder maps to two code points
        assert_eq!(big5(1133), None);
        assert_eq!(jis0208(0), Some('\u{3000}'));
        assert_eq!(jis0208(11103), Some('\u{9ED1}'));
        assert_eq!(jis0208(11104), None);
        let jis0212 = |p| bmp(&jis0212::CODE_POINTS, jis0212::FIRST, p);
        assert_eq!(jis0212(107), None);
        assert_eq!(jis0212(108), Some('\u{2D8}'));
        let gb = |p| bmp(&gb18030::CODE_POINTS, gb18030::FIRST, p);
        assert_eq!(gb(0), Some('\u{4E02}'));
        assert_eq!(gb(23939), Some('\u{E4C5}'));
        let kr = |p| bmp(&euc_kr::CODE_POINTS, euc_kr::FIRST, p);
        assert_eq!(kr(0), Some('\u{AC02}'));
    }

    #[test]
    fn no_entry_is_a_surrogate() {
        let indexes: [&[u16]; 4] = [
            &euc_kr::CODE_POINTS,
            &gb18030::CODE_POINTS,
            &jis0208::CODE_POINTS,
            &jis0212::CODE_POINTS,
        ];
        for index in indexes {
            assert!(index.iter().all(|&u| !(0xD800..=0xDFFF).contains(&u)));
        }
    }

    #[test]
    fn big5_surrogates_index_plane_2_in_order() {
        let marks: alloc::vec::Vec<u16> = (big5::CODE_POINTS.iter().copied())
            .filter(|u| (0xD800..=0xDFFF).contains(u))
            .collect();
        assert_eq!(marks.len(), big5::PLANE_2.len());
        assert!(
            marks
                .iter()
                .enumerate()
                .all(|(k, &u)| usize::from(u - 0xD800) == k)
        );
    }

    #[test]
    fn big5_reaches_plane_2() {
        let plane_2 = (942..=19781).filter_map(big5).filter(|&c| c > '\u{FFFF}');
        assert_eq!(plane_2.count(), 1713);
    }

    #[test]
    fn ranges_reach_every_pointer_they_cover() {
        assert_eq!(gb18030_ranges(0), Some('\u{80}'));
        assert_eq!(gb18030_ranges(36), Some('\u{A5}'));
        assert_eq!(gb18030_ranges(7457), Some('\u{E7C7}'));
        assert_eq!(gb18030_ranges(39419), Some('\u{FFFF}'));
        assert_eq!(gb18030_ranges(39420), None);
        assert_eq!(gb18030_ranges(188999), None);
        assert_eq!(gb18030_ranges(189000), Some('\u{10000}'));
        assert_eq!(gb18030_ranges(1237575), Some('\u{10FFFF}'));
        assert_eq!(gb18030_ranges(1237576), None);
        // no pointer below 39420 lands on a surrogate
        assert!((0..=39419).all(|p| gb18030_ranges(p).is_some()));
    }
}

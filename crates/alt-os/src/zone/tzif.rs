//! A compiled file of the zone database (TZif, versions 1 to 3 and
//! later ones, which keep their layout): a header of counts and a table
//! with instants of 32 bits, then from version 2 on the same with
//! instants of 64 bits and a last line that spells the rule for the
//! years after the table. Leap seconds are not applied.

use super::{Zone, rule};

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(length)?;
        self.0 = rest;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
}

/// How many of each thing a table has.
struct Counts {
    ut: usize,
    standard: usize,
    leaps: usize,
    changes: usize,
    kinds: usize,
    letters: usize,
}

impl Counts {
    /// The length of what follows the kinds of local time in a table
    /// whose instants are `width` bytes.
    fn rest(&self, width: usize) -> usize {
        self.letters + self.leaps * (width + 4) + self.standard + self.ut
    }

    fn table(&self, width: usize) -> usize {
        self.changes * (width + 1) + self.kinds * 6 + self.rest(width)
    }

    /// Whether a table of these counts is no longer than the `left`
    /// bytes it is to be read from: no count is believed past that.
    fn fits(&self, width: usize, left: usize) -> bool {
        let Counts {
            ut,
            standard,
            leaps,
            changes,
            kinds,
            letters,
        } = *self;
        let parts = [
            (changes, width + 1),
            (kinds, 6),
            (letters, 1),
            (leaps, width + 4),
            (standard, 1),
            (ut, 1),
        ];
        let length = parts.iter().try_fold(0usize, |length, (count, each)| {
            length.checked_add(count.checked_mul(*each)?)
        });
        length.is_some_and(|length| length <= left)
    }
}

/// The version and the counts of the table that follows.
fn header(from: &mut Reader) -> Option<(u8, Counts)> {
    if from.take(4)? != b"TZif" {
        return None;
    }
    let version = from.take(16)?[0];
    let mut counts = [0; 6];
    for count in &mut counts {
        *count = from.u32()? as usize;
    }
    let [ut, standard, leaps, changes, kinds, letters] = counts;
    let counts = Counts {
        ut,
        standard,
        leaps,
        changes,
        kinds,
        letters,
    };
    Some((version, counts))
}

pub(super) fn parse(bytes: &[u8]) -> Option<Zone> {
    let mut from = Reader(bytes);
    let (version, mut counts) = header(&mut from)?;
    let wide = version >= b'2';
    if wide {
        counts.fits(4, from.0.len()).then_some(())?;
        from.take(counts.table(4))?;
        counts = header(&mut from)?.1;
    }
    let width = if wide { 8 } else { 4 };
    counts.fits(width, from.0.len()).then_some(())?;
    let mut instants = Vec::with_capacity(counts.changes);
    for _ in 0..counts.changes {
        let instant = from.take(width)?;
        instants.push(match wide {
            true => i64::from_be_bytes(instant.try_into().ok()?),
            false => i64::from(i32::from_be_bytes(instant.try_into().ok()?)),
        });
    }
    let kind_of = from.take(counts.changes)?;
    let mut offsets = Vec::with_capacity(counts.kinds);
    for _ in 0..counts.kinds {
        // the offset, whether it is summer time, and where its name is
        let kind = from.take(6)?;
        offsets.push(i32::from_be_bytes(kind[..4].try_into().ok()?));
    }
    from.take(counts.rest(width))?;
    let rule = match wide {
        true => {
            from.take(1).filter(|start| *start == b"\n")?;
            let line = from.0.strip_suffix(b"\n")?;
            match line.is_empty() {
                true => None,
                false => Some(rule::parse(std::str::from_utf8(line).ok()?)?),
            }
        }
        false => None,
    };
    let mut changes = Vec::with_capacity(instants.len());
    for (instant, kind) in instants.into_iter().zip(kind_of) {
        changes.push((instant, *offsets.get(usize::from(*kind))?));
    }
    if !changes.is_sorted_by(|a, b| a.0 < b.0) {
        return None;
    }
    Some(Zone {
        changes,
        first: *offsets.first()?,
        rule,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of `changes`, each an instant and the index of its kind,
    /// and of `offsets`, one for each kind, with instants `width` bytes.
    fn table(version: u8, width: usize, changes: &[(i64, u8)], offsets: &[i32]) -> Vec<u8> {
        let mut bytes = b"TZif".to_vec();
        bytes.push(version);
        bytes.extend([0; 15]);
        // a letter for each kind and one leap second, to be passed over
        let counts = [0, 0, 1, changes.len(), offsets.len(), offsets.len()];
        for count in counts {
            bytes.extend((count as u32).to_be_bytes());
        }
        for (instant, _) in changes {
            match width {
                8 => bytes.extend(instant.to_be_bytes()),
                _ => bytes.extend((*instant as i32).to_be_bytes()),
            }
        }
        bytes.extend(changes.iter().map(|(_, kind)| *kind));
        for (name, offset) in offsets.iter().enumerate() {
            bytes.extend(offset.to_be_bytes());
            bytes.extend([0, name as u8]);
        }
        bytes.extend(vec![b'X'; offsets.len()]);
        bytes.extend(vec![0; width + 4]);
        bytes
    }

    const CHANGES: &[(i64, u8)] = &[(-100, 1), (1000, 0), (2000, 1)];
    const OFFSETS: &[i32] = &[-18_000, -14_400];

    #[test]
    fn a_file_of_the_first_version_has_its_one_table_read() {
        let zone = parse(&table(0, 4, CHANGES, OFFSETS)).unwrap();
        assert_eq!(zone.offset_at(-101), -18_000);
        assert_eq!(zone.offset_at(-100), -14_400);
        assert_eq!(zone.offset_at(1000), -18_000);
        assert_eq!(zone.offset_at(1 << 40), -14_400);
    }

    #[test]
    fn a_later_file_has_its_second_table_and_its_last_line_read() {
        for version in *b"234" {
            let far = 1 << 40;
            let mut file = table(version, 4, &[(5, 0)], &[60]);
            file.extend(table(version, 8, &[(-far, 1), (far, 0)], OFFSETS));
            let mut ruled = file.clone();
            file.extend(b"\n\n");
            let zone = parse(&file).unwrap();
            assert_eq!(zone.offset_at(-far - 1), -18_000);
            assert_eq!(zone.offset_at(0), -14_400);
            assert_eq!(zone.offset_at(far), -18_000);

            ruled.extend(b"\n<+09>-9\n");
            let zone = parse(&ruled).unwrap();
            assert_eq!(zone.offset_at(0), -14_400);
            assert_eq!(zone.offset_at(far - 1), -14_400);
            assert_eq!(zone.offset_at(far), 32_400);
        }
    }

    #[test]
    fn what_is_cut_short_or_is_not_such_a_file_is_refused() {
        let first = table(0, 4, CHANGES, OFFSETS);
        for length in 0..first.len() {
            assert!(parse(&first[..length]).is_none(), "{length}");
        }
        let mut second = table(b'2', 4, &[], &[0]);
        second.extend(table(b'2', 8, CHANGES, OFFSETS));
        second.extend(b"\nJST-9\n");
        assert!(parse(&second).is_some());
        for length in 0..second.len() {
            assert!(parse(&second[..length]).is_none(), "{length}");
        }
        let mut other = first.clone();
        other[0] = b'X';
        assert!(parse(&other).is_none());
        // a change to a kind the file does not have
        assert!(parse(&table(0, 4, &[(0, 2)], OFFSETS)).is_none());
        assert!(parse(&table(0, 4, &[], &[])).is_none());
        let mut misruled = table(b'2', 4, &[], &[0]);
        misruled.extend(table(b'2', 8, CHANGES, OFFSETS));
        misruled.extend(b"\nnot a rule\n");
        assert!(parse(&misruled).is_none());
    }

    /// Where the six counts of a header begin.
    const COUNTS: usize = 20;

    #[test]
    fn a_count_greater_than_what_is_left_of_the_file_is_not_believed() {
        let first = table(0, 4, CHANGES, OFFSETS);
        let mut second = table(b'2', 4, &[(5, 0)], &[60]);
        let later = second.len();
        second.extend(table(b'2', 8, CHANGES, OFFSETS));
        second.extend(b"\nJST-9\n");
        assert!(parse(&first).is_some() && parse(&second).is_some());
        for (file, header) in [(&first, 0), (&second, 0), (&second, later)] {
            for field in 0..6 {
                for count in [u32::MAX, u32::MAX / 2, 1 << 24, file.len() as u32] {
                    let mut file = file.clone();
                    let at = header + COUNTS + 4 * field;
                    file[at..at + 4].copy_from_slice(&count.to_be_bytes());
                    assert!(parse(&file).is_none(), "{field} {count}");
                }
            }
        }
    }

    fn next(state: &mut u64) -> usize {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 11) as usize
    }

    /// Reads `bytes` and asks what was read: nothing it holds is more
    /// than the file had.
    fn tried(bytes: &[u8]) -> bool {
        let Some(zone) = parse(bytes) else {
            return false;
        };
        assert!(zone.changes.capacity() <= bytes.len());
        for instant in [i64::MIN, -1 << 40, 0, 1_112_911_993, 1 << 40, i64::MAX] {
            zone.offset_at(instant);
        }
        for wall in [i64::MIN, 0, 1_112_911_993, 4_102_444_799, i64::MAX] {
            zone.offset_of_local(wall);
        }
        true
    }

    #[test]
    fn no_bytes_make_reading_a_file_or_asking_what_was_read_fail() {
        let mut state = 0x2545_F491_4F6C_DD1D;
        let mut files = vec![table(0, 4, CHANGES, OFFSETS)];
        let mut second = table(b'3', 4, &[(5, 0)], &[60]);
        second.extend(table(b'3', 8, CHANGES, OFFSETS));
        second.extend(b"\nEST5EDT,M3.2.0,M11.1.0\n");
        files.push(second);
        // a file of the system, which has some hundred changes
        files.push(std::fs::read("/usr/share/zoneinfo/America/New_York").unwrap());
        let (mut tries, mut read) = (0, 0);
        for file in &files {
            assert!(tried(file));
            for length in 0..file.len() {
                read += usize::from(tried(&file[..length]));
                let mut changed = file.clone();
                changed[length] = next(&mut state) as u8;
                read += usize::from(tried(&changed));
                changed[length] = [0, 0xFF, 0x7F, 0x80][length % 4];
                read += usize::from(tried(&changed));
                tries += 3;
            }
            for _ in 0..500 {
                let mut changed = file.clone();
                let field = COUNTS + 4 * (next(&mut state) % 6);
                let count = (next(&mut state) as u32) >> (next(&mut state) % 32);
                changed[field..field + 4].copy_from_slice(&count.to_be_bytes());
                read += usize::from(tried(&changed));
                tries += 1;
            }
        }
        for _ in 0..4000 {
            let length = next(&mut state) % 300;
            let mut bytes: Vec<u8> = (0..length).map(|_| next(&mut state) as u8).collect();
            if next(&mut state).is_multiple_of(2) && length >= 5 {
                bytes[..5].copy_from_slice(b"TZif2");
            }
            read += usize::from(tried(&bytes));
            tries += 1;
        }
        assert!(tries > 9000, "{tries}");
        // most changes leave a file that reads: what was read was asked
        assert!(read > 1000, "{read} of {tries}");
    }
}

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

    /// Where the second header of a file of a later version begins.
    fn second_header(file: &[u8]) -> Option<usize> {
        let (version, counts) = header(&mut Reader(file))?;
        (version >= b'2').then(|| 44 + counts.table(4))
    }

    fn one_of<T: Copy>(of: &[T], state: &mut u64) -> T {
        of[next(state) % of.len()]
    }

    /// A table with a header, of few things of each kind so that the
    /// file is long enough for its counts: instants in their order or
    /// not, at either end of what an instant can be, kinds of local time
    /// the table has or does not have, offsets at either end of theirs.
    /// One time in eight a count of the header is not that of the table.
    fn made_table(state: &mut u64, version: u8, width: usize) -> Vec<u8> {
        let changes = next(state) % 7;
        let kinds = match next(state) % 8 {
            0 => 0,
            _ => 1 + next(state) % 4,
        };
        let [letters, leaps, standard, ut] = [7, 3, 4, 4].map(|most| next(state) % most);
        let mut counts = [ut, standard, leaps, changes, kinds, letters];
        if next(state).is_multiple_of(8) {
            counts[next(state) % 6] = one_of(&[0, 1, 2, 5, 40, 1 << 20, u32::MAX as usize], state);
        }
        let mut bytes = b"TZif".to_vec();
        bytes.push(version);
        bytes.extend([0; 15]);
        for count in counts {
            bytes.extend((count as u32).to_be_bytes());
        }
        let ordered = !next(state).is_multiple_of(4);
        let mut instant = one_of(&[i64::MIN, -(1 << 40), -5, 0, 1_112_911_993], state);
        for _ in 0..changes {
            match width {
                8 => bytes.extend(instant.to_be_bytes()),
                _ => bytes.extend((instant as i32).to_be_bytes()),
            }
            instant = match ordered {
                true => instant.saturating_add(1 + (next(state) % (1 << 30)) as i64),
                false => one_of(&[i64::MIN, 0, i64::MAX, next(state) as i64], state),
            };
            if ordered && next(state).is_multiple_of(16) {
                instant = i64::MAX;
            }
        }
        for _ in 0..changes {
            let has = !next(state).is_multiple_of(8) && kinds > 0;
            bytes.push(if has {
                (next(state) % kinds) as u8
            } else {
                0xFF
            });
        }
        for _ in 0..kinds {
            let offset = one_of(
                &[i32::MIN, i32::MAX, 0, 3600, -18_000, next(state) as i32],
                state,
            );
            bytes.extend(offset.to_be_bytes());
            bytes.extend([next(state) as u8, next(state) as u8]);
        }
        let rest = letters + leaps * (width + 4) + standard + ut;
        bytes.extend((0..rest).map(|_| one_of(&[0, b'X', next(state) as u8], state)));
        bytes
    }

    /// A file of a version there is, with one table or two as the
    /// version has, and after two a last line that is a rule, is none,
    /// lacks an end, or holds what no rule does.
    fn made(state: &mut u64) -> Vec<u8> {
        let version = one_of(&[0, b'2', b'2', b'3', b'4'], state);
        let mut bytes = made_table(state, version, 4);
        if version == 0 {
            return bytes;
        }
        bytes.extend(made_table(state, version, 8));
        let lines: [&[u8]; 12] = [
            b"\n\n",
            b"\nJST-9\n",
            b"\nEST5EDT,M3.2.0,M11.1.0\n",
            b"\nAEST-10AEDT,M10.1.0,M4.1.0/3\n",
            b"\n<-03>3<-02>,M3.5.0/-2,M10.5.0/-1\n",
            b"\nXYZ5ABC,J60/1,300\n",
            b"\nJST-9",
            b"",
            b"\nJST\0-9\n",
            b"\nXXX99999999999999999999999999\n",
            b"\nEST5EDT,M3.2.0/9223372036854775807,M11.1.0\n",
            b"\n\xFF\xFE\n\n",
        ];
        // the lines that are a rule or none are the likelier
        let line = match next(state) % 4 {
            0 => one_of(&lines, state),
            _ => one_of(&lines[..6], state),
        };
        bytes.extend(line);
        bytes
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
            // a count of either header is made another
            let headers = [Some(0), second_header(file)];
            for header in headers.into_iter().flatten() {
                for _ in 0..500 {
                    let mut changed = file.clone();
                    let field = header + COUNTS + 4 * (next(&mut state) % 6);
                    let count = (next(&mut state) as u32) >> (next(&mut state) % 32);
                    changed[field..field + 4].copy_from_slice(&count.to_be_bytes());
                    read += usize::from(tried(&changed));
                    tries += 1;
                }
            }
        }
        assert!(tries > 9000, "{tries}");
        // a change of one byte leaves most files to be read and asked
        assert!(read * 100 >= tries * CHANGED_READ, "{read} of {tries}");
    }

    /// Of a hundred files, how many at the least are read: of those with
    /// a byte or a count changed, and of those made part by part.
    const CHANGED_READ: usize = 30;
    const MADE_READ: usize = 15;

    #[test]
    fn no_file_made_part_by_part_makes_reading_it_fail() {
        let mut state = 0x9E37_79B9_7F4A_7C15;
        let (tries, mut read) = (8000, 0);
        let mut ruled = 0;
        for _ in 0..tries {
            let file = made(&mut state);
            read += usize::from(tried(&file));
            ruled += usize::from(parse(&file).is_some_and(|zone| zone.rule.is_some()));
            // and the same cut short, or with something after it
            tried(&file[..next(&mut state) % (file.len() + 1)]);
            tried(&[&file[..], &[0, b'\n', 0xFF][..next(&mut state) % 4]].concat());
        }
        assert!(read * 100 >= tries * MADE_READ, "{read} of {tries}");
        assert!(ruled * 100 >= tries * MADE_READ / 2, "{ruled} of {tries}");
    }
}

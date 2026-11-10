use alt_byte_segment::{Error, Input, Limits, MAX_BYTES, MAX_MEMBERS, Segment, encode};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

fn fixture(values: &[Vec<u8>]) -> Vec<u8> {
    let mut sources: Vec<_> = values.iter().map(Cursor::new).collect();
    let mut inputs: Vec<_> = values
        .iter()
        .zip(&mut sources)
        .map(|(data, source)| Input {
            digest: *blake3::hash(data).as_bytes(),
            bytes: data.len() as u64,
            source,
        })
        .collect();
    let mut output = Vec::new();
    let written = encode(&mut output, &mut inputs, Limits::default()).unwrap();
    assert_eq!(written.index.file_bytes(), output.len() as u64);
    let mut reopened = Segment::open(Cursor::new(&output), Limits::default()).unwrap();
    assert_eq!(reopened.index(), &written.index);
    assert_eq!(reopened.recorded_checksum(), &written.checksum);
    reopened.verify().unwrap();
    output
}

fn repair_index(bytes: &mut [u8]) {
    let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let digest = blake3::hash(&bytes[64..64 + 48 * count]);
    bytes[32..64].copy_from_slice(digest.as_bytes());
}
fn repair_complete(bytes: &mut [u8]) {
    let end = bytes.len() - 32;
    let digest = blake3::hash(&bytes[..end]);
    bytes[end..].copy_from_slice(digest.as_bytes());
}

#[test]
fn expected_checksum_cannot_be_replaced_by_a_repaired_trailer() {
    let original = fixture(&[b"original payload".to_vec()]);
    let expected = *Segment::open(Cursor::new(&original), Limits::default())
        .unwrap()
        .recorded_checksum();
    Segment::open(Cursor::new(&original), Limits::default())
        .unwrap()
        .verify_checksum(&expected)
        .unwrap();
    let mut changed = original.clone();
    let position = 64 + 48;
    changed[position] ^= 1;
    repair_complete(&mut changed);
    let mut current = Segment::open(Cursor::new(&changed), Limits::default()).unwrap();
    assert!(current.verify_checksum(&expected).is_err());
    // A container's own repaired checksum does not prove member identities.
    let untrusted = *current.recorded_checksum();
    current.verify_checksum(&untrusted).unwrap();
    assert!(current.verify().is_err());
}

#[test]
fn deterministic_original_bytes_and_bounded_random_access() {
    let values = vec![
        b"first member".to_vec(),
        vec![],
        vec![0xab; 131073],
        b"last".to_vec(),
    ];
    let bytes = fixture(&values);
    let mut reversed = values.clone();
    reversed.reverse();
    assert_eq!(bytes, fixture(&reversed));
    let mut segment = Segment::open(Cursor::new(&bytes), Limits::default()).unwrap();
    for value in &values {
        let digest = blake3::hash(value);
        let mut reader = segment.member(digest.as_bytes()).unwrap().unwrap();
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!(&actual, value);
        assert_eq!(reader.stream_position().unwrap(), value.len() as u64);
        assert!(reader.seek(SeekFrom::Current(1)).is_err());
        assert_eq!(reader.stream_position().unwrap(), value.len() as u64);
        assert!(reader.seek(SeekFrom::Start(u64::MAX)).is_err());
        assert!(reader.seek(SeekFrom::End(i64::MIN)).is_err());
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        if !value.is_empty() {
            reader.seek(SeekFrom::End(-1)).unwrap();
            let mut last = [0; 2];
            assert_eq!(reader.read(&mut last).unwrap(), 1);
            assert_eq!(last[0], *value.last().unwrap());
            assert_eq!(reader.read(&mut last).unwrap(), 0);
        }
    }
    assert!(segment.member(&[0xff; 32]).unwrap().is_none());
    let mut owned = segment
        .into_member(blake3::hash(&values[0]).as_bytes())
        .unwrap()
        .unwrap();
    let mut actual = Vec::new();
    owned.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, values[0]);
}

#[test]
fn every_truncation_and_trailing_bytes_are_rejected() {
    let original = fixture(&[b"payload".to_vec()]);
    for end in 0..original.len() {
        assert!(
            Segment::open(Cursor::new(&original[..end]), Limits::default()).is_err(),
            "{end}"
        );
    }
    let mut extra = original;
    extra.push(0);
    assert!(Segment::open(Cursor::new(extra), Limits::default()).is_err());
}

#[test]
fn malformed_header_index_order_and_ranges_are_rejected() {
    let original = fixture(&[b"one".to_vec(), b"two".to_vec()]);
    for case in 0..12 {
        let mut bytes = original.clone();
        match case {
            0 => bytes[0] ^= 1,
            1 => bytes[12] = 1,
            2 => bytes[8..12].copy_from_slice(&0u32.to_le_bytes()),
            3 => bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes()),
            4 => bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes()),
            5 => bytes[24..32].copy_from_slice(&u64::MAX.to_le_bytes()),
            6 => bytes[32] ^= 1,
            7 => {
                bytes[96..104].copy_from_slice(&0u64.to_le_bytes());
                repair_index(&mut bytes);
            }
            8 => {
                bytes[104..112].copy_from_slice(&u64::MAX.to_le_bytes());
                repair_index(&mut bytes);
            }
            9 => {
                let first = bytes[64..96].to_vec();
                bytes[112..144].copy_from_slice(&first);
                repair_index(&mut bytes);
            }
            10 => {
                for i in 0..48 {
                    bytes.swap(64 + i, 112 + i);
                }
                repair_index(&mut bytes);
            }
            11 => {
                bytes[152..160].copy_from_slice(&0u64.to_le_bytes());
                repair_index(&mut bytes);
            }
            _ => unreachable!(),
        }
        assert!(
            Segment::open(Cursor::new(bytes), Limits::default()).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn valid_index_or_complete_checksum_does_not_prove_member_bytes() {
    let original = fixture(&[b"original payload".to_vec()]);
    for repair in [false, true] {
        let mut bytes = original.clone();
        bytes[112] ^= 1;
        if repair {
            repair_complete(&mut bytes);
        }
        let mut segment = Segment::open(Cursor::new(bytes), Limits::default()).unwrap();
        assert!(matches!(segment.verify(), Err(Error::Format(_))));
    }
    let mut bytes = original;
    let end = bytes.len() - 1;
    bytes[end] ^= 1;
    assert!(
        Segment::open(Cursor::new(bytes), Limits::default())
            .unwrap()
            .verify()
            .is_err()
    );
}

#[test]
fn input_lengths_hashes_duplicates_and_limits_fail_before_completion() {
    for case in 0..6 {
        let mut source = Cursor::new(b"hello");
        let mut input = Input {
            digest: *blake3::hash(b"hello").as_bytes(),
            bytes: 5,
            source: &mut source,
        };
        let limits = match case {
            0 => {
                input.bytes = 4;
                Limits::default()
            }
            1 => {
                input.bytes = 6;
                Limits::default()
            }
            2 => {
                input.digest = [1; 32];
                Limits::default()
            }
            3 => {
                input.bytes = MAX_BYTES + 1;
                Limits::default()
            }
            4 => Limits {
                members: 0,
                bytes: MAX_BYTES,
            },
            5 => Limits {
                members: MAX_MEMBERS,
                bytes: 4,
            },
            _ => unreachable!(),
        };
        assert!(encode(&mut Vec::new(), &mut [input], limits).is_err());
    }
    assert!(encode(&mut Vec::new(), &mut [], Limits::default()).is_err());
    let mut sources = [Cursor::new(b"same"), Cursor::new(b"same")];
    let mut inputs: Vec<_> = sources
        .iter_mut()
        .map(|source| Input {
            digest: *blake3::hash(b"same").as_bytes(),
            bytes: 4,
            source,
        })
        .collect();
    let mut output = Vec::new();
    assert!(encode(&mut output, &mut inputs, Limits::default()).is_err());
    assert!(output.is_empty());
    let mut source = Cursor::new([]);
    let mut input = [Input {
        digest: [0; 32],
        bytes: u64::MAX,
        source: &mut source,
    }];
    assert!(encode(&mut Vec::new(), &mut input, Limits::default()).is_err());
}

#[test]
fn member_count_boundary_is_enforced_independently_of_bytes() {
    let values: Vec<_> = (0..MAX_MEMBERS as u32)
        .map(|i| i.to_le_bytes().to_vec())
        .collect();
    let bytes = fixture(&values);
    let segment = Segment::open(Cursor::new(&bytes), Limits::default()).unwrap();
    assert_eq!(segment.index().members().len(), MAX_MEMBERS);
    assert!(
        Segment::open(
            Cursor::new(bytes),
            Limits {
                members: MAX_MEMBERS - 1,
                bytes: MAX_BYTES
            }
        )
        .is_err()
    );
    let mut values = values;
    values.push(b"extra".to_vec());
    let mut sources: Vec<_> = values.iter().map(Cursor::new).collect();
    let mut inputs: Vec<_> = values
        .iter()
        .zip(&mut sources)
        .map(|(value, source)| Input {
            digest: *blake3::hash(value).as_bytes(),
            bytes: value.len() as u64,
            source,
        })
        .collect();
    let mut output = Vec::new();
    assert!(encode(&mut output, &mut inputs, Limits::default()).is_err());
    assert!(output.is_empty());
}

#[test]
fn write_errors_never_produce_a_completed_encoding() {
    struct FailAfter(usize);
    impl Write for FailAfter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::other("injected failure"));
            }
            let n = self.0.min(bytes.len());
            self.0 -= n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    for limit in [0, 1, 63, 64, 100, 112, 115, 119, 130, 150] {
        let mut source = Cursor::new(b"payload");
        let mut inputs = [Input {
            digest: *blake3::hash(b"payload").as_bytes(),
            bytes: 7,
            source: &mut source,
        }];
        assert!(matches!(
            encode(&mut FailAfter(limit), &mut inputs, Limits::default()),
            Err(Error::Io(_))
        ));
    }
}

#[test]
fn changed_extent_and_read_failures_are_not_successful_end_of_input() {
    use std::{cell::RefCell, rc::Rc};
    #[derive(Clone)]
    struct Shared(Rc<RefCell<Cursor<Vec<u8>>>>);
    impl Read for Shared {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.0.borrow_mut().read(bytes)
        }
    }
    impl Seek for Shared {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.0.borrow_mut().seek(from)
        }
    }
    let bytes = fixture(&[b"original".to_vec()]);
    let shared = Shared(Rc::new(RefCell::new(Cursor::new(bytes))));
    let mut segment = Segment::open(shared.clone(), Limits::default()).unwrap();
    shared.0.borrow_mut().get_mut().push(0);
    assert!(segment.verify().is_err());
    shared.0.borrow_mut().get_mut().pop();
    let mut member = segment
        .into_member(blake3::hash(b"original").as_bytes())
        .unwrap()
        .unwrap();
    shared.0.borrow_mut().get_mut().truncate(113);
    assert_eq!(
        member.read_to_end(&mut Vec::new()).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    struct Fail;
    impl Read for Fail {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }
    let mut source = Fail;
    let mut input = [Input {
        digest: [0; 32],
        bytes: 1,
        source: &mut source,
    }];
    assert!(matches!(
        encode(&mut Vec::new(), &mut input, Limits::default()),
        Err(Error::Io(_))
    ));
}

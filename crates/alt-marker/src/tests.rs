use super::*;

/// A state of twelve bytes behind a legacy head of eight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Count {
    n: u64,
    flags: u32,
}

impl Slot for Count {
    const LEGACY_LEN: usize = 8;
    const BODY_LEN: usize = 12;

    fn encode(&self, body: &mut [u8]) {
        body[..8].copy_from_slice(&self.n.to_le_bytes());
        body[8..].copy_from_slice(&self.flags.to_le_bytes());
    }

    fn decode(body: &[u8]) -> Self {
        Count {
            n: u64::from_le_bytes(body[..8].try_into().unwrap()),
            flags: u32::from_le_bytes(body[8..].try_into().unwrap()),
        }
    }
}

const SLOTS_AT: usize = 16;
const SLOT_LEN: usize = 28;

fn state(n: u64) -> Count {
    Count { n, flags: 0 }
}

fn said(path: &Path) -> Marker<Count> {
    read(path)
}

fn set(path: &Path, n: u64) {
    update(path, |_| Some(state(n))).unwrap();
}

fn serials(path: &Path) -> [Option<u64>; 2] {
    let data = std::fs::read(path).unwrap();
    [0, 1].map(|n| {
        let at = SLOTS_AT + n * SLOT_LEN;
        let slot = data.get(at..at + SLOT_LEN)?;
        decode::<Count>(slot).map(|(serial, _)| serial)
    })
}

fn flip(path: &Path, at: usize) {
    let mut data = std::fs::read(path).unwrap();
    data[at] ^= 0xff;
    std::fs::write(path, data).unwrap();
}

#[test]
fn the_layout_follows_from_the_slot() {
    assert_eq!(
        (slots_at::<Count>(), slot_len::<Count>(), len::<Count>()),
        (SLOTS_AT, SLOT_LEN, 72)
    );
}

#[test]
fn writes_take_turns_between_the_two_slots() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("durable");
    set(&path, 10);
    let data = std::fs::read(&path).unwrap();
    assert_eq!(data.len(), len::<Count>());
    assert_eq!(data[..8], [0u8; 8], "zero for an older format");
    assert_eq!(&data[8..12], b"ALT2");
    assert_eq!(serials(&path), [Some(1), None]);
    let mut turns = Vec::new();
    for n in 11..15 {
        set(&path, n);
        assert_eq!(said(&path), Marker::Recorded(state(n)));
        turns.push(serials(&path));
    }
    let expected = [
        [Some(1), Some(2)],
        [Some(3), Some(2)],
        [Some(3), Some(4)],
        [Some(5), Some(4)],
    ];
    assert_eq!(turns, expected);
    // writing what is there already writes nothing
    set(&path, 14);
    assert_eq!(serials(&path), [Some(5), Some(4)]);
}

#[test]
fn a_slot_that_does_not_check_gives_way_to_the_other() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("durable");
    for n in 10..13 {
        set(&path, n);
    }
    assert_eq!(serials(&path), [Some(3), Some(2)]);
    // the write of serial 3 torn: the state before it stands
    flip(&path, SLOTS_AT + 10);
    assert_eq!(said(&path), Marker::Recorded(state(11)));
    // and the next write goes to the broken slot
    set(&path, 20);
    assert_eq!(serials(&path), [Some(3), Some(2)]);
    assert_eq!(said(&path), Marker::Recorded(state(20)));

    flip(&path, SLOTS_AT + 10);
    flip(&path, SLOTS_AT + SLOT_LEN + 10);
    assert_eq!(said(&path), Marker::Unreadable);
    set(&path, 30);
    assert_eq!(serials(&path), [Some(1), None]);
    assert_eq!(said(&path), Marker::Recorded(state(30)));
}

#[test]
fn the_slot_with_the_larger_serial_is_the_state() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("durable");
    for (first, second, current) in [(9, 7, 5), (7, 9, 50)] {
        let mut data = vec![0u8; SLOTS_AT];
        data[8..12].copy_from_slice(&MAGIC);
        data.extend_from_slice(&encode(&state(5), first));
        data.extend_from_slice(&encode(&state(50), second));
        std::fs::write(&path, data).unwrap();
        assert_eq!(said(&path), Marker::Recorded(state(current)));
    }
}

#[test]
fn an_older_marker_is_told_from_a_broken_one_and_from_none() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("durable");
    assert_eq!(said(&path), Marker::Absent);
    // an update with nothing to write does not create it
    update(&path, |_: &Marker<Count>| None).unwrap();
    assert!(!path.exists());

    let older = 7u64.to_le_bytes();
    for extra in [&b""[..], &b"ALTD and what another format put after it"[..]] {
        std::fs::write(&path, [&older[..], extra].concat()).unwrap();
        assert_eq!(said(&path), Marker::Legacy(older.to_vec()));
    }
    for short in [0, 1, 7] {
        std::fs::write(&path, vec![0u8; short]).unwrap();
        assert_eq!(said(&path), Marker::Unreadable, "{short} bytes");
    }
    let mut cut = vec![0u8; SLOTS_AT];
    cut[8..12].copy_from_slice(&MAGIC);
    std::fs::write(&path, cut).unwrap();
    assert_eq!(said(&path), Marker::Unreadable, "no slot");

    // an older marker is replaced whole
    std::fs::write(&path, older).unwrap();
    set(&path, 10);
    assert_eq!(said(&path), Marker::Recorded(state(10)));
    let data = std::fs::read(&path).unwrap();
    assert_eq!((data.len(), &data[..8]), (len::<Count>(), &[0u8; 8][..]));
}

#[test]
fn updates_from_many_threads_are_all_kept() {
    const THREADS: u64 = 8;
    const EACH: u64 = 100;
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("durable");
    let threads: Vec<_> = (0..THREADS)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                for _ in 0..EACH {
                    let count = |marker: &Marker<Count>| {
                        let before = match marker {
                            Marker::Recorded(state) => state.n,
                            _ => 0,
                        };
                        Some(state(before + 1))
                    };
                    update(&path, count).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(said(&path), Marker::Recorded(state(THREADS * EACH)));
}

#[test]
fn a_marker_reads_the_same_whatever_follows_it_in_the_file() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("durable");
    set(&path, 40);
    let mut longer = std::fs::read(&path).unwrap();
    longer.extend_from_slice(&[7; 300]);
    std::fs::write(&path, longer).unwrap();
    assert_eq!(said(&path), Marker::Recorded(state(40)));
    // an older format's marker is shorter than this one
    std::fs::write(&path, 9u64.to_le_bytes()).unwrap();
    assert_eq!(said(&path), Marker::Legacy(9u64.to_le_bytes().to_vec()));
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(said(&path), Marker::Unreadable);
}

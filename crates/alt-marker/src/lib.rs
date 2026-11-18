//! A marker file: one small state, kept so that a crash in the middle of
//! a change leaves the state before it.
//!
//! ```text
//! [0..L)        zero, where an older format keeps its own L bytes
//! [L..L+4)      magic "ALT2"
//! [L+4..L+8)    zero
//! slot 0, slot 1
//! slot: [serial u64][body][checksum 8 = blake3(serial and body)[..8]]
//! ```
//!
//! What the body holds, how long it is and how long `L` is are the
//! owner's ([`Slot`]). Every change is made in place under an exclusive
//! lock on the marker file itself ([`update`]): it goes to the slot that
//! does not hold the current state, with the next serial, and is
//! fdatasynced. A reader takes the valid slot with the larger serial and
//! no lock ([`read`]).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: [u8; 4] = *b"ALT2";

/// The state a marker records, as its owner lays it out in a slot.
pub trait Slot: Copy + PartialEq {
    /// how many bytes an older format keeps at the head of the file; the
    /// magic of this one follows them
    const LEGACY_LEN: usize;
    /// how long the encoded state is
    const BODY_LEN: usize;

    /// Writes the state into `body`, which is `BODY_LEN` bytes long.
    fn encode(&self, body: &mut [u8]);

    /// The state `body` holds; its checksum held.
    fn decode(body: &[u8]) -> Self;
}

/// What a marker file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker<S> {
    Recorded(S),
    /// the first `LEGACY_LEN` bytes of a file an older format wrote
    Legacy(Vec<u8>),
    /// shorter than any marker, or no slot of it holds
    Unreadable,
    Absent,
}

const fn slots_at<S: Slot>() -> usize {
    S::LEGACY_LEN + 8
}

const fn slot_len<S: Slot>() -> usize {
    8 + S::BODY_LEN + 8
}

/// How long a marker file of `S` is.
pub const fn len<S: Slot>() -> usize {
    slots_at::<S>() + 2 * slot_len::<S>()
}

fn check(bytes: &[u8]) -> [u8; 8] {
    alt_hash::blake3::hash(bytes).as_bytes()[..8]
        .try_into()
        .unwrap()
}

fn encode<S: Slot>(state: &S, serial: u64) -> Vec<u8> {
    let mut slot = vec![0u8; slot_len::<S>()];
    slot[..8].copy_from_slice(&serial.to_le_bytes());
    state.encode(&mut slot[8..8 + S::BODY_LEN]);
    let sum = check(&slot[..8 + S::BODY_LEN]);
    slot[8 + S::BODY_LEN..].copy_from_slice(&sum);
    slot
}

/// The serial and state of a slot whose checksum holds.
fn decode<S: Slot>(slot: &[u8]) -> Option<(u64, S)> {
    let (held, sum) = slot.split_at(8 + S::BODY_LEN);
    if check(held) != *sum {
        return None;
    }
    let serial = u64::from_le_bytes(held[..8].try_into().unwrap());
    Some((serial, S::decode(&held[8..])))
}

/// What `data` says and, for a marker in this format, the slot the next
/// write goes to with its serial.
fn parse<S: Slot>(data: &[u8]) -> (Marker<S>, Option<(usize, u64)>) {
    if data.len() < S::LEGACY_LEN {
        return (Marker::Unreadable, None);
    }
    if data.get(S::LEGACY_LEN..S::LEGACY_LEN + 4) != Some(&MAGIC[..]) {
        return (Marker::Legacy(data[..S::LEGACY_LEN].to_vec()), None);
    }
    let slot = |n: usize| {
        let at = slots_at::<S>() + n * slot_len::<S>();
        data.get(at..at + slot_len::<S>()).and_then(decode::<S>)
    };
    match (slot(0), slot(1)) {
        (None, None) => (Marker::Unreadable, None),
        (Some((serial, state)), None) => (Marker::Recorded(state), Some((1, serial + 1))),
        (None, Some((serial, state))) => (Marker::Recorded(state), Some((0, serial + 1))),
        (Some((first, state)), Some((second, _))) if first > second => {
            (Marker::Recorded(state), Some((1, first + 1)))
        }
        (Some(_), Some((second, state))) => (Marker::Recorded(state), Some((0, second + 1))),
    }
}

/// A whole marker file holding `state`, for a file that is not one in this
/// format: it is replaced in one write.
fn fresh<S: Slot>(state: &S) -> Vec<u8> {
    let mut file = vec![0u8; len::<S>()];
    file[S::LEGACY_LEN..S::LEGACY_LEN + 4].copy_from_slice(&MAGIC);
    let at = slots_at::<S>();
    file[at..at + slot_len::<S>()].copy_from_slice(&encode(state, 1));
    file
}

/// Reads the marker at `path`, taking no lock: one read of as much of the
/// file as a marker takes up.
pub fn read<S: Slot>(path: &Path) -> Marker<S> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Marker::Absent,
        Err(_) => return Marker::Unreadable,
    };
    let mut data = vec![0u8; len::<S>()];
    let mut have = 0;
    while have < data.len() {
        match file.read(&mut data[have..]) {
            Ok(0) => break,
            Ok(n) => have += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Marker::Unreadable,
        }
    }
    parse(&data[..have]).0
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    alt_os::sys::flock(file, alt_os::sys::LOCK_EX)
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

/// Changes the marker: `next` is given what it says and returns what it
/// is to say, or `None` to leave it as it is. Reading, deciding and writing
/// happen under an exclusive lock on the marker file, taken through a
/// descriptor opened here, so that threads of one process exclude each
/// other like processes do; nothing else is locked while it is held. The
/// new state is fdatasynced before this returns. A marker that is absent
/// is created only when there is something to write.
pub fn update<S: Slot>(path: &Path, next: impl Fn(&Marker<S>) -> Option<S>) -> std::io::Result<()> {
    let open = |create: bool| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .open(path)
    };
    let (mut file, created) = match open(false) {
        Ok(file) => (file, false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if next(&Marker::Absent).is_none() {
                return Ok(());
            }
            (open(true)?, true)
        }
        Err(e) => return Err(e),
    };
    lock_exclusive(&file)?;
    let mut data = Vec::with_capacity(len::<S>());
    file.read_to_end(&mut data)?;
    let (marker, slot) = match created && data.is_empty() {
        true => (Marker::Absent, None),
        false => parse(&data),
    };
    let Some(state) = next(&marker) else {
        return Ok(());
    };
    if marker == Marker::Recorded(state) {
        return Ok(());
    }
    let (at, bytes) = match slot {
        Some((n, serial)) => (
            slots_at::<S>() + n * slot_len::<S>(),
            encode(&state, serial),
        ),
        None => (0, fresh(&state)),
    };
    file.seek(SeekFrom::Start(at as u64))?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    if let (true, Some(dir)) = (created, path.parent()) {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

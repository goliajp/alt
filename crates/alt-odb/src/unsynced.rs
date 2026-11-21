//! Appends that were left unsynced, and the boot they were made in.
//!
//! A writer that leaves its appends unsynced marks the synced marker so
//! ([`crate::synced::distrust`]). What it appended is in the kernel's
//! cache until the machine goes down: for as long as the boot is the same
//! every record past the marker's boundaries is as it was written, and
//! nobody has to look at it. After another boot any of the four files may
//! end anywhere past its boundary, each on its own: a record that names
//! content can be there without the content.
//!
//! So before the marker is marked, the lock file gets a note of the
//! running boot and of the boundaries the marker holds then:
//!
//! ```text
//! [boot 16][pack seq u32][reserved 4]
//! [pack u64][blob map u64][tier1 u64][map.alt u64][checksum 8]      at byte 1224
//! ```
//!
//! It is not fsynced: a note that was lost, or that reads as another
//! boot's, sends whoever opens the store to the records. A writer of this
//! format takes the note away when it has looked at them and has given the
//! marker boundaries that hold. An older alt does neither: it takes
//! records past the boundaries as it finds them and moves the boundaries
//! over them. The note it leaves behind then tells of it: the next writer
//! of this format looks at the records past the boundaries of the note
//! ([`unseen`]). One of them that names content which is not stored lies
//! where the marker says the files were synced, with whatever the older
//! alt appended after it: it is reported as damage and nothing is cut.

use std::fs::File;
use std::os::unix::fs::FileExt;

use crate::head::{UNSYNCED_AT, check};
use alt_store::Basis;

use crate::synced::{Lens, Log, Marker, State};

const BODY: usize = 56;
pub(crate) const LEN: usize = BODY + 8;

/// The note as a handle last read it, shared with what judges a record
/// of the files that does not read.
pub(crate) type Noted = std::sync::Arc<std::sync::Mutex<Option<Left>>>;

/// The note: the boot unsynced appends were made in, and the boundaries
/// they lie past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Left {
    pub boot: [u8; 16],
    pub whole: Lens,
}

impl Left {
    pub fn parse(data: &[u8]) -> Option<Left> {
        let (body, sum) = data.get(..LEN)?.split_at(BODY);
        if check(body) != *sum {
            return None;
        }
        let seq = u32::from_le_bytes(body[16..20].try_into().unwrap());
        Some(Left {
            boot: body[..16].try_into().unwrap(),
            whole: Lens::decode(seq, &body[24..]),
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(LEN);
        bytes.extend_from_slice(&self.boot);
        bytes.extend_from_slice(&self.whole.seq.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        self.whole.encode(&mut bytes);
        let sum = check(&bytes);
        bytes.extend_from_slice(&sum);
        bytes
    }
}

/// What the lock file notes of unsynced appends.
pub(crate) fn read(lock: &File) -> Option<Left> {
    let mut bytes = [0u8; LEN];
    lock.read_exact_at(&mut bytes, UNSYNCED_AT as u64).ok()?;
    Left::parse(&bytes)
}

/// Notes that appends past `whole` are left unsynced from now on, in the
/// running boot, which is returned; where the kernel tells no boot the
/// note is taken away.
pub(crate) fn note(lock: &File, whole: Lens) -> std::io::Result<Option<[u8; 16]>> {
    let Some(boot) = alt_os::boot::id() else {
        forget(lock)?;
        return Ok(None);
    };
    lock.write_all_at(&Left { boot, whole }.encode(), UNSYNCED_AT as u64)?;
    Ok(Some(boot))
}

/// Takes the note away: the marker holds boundaries with nothing left
/// unsynced past them that was not looked at.
pub(crate) fn forget(lock: &File) -> std::io::Result<()> {
    lock.write_all_at(&[0u8; LEN], UNSYNCED_AT as u64)
}

/// Whether the records past the boundaries of `marker` are known to be
/// as they were written: left unsynced in the running boot, by what the
/// note says, and nowhere else.
pub(crate) fn cached(marker: &Marker, note: Option<Left>) -> bool {
    let here = note.is_some_and(|left| alt_os::boot::id() == Some(left.boot));
    here && !marker.trusted() && marker.recorded().is_some()
}

/// What `marker` tells about `log` not reading to its end
/// ([`Marker::basis`]); where the records past its boundaries are
/// `cached`, only the last one can be an append that never finished.
pub(crate) fn basis(
    marker: &Marker,
    cached: bool,
    (log, seq, len): (Log, u32, u64),
    bounded: impl Fn() -> bool,
) -> Basis {
    match marker.recorded() {
        Some(_) if cached => Basis::LastOnly,
        _ => marker.basis(log, seq, len, bounded),
    }
}

/// The marker a writer looks at the records by, where `note` is of
/// another boot and `marker` is not one that says appends were left
/// unsynced: an older alt, which keeps no note, recorded the files as
/// synced over what that boot left, without a look at it, or the marker
/// is gone. The records past the boundaries of the note are looked at;
/// what the marker says of the files stays.
pub(crate) fn unseen(marker: &Marker, note: Option<Left>) -> Option<Marker> {
    let left = note.filter(|left| alt_os::boot::id() != Some(left.boot))?;
    let whole = match marker.recorded() {
        Some(state) if marker.trusted() => state.whole.min(left.whole),
        Some(_) => return None,
        None => left.whole,
    };
    Some(Marker::Recorded(State {
        generation: marker.generation(),
        flags: 0,
        eof: whole,
        whole,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synced::UNTRUSTED;

    fn lens(n: u64) -> Lens {
        Lens {
            seq: 1,
            pack: 100 * n,
            blobmap: 10 * n,
            tier1: 5,
            map: 20 * n,
        }
    }

    fn state(flags: u32, whole: Lens) -> State {
        State {
            generation: 3,
            flags,
            eof: whole,
            whole,
        }
    }

    #[test]
    fn a_note_reads_back_as_written_and_what_is_cut_or_changed_is_no_note() {
        let left = Left {
            boot: [7; 16],
            whole: lens(4),
        };
        let bytes = left.encode();
        assert_eq!(bytes.len(), LEN);
        assert_eq!(Left::parse(&bytes), Some(left));
        assert_eq!(Left::parse(&bytes[..LEN - 1]), None);
        assert_eq!(Left::parse(&[0; LEN]), None);
        for at in 0..LEN {
            let mut changed = bytes.clone();
            changed[at] ^= 1;
            assert_eq!(Left::parse(&changed), None, "byte {at}");
        }
    }

    #[test]
    fn appends_are_as_written_only_under_a_note_of_the_running_boot() {
        let Some(boot) = alt_os::boot::id() else {
            return;
        };
        let here = Some(Left {
            boot,
            whole: lens(2),
        });
        let mut other = boot;
        other[0] ^= 1;
        let elsewhere = Some(Left {
            boot: other,
            whole: lens(2),
        });
        let left = Marker::Recorded(state(UNTRUSTED, lens(2)));
        assert!(cached(&left, here));
        assert!(!cached(&left, None) && !cached(&left, elsewhere));
        // boundaries that hold have nothing unsynced past them
        let synced = Marker::Recorded(state(0, lens(5)));
        assert!(!cached(&synced, here) && !cached(&synced, elsewhere));
        for none in [Marker::Absent, Marker::Unreadable, Marker::Legacy([9; 3])] {
            assert!(!cached(&none, here));
            // nothing vouches for what the other boot left
            let from_note = Marker::Recorded(State {
                generation: 0,
                ..state(0, lens(2))
            });
            assert_eq!(unseen(&none, elsewhere), Some(from_note));
            assert_eq!(unseen(&none, here), None);
        }
        // an older alt moved them over appends of another boot unseen
        let looked_by = Marker::Recorded(state(0, lens(2)));
        assert_eq!(unseen(&synced, elsewhere), Some(looked_by));
        assert_eq!(unseen(&synced, here), None);
        assert_eq!(unseen(&synced, None), None);
        assert_eq!(unseen(&left, elsewhere), None);
    }
}

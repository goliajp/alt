//! The record of a removal of lost records that is under way, and
//! carrying one out.
//!
//! Records are taken out of the blob map, the Tier 1 map and `map.alt` by
//! writing over them in place ([`Removal`]). A power cut can leave such a
//! write half made, and a record that does not read where records were
//! synced stops every command. So what is about to be written is first
//! kept in `odb.repair`, fsynced, and a removal that was interrupted is
//! made again from there.
//!
//! Before `odb.repair` is written the lock file notes, fsynced, that a
//! removal is under way, in eight bytes past its note of unsynced
//! appends. Whoever opens the store to write, or takes its write lock,
//! reads the lock file there anyway and finishes the removal before it
//! does anything else: no append is made to a log a removal is to be
//! finished on. A command that only reads changes no file: it reads on
//! where every record reads, and where one does not it says that a
//! removal was interrupted.
//!
//! After those eight bytes the lock file notes the marker's generation,
//! which moves whenever records are taken out of the store, as the last
//! writer left it: the number and its complement, not fsynced. A handle
//! reads it when it opens the store and compares the marker with it when
//! it takes the write lock: records taken out between the two, with as
//! many appended after them, leave the files as long as the handle read
//! them, and the handle reads them again. A note that fell behind the
//! marker, which an older alt replaces, costs that second read once.
//!
//! ```text
//! [magic "ALTV"][version u8][reserved 3]
//! 3 x [was u64][len u64][end 32][record len u32][count u32]
//!     count x [at u64][record]
//! [checksum 8]
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use alt_store::reclog::Removal;

use std::os::unix::fs::FileExt;

use crate::head::{self, UNSYNCED_AT, check};
use crate::lock::{hold_exclusive, release_exclusive};
use crate::synced::{self, Lens, State};
use crate::{NativeOdb, OdbError, unsynced};

const NAME: &str = "odb.repair";
/// Where the lock file notes that a removal is under way.
pub(crate) const NOTED_AT: usize = UNSYNCED_AT + unsynced::LEN;
pub(crate) const NOTED: [u8; 8] = *b"ALTVmove";
/// How long the two notes are together.
pub(crate) const NOTES: usize = NOTED.len() + 16;

/// What the lock file notes from [`NOTED_AT`] on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Notes {
    /// a removal is under way
    pub removal: bool,
    /// the marker's generation as the last writer left it
    pub generation: Option<u64>,
    pub boot_runs: Option<unsynced::boot_runs::BootRuns>,
}
const MAGIC: [u8; 8] = *b"ALTV\x01\0\0\0";
/// The logs records are removed from, in the order they are written.
pub(crate) const LOGS: [&str; 3] = ["store/manifests/blobmap", "tier1", "map.alt"];
const DAMAGED: OdbError = OdbError::Format("odb.repair does not read");

fn encode(removals: &[Removal; 3]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    for removal in removals {
        out.extend_from_slice(&removal.was.to_le_bytes());
        out.extend_from_slice(&removal.len.to_le_bytes());
        out.extend_from_slice(&removal.end);
        let rec_len = removal.moves.first().map_or(0, |(_, record)| record.len());
        out.extend_from_slice(&(rec_len as u32).to_le_bytes());
        out.extend_from_slice(&(removal.moves.len() as u32).to_le_bytes());
        for (at, record) in &removal.moves {
            out.extend_from_slice(&at.to_le_bytes());
            out.extend_from_slice(record);
        }
    }
    let sum = check(&out);
    out.extend_from_slice(&sum);
    out
}

fn decode(data: &[u8]) -> Option<[Removal; 3]> {
    let (mut body, sum) = data.split_at_checked(data.len().checked_sub(8)?)?;
    if check(body) != *sum || body.split_off(..8)? != MAGIC {
        return None;
    }
    let mut take = |n: usize| body.split_off(..n);
    let u64_of = |bytes: &[u8]| u64::from_le_bytes(bytes.try_into().unwrap());
    let u32_of = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().unwrap()) as usize;
    let mut removals = Vec::with_capacity(3);
    for _ in 0..3 {
        let (was, len) = (u64_of(take(8)?), u64_of(take(8)?));
        let end = take(32)?.try_into().unwrap();
        let (rec_len, count) = (u32_of(take(4)?), u32_of(take(4)?));
        let mut moves = Vec::new();
        for _ in 0..count {
            moves.push((u64_of(take(8)?), take(rec_len)?.to_vec()));
        }
        removals.push(Removal {
            was,
            len,
            end,
            moves,
        });
    }
    removals.try_into().ok()
}

/// What `noted`, the bytes of the lock file from [`NOTED_AT`] on, says.
pub(crate) fn notes(noted: Option<&[u8]>) -> Notes {
    let noted = noted.unwrap_or_default();
    let word = |at: usize| {
        let bytes = noted.get(at..at + 8)?;
        Some(u64::from_le_bytes(bytes.try_into().unwrap()))
    };
    let generation = match (word(NOTED.len()), word(NOTED.len() + 8)) {
        (Some(generation), Some(check)) if check == !generation => Some(generation),
        _ => None,
    };
    Notes {
        removal: noted.starts_with(&NOTED),
        generation,
        boot_runs: noted
            .get(NOTES..)
            .and_then(unsynced::boot_runs::BootRuns::parse),
    }
}

/// Notes `generation` in the lock file `lock` as the marker's. Not
/// fsynced, failures ignored: a note that is lost or behind sends a
/// handle to read the store a second time.
pub(crate) fn note_generation(lock: &File, generation: u64) {
    let note = [generation.to_le_bytes(), (!generation).to_le_bytes()].concat();
    let _ = lock.write_all_at(&note, (NOTED_AT + NOTED.len()) as u64);
}

/// Keeps `removals` on disk, fsynced, before the first of them is made;
/// the lock file `lock` notes first, fsynced, that there is a removal.
pub(crate) fn save(alt_dir: &Path, lock: &File, removals: &[Removal; 3]) -> std::io::Result<()> {
    lock.write_all_at(&NOTED, NOTED_AT as u64)?;
    lock.sync_data()?;
    let tmp = alt_dir.join("odb.repair.tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(&encode(removals))?;
    file.sync_all()?;
    fs::rename(&tmp, alt_dir.join(NAME))?;
    File::open(alt_dir)?.sync_all()
}

/// The removals an interrupted run kept, when there are any.
fn kept(alt_dir: &Path) -> Result<Option<[Removal; 3]>, OdbError> {
    match fs::read(alt_dir.join(NAME)) {
        Ok(data) => decode(&data).map(Some).ok_or(DAMAGED),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Forgets the removals kept by [`save`]: they are made. The note of
/// the lock file goes last and is not fsynced: one that outlives the
/// removals is taken away by the next writer, which finds none kept.
pub(crate) fn forget(alt_dir: &Path, lock: &File) -> std::io::Result<()> {
    match fs::remove_file(alt_dir.join(NAME)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => File::open(alt_dir)?.sync_all()?,
    }
    lock.write_all_at(&[0; NOTED.len()], NOTED_AT as u64)
}

/// With the write lock `lock` held: makes the removals an interrupted
/// run kept, where there are any, and takes the note of them away.
pub(crate) fn finish_held(alt_dir: &Path, lock: &File) -> Result<(), OdbError> {
    if let Some(removals) = kept(alt_dir)? {
        carry_out(alt_dir, lock, &removals)?;
    }
    Ok(forget(alt_dir, lock)?)
}

/// With the write lock `lock` held: makes `removals`, each on the log of
/// [`LOGS`] it is for, where that log still ends in the records it was
/// planned over. The marker's generation moves first: a handle that
/// holds records of the logs reads them all again. The records that stay
/// are written and fsynced next, while the marker still says the logs
/// are synced to their ends: an older alt that came upon them between
/// would take what lies past a lower boundary for unsynced, and cut the
/// records that are to stay with the ones that go. Then what the marker
/// says of how far the logs were synced comes down, and the cuts go from
/// the records that name content down to the content. Whichever step a
/// crash interrupts, every record that stays is in its log, and making
/// the removals again finishes them.
pub(crate) fn carry_out(
    alt_dir: &Path,
    lock: &File,
    removals: &[Removal; 3],
) -> Result<(), OdbError> {
    let paths = LOGS.map(|log| alt_dir.join(log));
    let mut pending = [false; 3];
    for log in 0..3 {
        pending[log] = removals[log].pending(&paths[log])?;
    }
    if !pending.contains(&true) {
        return Ok(());
    }
    head::forget_synced(lock)?;
    let lower = |lens: Lens| Lens {
        blobmap: lens.blobmap.min(removals[0].len),
        tier1: lens.tier1.min(removals[1].len),
        map: lens.map.min(removals[2].len),
        ..lens
    };
    // a writer or a sink that wrote before does not record afterwards
    // what the removal took
    let durable = alt_dir.join("odb.durable");
    synced::update(&durable, |marker| {
        let state = marker.recorded()?;
        Some(State {
            generation: state.generation + 1,
            ..*state
        })
    })?;
    for log in 0..3 {
        if pending[log] {
            removals[log].fill(&paths[log])?;
        }
    }
    synced::update(&durable, |marker| {
        let state = marker.recorded()?;
        Some(State {
            eof: lower(state.eof),
            whole: lower(state.whole),
            ..*state
        })
    })?;
    for log in [2, 1, 0] {
        if pending[log] {
            removals[log].cut(&paths[log])?;
        }
    }
    Ok(())
}

/// `error` as a store under `alt_dir` reports it: one met while a removal
/// is waiting to be finished says so.
pub(crate) fn told(alt_dir: &Path, error: OdbError) -> OdbError {
    match alt_dir.join(NAME).exists() {
        true => OdbError::Interrupted(Box::new(error)),
        false => error,
    }
}

/// Takes the write lock of the store under `alt_dir` and finishes a
/// removal that was interrupted there ([`finish_held`]).
pub(crate) fn finish(alt_dir: &Path) -> Result<(), OdbError> {
    let lock = OpenOptions::new()
        .write(true)
        .open(alt_dir.join("odb.lock"))?;
    hold_exclusive(&lock)?;
    // under the lock: another run may have finished it
    let made = finish_held(alt_dir, &lock);
    release_exclusive(&lock);
    made
}

impl NativeOdb {
    /// Finishes a removal of lost records ([`Self::drop_lost`]) that was
    /// interrupted, before the database under `alt_dir` is opened: an
    /// open may not get past a record the interruption left half
    /// written. Says whether there was one.
    pub fn finish_removal(alt_dir: &Path) -> Result<bool, OdbError> {
        if kept(alt_dir)?.is_none() {
            return Ok(false);
        }
        finish(alt_dir).map(|()| true)
    }

    /// Whether a removal of lost records was interrupted and is still to
    /// be finished ([`Self::finish_removal`]).
    pub fn removal_interrupted(alt_dir: &Path) -> bool {
        alt_dir.join(NAME).exists()
    }
}

#[cfg(test)]
mod tests {
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

    use super::*;
    use crate::Lost;

    #[test]
    fn removals_read_back_as_they_were_kept_and_not_at_all_once_a_bit_is_off() {
        let removal = |n: u64, moves: Vec<(u64, Vec<u8>)>| Removal {
            was: 5 + 80 * n,
            len: 5 + 80 * (n - 1),
            end: [n as u8; 32],
            moves,
        };
        let removals = [
            removal(4, vec![(5, vec![1; 80]), (85, vec![2; 80])]),
            removal(1, Vec::new()),
            removal(9, vec![(87, vec![3; 82])]),
        ];
        let bytes = encode(&removals);
        assert_eq!(decode(&bytes), Some(removals));
        for bit in 0..bytes.len() * 8 {
            let mut off = bytes.clone();
            off[bit / 8] ^= 1 << (bit % 8);
            assert_eq!(decode(&off), None, "bit {bit}");
        }
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
    }

    fn object(name: &str, n: u32) -> (ObjectId, Vec<u8>) {
        let data = format!("{name} {n} {}", "y".repeat(50)).into_bytes();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
        (oid, data)
    }

    fn put(dir: &Path, name: &str, count: u32) {
        let mut odb = NativeOdb::open(dir).unwrap();
        for n in 0..count {
            let (oid, data) = object(name, n);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
        odb.flush().unwrap();
    }

    /// A store that holds twenty objects, the records of twenty more
    /// whose content a power cut took, and three hundred stored and
    /// synced after them by an alt that took the records at their word:
    /// an index run is over all of them.
    fn store(dir: &Path) -> Lost {
        put(dir, "kept", 20);
        let pack = dir.join("store/pack-00000001.altpack");
        let kept = fs::metadata(&pack).unwrap().len();
        put(dir, "lost", 20);
        head::forget_synced(&lock(dir)).unwrap();
        let cut = OpenOptions::new().write(true).open(&pack).unwrap();
        cut.set_len(kept).unwrap();
        synced::update(&dir.join("odb.durable"), |marker| {
            let state = marker.recorded()?;
            let lower = |lens: Lens| Lens { pack: kept, ..lens };
            Some(State {
                eof: lower(state.eof),
                whole: lower(state.whole),
                ..*state
            })
        })
        .unwrap();
        put(dir, "after", 300);
        assert!(fs::read_dir(dir.join("map.alt.idx")).unwrap().count() > 0);
        let lost = NativeOdb::open(dir).unwrap().lost().unwrap();
        assert_eq!(lost.objects.len(), 20);
        assert!(!lost.removals[2].moves.is_empty());
        lost
    }

    fn lock(dir: &Path) -> File {
        let mut lock = OpenOptions::new();
        let lock = lock.read(true).write(true).open(dir.join("odb.lock"));
        lock.unwrap()
    }

    /// Whether the lock file notes a removal as under way.
    fn is_noted(dir: &Path) -> bool {
        let mut note = [0u8; NOTED.len()];
        let read = lock(dir).read_at(&mut note, NOTED_AT as u64).unwrap_or(0);
        notes(note.get(..read)).removal
    }

    /// An alt that knows of no removal stores objects.
    fn older_alt_puts(dir: &Path, name: &str) {
        let was = is_noted(dir);
        lock(dir).write_all_at(&[0; 8], NOTED_AT as u64).unwrap();
        put(dir, name, 20);
        if was {
            lock(dir).write_all_at(&NOTED, NOTED_AT as u64).unwrap();
        }
    }

    /// No removal is under way, the store holds every object but the
    /// lost ones, takes those again, and passes the full check. What was
    /// found stored at generation `was` is to be looked for again.
    fn sound(dir: &Path, names: &[(&str, u32)], was: Option<u64>) {
        assert!(!NativeOdb::removal_interrupted(dir) && !is_noted(dir));
        let generation = NativeOdb::deferred(dir).generation();
        assert!(was.is_some() && generation.is_some() && generation != was);
        let mut odb = NativeOdb::open(dir).unwrap();
        assert!(odb.lost().unwrap().is_empty());
        assert!(!odb.contains(&object("lost", 3).0).unwrap());
        drop(odb);
        put(dir, "lost", 20);
        let odb = NativeOdb::open(dir).unwrap();
        for (name, count) in names.iter().chain(&[("lost", 20)]) {
            for n in 0..*count {
                let (oid, data) = object(name, n);
                assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "{name} {n}");
            }
        }
        odb.check().unwrap();
    }

    /// The states an interruption leaves a removal in, by what was done
    /// of it.
    const STEPS: [&str; 6] = [
        "kept and nothing written",
        "the runs gone and a record half written",
        "every record written and no log cut",
        "the marker down and one log cut",
        "all of it made and not forgotten",
        "all of it made and not forgotten, and the store written to since",
    ];

    /// What a removal does before it lowers the marker: the generation
    /// moves, then the records that stay are written over the ones
    /// that go.
    fn filled(dir: &Path, lost: &Lost) {
        synced::update(&dir.join("odb.durable"), |marker| {
            let state = marker.recorded()?;
            Some(State {
                generation: state.generation + 1,
                ..*state
            })
        })
        .unwrap();
        for (removal, log) in lost.removals.iter().zip(LOGS) {
            removal.fill(&dir.join(log)).unwrap();
        }
    }

    fn interrupted(dir: &Path, lost: &Lost, step: usize) {
        let paths = LOGS.map(|log| dir.join(log));
        save(dir, &lock(dir), &lost.removals).unwrap();
        if step == 1 {
            let (at, record) = &lost.removals[2].moves[0];
            alt_store::runs::remove_past(&dir.join("map.alt.idx"), *at).unwrap();
            let map = OpenOptions::new().write(true).open(&paths[2]).unwrap();
            map.write_all_at(&record[..40], *at).unwrap();
        }
        if (2..4).contains(&step) {
            filled(dir, lost);
        }
        if step == 3 {
            synced::update(&dir.join("odb.durable"), |marker| {
                let state = marker.recorded()?;
                let lower = |lens: Lens| Lens {
                    map: lost.removals[2].len,
                    ..lens
                };
                Some(State {
                    eof: lower(state.eof),
                    whole: lower(state.whole),
                    ..*state
                })
            })
            .unwrap();
            lost.removals[2].cut(&paths[2]).unwrap();
        }
        if step >= 4 {
            carry_out(dir, &lock(dir), &lost.removals).unwrap();
        }
        if step == 5 {
            older_alt_puts(dir, "later");
        }
    }

    #[test]
    fn a_writer_finishes_a_removal_interrupted_at_any_step_before_it_appends() {
        for (step, what) in STEPS.iter().enumerate() {
            // finished by the command made for it, by a handle that opens
            // the store to write, and by one that had it open before
            for by in ["fsck", "opened", "held"] {
                let tmp = alt_os::temp::tempdir().unwrap();
                let dir = tmp.path();
                let lost = store(dir);
                let was = NativeOdb::deferred(dir).generation();
                assert!(!NativeOdb::finish_removal(dir).unwrap());
                let mut held = NativeOdb::open(dir).unwrap();
                interrupted(dir, &lost, step);
                assert!(
                    NativeOdb::removal_interrupted(dir) && is_noted(dir),
                    "{what}"
                );
                // a handle that only reads changes nothing: it reads on, or
                // says what was interrupted where a record does not read
                let read = NativeOdb::reading(dir).get(&object("kept", 3).0);
                match step {
                    1 => assert!(matches!(read, Err(OdbError::Interrupted(_))), "{read:?}"),
                    _ => assert!(read.unwrap().is_some(), "{what}"),
                }
                assert!(
                    NativeOdb::removal_interrupted(dir) && is_noted(dir),
                    "{what}"
                );
                let len = fs::metadata(dir.join("map.alt")).unwrap().len();
                match by {
                    "fsck" => assert!(NativeOdb::finish_removal(dir).unwrap(), "{what}"),
                    "opened" => put(dir, "more", 20),
                    _ => {
                        for n in 0..20 {
                            let (oid, data) = object("more", n);
                            held.put(oid, ObjectKind::Blob, &data).unwrap();
                        }
                        held.flush().unwrap();
                    }
                }
                drop(held);
                let mut names = vec![("kept", 20), ("after", 300)];
                if step == 5 {
                    // nothing of what was written since is cut
                    let now = fs::metadata(dir.join("map.alt")).unwrap().len();
                    assert!(now >= len, "{what}");
                    names.push(("later", 20));
                }
                if by != "fsck" {
                    names.push(("more", 20));
                }
                sound(dir, &names, was);
            }
        }
    }

    #[test]
    fn a_note_of_a_removal_without_removals_kept_is_taken_away_by_the_next_writer() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let lost = store(dir);
        // the power went between the note and the file
        lock(dir).write_all_at(&NOTED, NOTED_AT as u64).unwrap();
        put(dir, "more", 20);
        assert!(!is_noted(dir) && !NativeOdb::removal_interrupted(dir));
        assert_eq!(
            NativeOdb::open(dir).unwrap().lost().unwrap().objects,
            lost.objects
        );
    }

    #[test]
    fn records_written_over_before_an_older_alt_appended_are_not_looked_up_through_a_run() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let lost = store(dir);
        let was = NativeOdb::deferred(dir).generation();
        save(dir, &lock(dir), &lost.removals).unwrap();
        filled(dir, &lost);
        // the logs no longer end in what the removal was planned over
        older_alt_puts(dir, "later");
        let odb = NativeOdb::reading(dir);
        for n in 0..20 {
            odb.contains(&object("lost", n).0).unwrap();
        }
        for (name, count) in [("kept", 20), ("after", 300), ("later", 20)] {
            for n in 0..count {
                let (oid, data) = object(name, n);
                assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "{name} {n}");
            }
        }
        drop(odb);
        // the next writer finds nothing to finish on logs that grew, and
        // what is left of the lost records is found and taken out anew
        put(dir, "more", 20);
        assert!(!is_noted(dir) && !NativeOdb::removal_interrupted(dir));
        let mut odb = NativeOdb::open(dir).unwrap();
        let left = odb.lost().unwrap();
        assert!(left.is_empty() || odb.drop_lost(&left).unwrap());
        drop(odb);
        let names = [("kept", 20), ("after", 300), ("later", 20), ("more", 20)];
        sound(dir, &names, was);
    }
}

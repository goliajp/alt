//! The record of a removal of lost records that is under way, and
//! carrying one out.
//!
//! Records are taken out of the blob map, the Tier 1 map and `map.alt` by
//! writing over them in place ([`Removal`]). A power cut can leave such a
//! write half made, and a record that does not read where records were
//! synced stops every command. So what is about to be written is first
//! kept in `odb.repair`, fsynced, and a removal that was interrupted is
//! made again from there before another is looked for.
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

use crate::head::{self, check};
use crate::lock::{hold_exclusive, release_exclusive};
use crate::synced::{self, Lens, State};
use crate::{NativeOdb, OdbError};

const NAME: &str = "odb.repair";
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

/// Keeps `removals` on disk, fsynced, before the first of them is made.
pub(crate) fn save(alt_dir: &Path, removals: &[Removal; 3]) -> std::io::Result<()> {
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

/// Forgets the removals kept by [`save`]: they are made.
pub(crate) fn forget(alt_dir: &Path) -> std::io::Result<()> {
    fs::remove_file(alt_dir.join(NAME))?;
    File::open(alt_dir)?.sync_all()
}

/// With the write lock `lock` held: makes `removals`, each on the log of
/// [`LOGS`] it is for, where that log still ends in the records it was
/// planned over. The records that stay are written and fsynced first;
/// what the marker and the lock file say of how far the logs were synced
/// comes down next; the cuts go from the records that name content down
/// to the content. Whichever step a crash interrupts, every record that
/// stays is in its log, and making the removals again finishes them.
pub(crate) fn carry_out(
    alt_dir: &Path,
    lock: &File,
    removals: &[Removal; 3],
) -> Result<(), OdbError> {
    let paths = LOGS.map(|log| alt_dir.join(log));
    let mut pending = [false; 3];
    for log in 0..3 {
        pending[log] = removals[log].pending(&paths[log])?;
        if pending[log] {
            removals[log].fill(&paths[log])?;
        }
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
    // the generation moves: a writer or a sink that wrote before the cut
    // does not record afterwards what the cut took
    synced::update(&alt_dir.join("odb.durable"), |marker| {
        let state = marker.recorded()?;
        Some(State {
            generation: state.generation + 1,
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

impl NativeOdb {
    /// Finishes a removal of lost records ([`Self::drop_lost`]) that was
    /// interrupted, before the database under `alt_dir` is opened: an
    /// open may not get past a record the interruption left half
    /// written. Says whether there was one.
    pub fn finish_removal(alt_dir: &Path) -> Result<bool, OdbError> {
        if kept(alt_dir)?.is_none() {
            return Ok(false);
        }
        let lock = OpenOptions::new()
            .write(true)
            .open(alt_dir.join("odb.lock"))?;
        hold_exclusive(&lock)?;
        // read again under the lock: another run may have finished it
        let made = kept(alt_dir).and_then(|removals| match removals {
            Some(removals) => {
                carry_out(alt_dir, &lock, &removals)?;
                Ok(forget(alt_dir)?)
            }
            None => Ok(()),
        });
        release_exclusive(&lock);
        made.map(|()| true)
    }

    /// Whether a removal of lost records was interrupted and is still to
    /// be finished ([`Self::finish_removal`]).
    pub fn removal_interrupted(alt_dir: &Path) -> bool {
        alt_dir.join(NAME).exists()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::FileExt;

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

    fn put(dir: &Path, name: &str) {
        let mut odb = NativeOdb::open(dir).unwrap();
        for n in 0..20 {
            let (oid, data) = object(name, n);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
        odb.flush().unwrap();
    }

    /// A store that holds twenty objects, the record of one more whose
    /// content a power cut took, and twenty stored and synced after it
    /// by an alt that took the record at its word.
    fn store(dir: &Path) -> Lost {
        put(dir, "kept");
        let pack = dir.join("store/pack-00000001.altpack");
        let kept = fs::metadata(&pack).unwrap().len();
        put(dir, "lost");
        let lock = OpenOptions::new().write(true).open(dir.join("odb.lock"));
        head::forget_synced(&lock.unwrap()).unwrap();
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
        put(dir, "after");
        let lost = NativeOdb::open(dir).unwrap().lost().unwrap();
        assert_eq!(lost.objects.len(), 20);
        assert!(!lost.removals[2].moves.is_empty());
        lost
    }

    /// The store holds every object but the lost ones, takes those
    /// again, and passes the full check.
    fn sound(dir: &Path, names: &[&str], was: Option<u64>) {
        assert!(!NativeOdb::removal_interrupted(dir));
        // what was found stored before the removal is to be looked for again
        let generation = NativeOdb::deferred(dir).generation();
        assert!(was.is_some() && generation.is_some() && generation != was);
        let mut odb = NativeOdb::open(dir).unwrap();
        assert!(odb.lost().unwrap().is_empty());
        assert!(!odb.contains(&object("lost", 3).0).unwrap());
        drop(odb);
        put(dir, "lost");
        let odb = NativeOdb::open(dir).unwrap();
        for name in names.iter().chain(&["lost"]) {
            for n in 0..20 {
                let (oid, data) = object(name, n);
                assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "{name} {n}");
            }
        }
        odb.check().unwrap();
    }

    #[test]
    fn a_removal_interrupted_at_any_step_is_finished_from_what_was_kept_of_it() {
        let lock = |dir: &Path| OpenOptions::new().write(true).open(dir.join("odb.lock"));
        // before anything was written to a log
        let tmp = alt_os::temp::tempdir().unwrap();
        let lost = store(tmp.path());
        let was = NativeOdb::deferred(tmp.path()).generation();
        assert!(!NativeOdb::finish_removal(tmp.path()).unwrap());
        save(tmp.path(), &lost.removals).unwrap();
        assert!(NativeOdb::removal_interrupted(tmp.path()));
        assert!(NativeOdb::finish_removal(tmp.path()).unwrap());
        sound(tmp.path(), &["kept", "after"], was);

        // a record half written over another: nothing reads there
        let tmp = alt_os::temp::tempdir().unwrap();
        let lost = store(tmp.path());
        let was = NativeOdb::deferred(tmp.path()).generation();
        save(tmp.path(), &lost.removals).unwrap();
        let (at, record) = &lost.removals[2].moves[0];
        let map = OpenOptions::new()
            .write(true)
            .open(tmp.path().join("map.alt"));
        map.unwrap().write_all_at(&record[..40], *at).unwrap();
        let (oid, data) = object("more", 0);
        let refused =
            NativeOdb::open(tmp.path()).and_then(|mut odb| odb.put(oid, ObjectKind::Blob, &data));
        assert!(
            matches!(refused, Err(OdbError::Interrupted(_))),
            "{refused:?}"
        );
        assert!(NativeOdb::finish_removal(tmp.path()).unwrap());
        sound(tmp.path(), &["kept", "after"], was);

        // every record written, no log cut
        let tmp = alt_os::temp::tempdir().unwrap();
        let lost = store(tmp.path());
        let was = NativeOdb::deferred(tmp.path()).generation();
        save(tmp.path(), &lost.removals).unwrap();
        for (removal, log) in lost.removals.iter().zip(LOGS) {
            removal.fill(&tmp.path().join(log)).unwrap();
        }
        assert!(NativeOdb::finish_removal(tmp.path()).unwrap());
        sound(tmp.path(), &["kept", "after"], was);

        // all of it made and not yet forgotten, and the store written
        // to since: nothing of that is cut
        let tmp = alt_os::temp::tempdir().unwrap();
        let lost = store(tmp.path());
        let was = NativeOdb::deferred(tmp.path()).generation();
        save(tmp.path(), &lost.removals).unwrap();
        carry_out(tmp.path(), &lock(tmp.path()).unwrap(), &lost.removals).unwrap();
        put(tmp.path(), "later");
        let len = fs::metadata(tmp.path().join("map.alt")).unwrap().len();
        assert!(NativeOdb::finish_removal(tmp.path()).unwrap());
        assert_eq!(fs::metadata(tmp.path().join("map.alt")).unwrap().len(), len);
        sound(tmp.path(), &["kept", "after", "later"], was);
    }
}

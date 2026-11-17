//! What the holder of the write lock does about the ends of the append
//! files before it appends.
//!
//! Content is written before the records that name it, and fsynced in that
//! order: pack, blob map, Tier 1 map, `map.alt`. Before the fsync the
//! kernel writes pages out in whatever order it likes, so a power cut can
//! leave a record whole on disk while the content it names never arrived,
//! or arrived in part. Such a record makes the store say it holds an
//! object that does not read, and a later `put` of the object is skipped.
//!
//! So a writer reads the four files to their ends without cutting
//! anything, and looks at the records it has no word for: those past the
//! boundaries the synced marker records, and every record when content is
//! about to be cut or when an older alt, which cuts content first, may
//! have cut it back. A record past its boundary that names
//! content which is not stored was never synced: its log is cut from there
//! on. Any other is damage: it is reported and no file changes. The cuts
//! go the way a rewind's do, from the records that name content down to
//! the content, the marker brought down first.

use std::collections::HashSet;

use alt_store::StoreError;

use super::Odb;
use crate::synced::{HEADER, Lens, Marker, State};
use crate::{BlobId, OdbError, tier1};

/// The logs whose records name content, in the order they are written.
const NAMING: [&str; 3] = ["the blob map", "the tier1 map", "map.alt"];

/// From which byte on the records of each naming log are not known to have
/// been synced, by what the marker says; `u64::MAX` for a log it says
/// nothing of. `None` when it tells of no log.
fn unsynced_from(marker: &Marker) -> Option<[u64; 3]> {
    match marker {
        Marker::Recorded(state) if marker.trusted() => {
            Some([state.whole.blobmap, state.whole.tier1, state.whole.map])
        }
        // file lengths at the last fsync of an older alt, which records
        // none for the Tier 1 map
        Marker::Legacy([_, blobmap, map]) if *blobmap >= HEADER && *map >= HEADER => {
            Some([*blobmap, u64::MAX, *map])
        }
        _ => None,
    }
}

/// What records synced whole are missing from a store that holds `holds`.
pub(crate) fn lost(marker: &Marker, holds: &Lens) -> Result<(), OdbError> {
    let Some(State { whole, .. }) = marker.recorded() else {
        return Ok(());
    };
    // of a pack older than the one recorded nothing is known whole
    let pack = match holds.seq.cmp(&whole.seq) {
        std::cmp::Ordering::Less => 0,
        std::cmp::Ordering::Equal => holds.pack,
        std::cmp::Ordering::Greater => u64::MAX,
    };
    let files = [
        ("the active pack", whole.pack, pack),
        (NAMING[0], whole.blobmap, holds.blobmap),
        (NAMING[1], whole.tier1, holds.tier1),
        (NAMING[2], whole.map, holds.map),
    ];
    match files.into_iter().find(|(_, synced, holds)| holds < synced) {
        Some((file, synced, holds)) => Err(OdbError::Lost {
            file,
            synced,
            holds,
        }),
        None => Ok(()),
    }
}

impl Odb {
    /// Under the write lock: reads all four files to their ends. What a
    /// dead writer left unfinished is passed over, for [`Self::settle`].
    pub(super) fn catch_up(&mut self) -> Result<(), OdbError> {
        self.blobs.sync_from_disk()?;
        self.blobs.open_map()?;
        match self.tier1.get_mut() {
            Some(tier1) => tier1.sync_from_disk()?,
            None => drop(self.tier1()?),
        }
        self.map.sync_from_disk()
    }

    /// The record boundaries of the files as this handle holds them.
    pub(crate) fn holds(&self) -> Result<Lens, OdbError> {
        let (seq, pack, blobmap) = self.blobs.checkpoint()?.lens();
        Ok(Lens {
            seq,
            pack,
            blobmap,
            tier1: self.tier1()?.appended_len(),
            map: self.map.appended_len(),
        })
    }

    /// Remembers, as the write lock is given up, that the records this
    /// batch found and wrote name content that is stored.
    pub(super) fn note_checked(&mut self) {
        let holds = self.holds();
        if let (Some((_, upto)), Ok(holds)) = (&mut self.checked, holds) {
            *upto = [holds.blobmap, holds.tier1, holds.map];
        }
    }

    /// With the write lock held and the files read to their ends: cuts
    /// off what never finished, and with it the records past the synced
    /// boundaries that name content the store does not hold. Fails, with
    /// every file as it was, when a record that was synced names such
    /// content, or when records that were synced are gone.
    pub(super) fn settle(&mut self, marker: &Marker) -> Result<(), OdbError> {
        let (pack_torn, blobmap_torn) = self.blobs.unfinished()?;
        let torn = [
            blobmap_torn,
            self.tier1()?.unfinished(),
            self.map.unfinished(),
        ];
        let holds = self.holds()?;
        let ends = [holds.blobmap, holds.tier1, holds.map];
        let bounds = unsynced_from(marker);
        // an older alt cuts the pack first when it rolls a batch back, and
        // its marker then records a pack longer than there is
        let cut_before = matches!(marker, Marker::Legacy([pack, ..]) if *pack > holds.pack);
        let from = if pack_torn || torn[0] || torn[1] || cut_before {
            Some([HEADER; 3])
        } else {
            let seen = self
                .checked
                .filter(|(generation, _)| *generation == marker.generation());
            bounds.map(|bounds| {
                [0, 1, 2].map(|log| match bounds[log] {
                    // no word of this log: all of it is looked at
                    u64::MAX => HEADER,
                    synced => synced.max(seen.map_or(0, |(_, upto)| upto[log])),
                })
            })
        };
        let cuts = match from.map(|from| [0, 1, 2].map(|log| from[log].min(ends[log]))) {
            Some(from) if from != ends => self.dangling(from, ends)?,
            _ => ends,
        };
        for log in 0..3 {
            let unsynced = bounds.is_some_and(|bounds| cuts[log] >= bounds[log]);
            if cuts[log] < ends[log] && !unsynced {
                return Err(OdbError::Dangling {
                    file: NAMING[log],
                    at: cuts[log],
                });
            }
        }
        let after = Lens {
            blobmap: cuts[0],
            tier1: cuts[1],
            map: cuts[2],
            ..holds
        };
        lost(marker, &after)?;
        let moved = cuts != ends;
        if moved || pack_torn || torn.contains(&true) {
            self.cut_back(after, pack_torn, moved)?;
        }
        self.checked = Some((marker.generation() + u64::from(moved), cuts));
        Ok(())
    }

    /// Where, in the blob map, the Tier 1 map and `map.alt`, the first
    /// record at or past `from` begins that names content which is not
    /// stored, or which a record cut before it names; `ends` where there
    /// is none.
    fn dangling(&self, from: [u64; 3], ends: [u64; 3]) -> Result<[u64; 3], OdbError> {
        let mut cuts = ends;
        let mut cut_blobs = HashSet::new();
        self.blobs.map_records_from(from[0], |at, blob, root| {
            if cuts[0] < ends[0] || !self.blobs.tree_stored(root)? {
                cuts[0] = cuts[0].min(at);
                cut_blobs.insert(blob);
            }
            Ok(())
        })?;
        let stored = |id: BlobId| -> Result<bool, StoreError> {
            if self.blobs.chunk_stored(id)? {
                return Ok(true);
            }
            Ok(!cut_blobs.contains(&id) && self.blobs.mapped(id)?)
        };
        let tier1 = self.tier1()?;
        let mut cut_parts = HashSet::new();
        tier1.each_from(from[1], |at, blob, record| {
            if cuts[1] < ends[1] || !self.parts_stored(record, &stored)? {
                cuts[1] = cuts[1].min(at);
                cut_parts.insert(blob);
            }
            Ok::<(), OdbError>(())
        })?;
        self.map.each_from(from[2], |at, entry| {
            if cuts[2] < ends[2] || stored(entry.alt)? {
                return Ok(());
            }
            if cut_parts.contains(&entry.alt) || !tier1.contains(entry.alt)? {
                cuts[2] = at;
            }
            Ok(())
        })?;
        Ok(cuts)
    }

    /// Whether the recipe record `record` and every part it lists are
    /// stored.
    fn parts_stored(
        &self,
        record: BlobId,
        stored: &impl Fn(BlobId) -> Result<bool, StoreError>,
    ) -> Result<bool, OdbError> {
        if !stored(record)? {
            return Ok(false);
        }
        let bytes = match self.blobs.get(record) {
            Err(StoreError::NotFound(_) | StoreError::BlobNotFound(_)) => return Ok(false),
            read => read?,
        };
        let (_, _, parts) =
            tier1::decode_record(&bytes).map_err(|e| OdbError::Tier1Record(record, e))?;
        for part in parts {
            if !stored(part)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// For a reader, which changes nothing: from which byte on the blob
    /// map, the Tier 1 map and `map.alt` hold records that were never
    /// synced and name content which is not stored, the records a writer
    /// cuts off; where each ends when it holds none.
    pub(crate) fn unconfirmed(&self) -> Result<[u64; 3], OdbError> {
        let holds = self.holds()?;
        let ends = [holds.blobmap, holds.tier1, holds.map];
        let Some(bounds) = unsynced_from(&Marker::read(&self.durable_path)) else {
            return Ok(ends);
        };
        // a log the marker has no word of is not looked at
        let from = [0, 1, 2].map(|log| bounds[log].min(ends[log]));
        if from == ends {
            return Ok(ends);
        }
        self.dangling(from, ends)
    }

    /// Runs `check` with the write lock held, the files read to their ends
    /// and settled: nothing is appended to the store meanwhile, and a
    /// record that is there names content that is stored. A batch this
    /// handle has open stays open.
    pub fn settled<T>(&mut self, check: impl FnOnce(&Self) -> T) -> Result<T, OdbError> {
        let held = self.held;
        self.acquire()?;
        let out = check(self);
        if !held {
            self.release();
        }
        Ok(out)
    }
}

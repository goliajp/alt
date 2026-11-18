//! Opening the object database and, on first use, its Tier 1 map.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alt_store::{Basis, BlobOptions, BlobStore, StoreError, Tail};

use super::Odb;
use crate::head::Head;
use crate::lock::{lock_shared, release_exclusive, try_hold_exclusive, unlock};
use crate::map::ObjectMap;
use crate::synced::{self, Deferred, Lens, Log, Marker, State};
use crate::tier1::Tier1Map;
use crate::{OdbError, default_registry};

/// How an [`Odb`] is opened.
pub(crate) struct Mode {
    /// leave the blob map and the Tier 1 map for the first call that needs
    /// them (where damage to them is then reported)
    pub late: bool,
    /// read and nothing else: no file is created or repaired
    pub read_only: bool,
    /// where enough synced records have no index run, the open builds one;
    /// records that were never synced and name content which is not
    /// stored are passed over
    pub index: bool,
    pub deferred: Deferred,
}

/// What a handle on the database is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// reads and writes
    Write,
    /// a command that only reads: no file is created or repaired, and of
    /// what is there only index runs are written
    Read,
    /// a check that leaves every file as it finds it
    Check,
}

impl Access {
    /// How a handle for this opens the database.
    pub fn mode(self, late: bool, deferred: Deferred) -> Mode {
        // a process that reads past damage changes no file
        let check = self == Access::Check || alt_store::reading_past_damage();
        Mode {
            late,
            read_only: check || self == Access::Read,
            index: !check,
            deferred,
        }
    }
}

fn open_lock(alt_dir: &Path, read_only: bool) -> std::io::Result<File> {
    let lock_path = alt_dir.join("odb.lock");
    if read_only {
        return File::open(lock_path);
    }
    let open = || {
        OpenOptions::new()
            .read(true)
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
    };
    match open() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(alt_dir)?;
            open()
        }
        other => other,
    }
}

/// What `log` not reading to its end means, from what the marker says of
/// how far it was synced; a cut of it brings the marker down first. A pack
/// no longer than `synced` says is not looked up in the marker.
fn tail(durable: &Path, read_only: bool, log: Log, synced: Option<Lens>) -> Tail {
    let (read, lowered) = (durable.to_owned(), durable.to_owned());
    let tail = Tail::judged_by(move |seq, len| {
        let bounded = || {
            File::open(read.with_file_name("odb.lock")).is_ok_and(|lock| Head::read(&lock).bounded)
        };
        Marker::read(&read).basis(log, seq, len, bounded)
    })
    .before_cut(move |seq, at| synced::lower_eof(&lowered, log, seq, at))
    // what a dead writer left unfinished is cut by `Odb::settle`
    .deferring();
    let tail = match (log, synced) {
        (Log::Pack, Some(synced)) => tail.synced_to(synced.seq, synced.pack),
        _ => tail,
    };
    if read_only { tail.read_only() } else { tail }
}

/// The append files as opened: the last value says whether `map.alt` was
/// created.
type Files = (BlobStore, OnceLock<Tier1Map>, ObjectMap, bool);

fn open_files(
    alt_dir: &Path,
    opts: BlobOptions,
    head: &Head,
    late: bool,
    tail: impl Fn(Log) -> Tail,
    confirmed: Option<Lens>,
) -> Result<Files, OdbError> {
    let tails = [tail(Log::Pack), tail(Log::Blobmap)];
    let mut blobs =
        BlobStore::open_deferred(alt_dir.join("store"), opts, Some(&head.store), tails)?;
    if let Some(noted) = confirmed {
        super::confirmed::confirm_blob_map(&mut blobs, noted, &alt_dir.join("odb.durable"));
    }
    let tier1 = if late {
        OnceLock::new()
    } else {
        blobs.open_map()?;
        let tail = tail(Log::Tier1);
        OnceLock::from(Tier1Map::open(alt_dir, head.tier1.as_deref(), tail)?)
    };
    let tail = tail(Log::Map);
    let (map, created) =
        ObjectMap::open_with_runs(&alt_dir.join("map.alt"), head.map.as_deref(), tail)?;
    Ok((blobs, tier1, map, created))
}

/// A store whose logs have outgrown their index runs — an older alt wrote
/// it, the runs were removed, or a writer could not write them — gets runs
/// from whoever opens it next, unless a writer is at work: over the records
/// the marker says were synced and no others, so that no run is ever over
/// records a writer may cut. The opener takes the write lock for it, reads
/// the files again under it (`read`) and gives those back; `None` when a
/// writer has the lock. That is all the lock is taken for: nothing is cut,
/// and neither the marker nor the lock file is written.
fn index_synced(
    lock: &File,
    durable: &Path,
    read: impl FnOnce() -> Result<Files, OdbError>,
) -> Result<Option<Files>, OdbError> {
    if !try_hold_exclusive(lock)? {
        return Ok(None);
    }
    let indexed = read().and_then(|mut files| {
        let marker = Marker::read(durable);
        // a marker that is gone or does not read bounds nothing
        let synced = |log, seq, len| match marker.basis(log, seq, len, || true) {
            Basis::Boundary(upto) => Some(upto),
            _ => None,
        };
        let upto = |log| synced(log, 0, 0).unwrap_or(0);
        let (blobs, tier1, map, _) = &mut files;
        let stores = blobs.index_synced(|seq, len| synced(Log::Pack, seq, len), upto(Log::Blobmap));
        let stores = stores.and_then(|()| match tier1.get_mut() {
            Some(tier1) => tier1.index_tail(upto(Log::Tier1)),
            None => Ok(()),
        });
        let indexed = stores
            .map_err(OdbError::from)
            .and_then(|()| map.index_tail(upto(Log::Map)));
        match indexed {
            // the runs are a cache: where they cannot be written, reads go on
            Ok(()) | Err(OdbError::Store(StoreError::IndexWrite(_))) => Ok(files),
            Err(damage) => Err(damage),
        }
    });
    release_exclusive(lock);
    indexed.map(Some)
}

impl Odb {
    /// Opens (or creates) the database under the `.alt` root directory.
    /// No log, marker or lock file that is there is changed: what a dead
    /// writer left unfinished is passed over, and a store without record
    /// boundaries or a head record stays without. The first write gives
    /// it those, under the write lock.
    pub fn open_with(
        alt_dir: impl Into<PathBuf>,
        opts: BlobOptions,
        mode: Mode,
    ) -> Result<Self, OdbError> {
        let alt_dir = alt_dir.into();
        let lock = open_lock(&alt_dir, mode.read_only)?;
        let durable_path = alt_dir.join("odb.durable");
        // Open the append files (active pack, blobmap, map.alt) under a
        // shared lock, so the open never overlaps another process's
        // exclusive append: a record that does not read is then one no
        // writer is still writing.
        lock_shared(&lock)?;
        let (head, synced) = Head::read_with_synced(&lock);
        // an alt that keeps no record boundaries may have cut the files
        // back since, and does not take the note back
        let synced = synced.filter(|_| head.bounded);
        let tail = |log: Log| tail(&durable_path, mode.read_only, log, synced);
        // without a note every file is longer than what is known synced
        let confirmed = mode.index.then(|| synced.unwrap_or(Lens::NONE));
        let opened = open_files(&alt_dir, opts, &head, mode.late, tail, confirmed);
        let _ = unlock(&lock);
        let mut files = opened?;
        if mode.index && (files.0.index_pending() || files.2.index_pending()) {
            let read = || open_files(&alt_dir, opts, &head, mode.late, tail, confirmed);
            if let Some(indexed) = index_synced(&lock, &durable_path, read)? {
                files = indexed;
            }
        }
        let (mut blobs, tier1, map, created) = files;
        blobs.guard_opens(alt_dir.join("odb.lock"));
        if created {
            // a new store: each file is synced as far as its header
            synced::update(&durable_path, |marker| {
                (*marker == Marker::Absent).then_some(State::NEW)
            })?;
        }
        let mut this = Self {
            blobs,
            map,
            tier1,
            registry: default_registry(),
            lock,
            held: false,
            head,
            sync_lock_path: alt_dir.join("odb.sync.lock"),
            tier1_tail: tail(Log::Tier1),
            durable_path,
            defer: false,
            write_count: 0,
            checked: None,
            discarded: Vec::new(),
            deferred: mode.deferred,
            confirmed,
            alt_dir,
        };
        this.confirm()?;
        Ok(this)
    }

    /// The Tier 1 map, opened on first use. Opening must not overlap a
    /// writer's append: without the write lock in hand the open runs under
    /// the shared one, like the open of the store. With it, the open cuts
    /// off what a dead writer left unfinished.
    pub(crate) fn tier1(&self) -> Result<&Tier1Map, OdbError> {
        if let Some(tier1) = self.tier1.get() {
            return Ok(tier1);
        }
        let open = || {
            let runs = self.head.tier1.as_deref();
            let tail = match self.held {
                true => self.tier1_tail.clone().locked(),
                false => self.tier1_tail.clone(),
            };
            Tier1Map::open(&self.alt_dir, runs, tail)
        };
        let tier1 = if self.held {
            open()?
        } else {
            let mut tier1 = alt_store::with_shared_lock(&self.alt_dir.join("odb.lock"), open)?;
            self.confirm_tier1(&mut tier1)?;
            tier1
        };
        Ok(self.tier1.get_or_init(|| tier1))
    }

    pub(crate) fn tier1_mut(&mut self) -> Result<&mut Tier1Map, OdbError> {
        self.tier1()?;
        Ok(self
            .tier1
            .get_mut()
            .expect("the tier1 map was opened just above"))
    }
}

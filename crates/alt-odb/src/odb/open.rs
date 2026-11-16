//! Opening the object database and, on first use, its Tier 1 map.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alt_store::{BlobOptions, BlobStore, Tail};

use super::Odb;
use crate::head::Head;
use crate::lock::{lock_shared, try_hold_exclusive, unlock};
use crate::map::ObjectMap;
use crate::synced::{self, Deferred, Log, Marker, State};
use crate::tier1::Tier1Map;
use crate::{OdbError, default_registry};

/// How an [`Odb`] is opened.
pub(crate) struct Mode {
    /// leave the blob map and the Tier 1 map for the first call that needs
    /// them (where damage to them is then reported)
    pub late: bool,
    /// read and nothing else: no file is created, repaired or indexed
    pub read_only: bool,
    pub deferred: Deferred,
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

/// What `log` not reading to its end means, from the marker's record of
/// how far it was synced.
fn tail(durable: &Path, read_only: bool, log: Log) -> Tail {
    let durable = durable.to_owned();
    let tail = Tail::synced_to(move |seq| Marker::read(&durable).recorded()?.whole.of(log, seq));
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
) -> Result<Files, OdbError> {
    let tails = [tail(Log::Pack), tail(Log::Blobmap)];
    let blobs = BlobStore::open_deferred(alt_dir.join("store"), opts, Some(&head.store), tails)?;
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

impl Odb {
    /// Opens (or creates) the database under the `.alt` root directory.
    pub fn open_with(
        alt_dir: impl Into<PathBuf>,
        opts: BlobOptions,
        mode: Mode,
    ) -> Result<Self, OdbError> {
        let alt_dir = alt_dir.into();
        let lock = open_lock(&alt_dir, mode.read_only)?;
        let durable_path = alt_dir.join("odb.durable");
        let tail = |log: Log| tail(&durable_path, mode.read_only, log);
        // Open the append files (active pack, blobmap, map.alt — read and
        // crash-recovered here) under a shared lock, so the open never overlaps
        // another process's exclusive append. Without this, an open scanning a
        // writer's half-written record truncates it and corrupts the store.
        lock_shared(&lock)?;
        let head = Head::read(&lock);
        let opened = open_files(&alt_dir, opts, &head, mode.late, tail);
        let _ = unlock(&lock);
        let (mut blobs, tier1, map, created) = opened?;
        blobs.guard_opens(alt_dir.join("odb.lock"));
        if created {
            // a new store: each file is synced as far as its header
            synced::update(&durable_path, |marker| {
                (*marker == Marker::Absent).then_some(State::NEW)
            })?;
        }
        let mut odb = Self {
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
            deferred: mode.deferred,
            alt_dir,
        };
        if !mode.read_only {
            odb.index_stale_tails()?;
        }
        Ok(odb)
    }

    /// A store whose logs have outgrown their index runs, or that has no
    /// head record — written by a program that keeps neither, or by a
    /// writer that died before its flush — gets both from whoever opens it
    /// next, unless a writer is at work.
    fn index_stale_tails(&mut self) -> Result<(), OdbError> {
        let pending =
            self.blobs.index_pending() || self.map.index_pending() || self.head == Head::default();
        if !pending || !try_hold_exclusive(&self.lock)? {
            return Ok(());
        }
        self.held = true;
        let indexed = (|| {
            self.blobs.sync_from_disk()?;
            self.map.sync_from_disk()?;
            // every log of such a store, opened so far or not
            self.blobs.open_map()?;
            self.tier1_mut()?;
            self.index_tails()
        })();
        self.release();
        indexed
    }

    /// The Tier 1 map, opened on first use. Opening repairs a torn tail,
    /// which must not overlap a writer's append: without the write lock in
    /// hand the open runs under the shared one, like the open of the store.
    pub(crate) fn tier1(&self) -> Result<&Tier1Map, OdbError> {
        if let Some(tier1) = self.tier1.get() {
            return Ok(tier1);
        }
        let open = || {
            let runs = self.head.tier1.as_deref();
            Tier1Map::open(&self.alt_dir, runs, self.tier1_tail.clone())
        };
        let tier1 = if self.held {
            open()?
        } else {
            alt_store::with_shared_lock(&self.alt_dir.join("odb.lock"), open)?
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

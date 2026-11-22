//! Opening the object database and, on first use, its Tier 1 map.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use alt_store::{BlobOptions, BlobStore, Tail};

use super::{Odb, runs};
use crate::head::Head;
use crate::lock::{lock_shared, unlock};
use crate::map::ObjectMap;
use crate::synced::{self, Deferred, Lens, Log, Marker, State};
use crate::tier1::Tier1Map;
use crate::unsynced::{self, Left, Noted, boot_runs};
use crate::{OdbError, default_registry};
use alt_store::runs::BootRun;

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
fn tail(
    durable: &Path,
    read_only: bool,
    log: Log,
    synced: Option<Lens>,
    left: &Noted,
    boot_run: Option<BootRun>,
) -> Tail {
    let (read, lowered, left) = (durable.to_owned(), durable.to_owned(), left.clone());
    let tail = Tail::judged_by(move |seq, len| {
        let bounded = || {
            File::open(read.with_file_name("odb.lock")).is_ok_and(|lock| Head::read(&lock).bounded)
        };
        let marker = Marker::read(&read);
        // a writer that syncs takes no record of a log past the marker's
        // boundary for written as it is: one that does not read there is
        // passed over, for `Odb::settle` to cut the log at
        let as_written = matches!(log, Log::Pack) || read_only || alt_store::relaxed_durability();
        let cached = as_written && unsynced::cached(&marker, *left.lock().unwrap());
        unsynced::basis(&marker, cached, (log, seq, len), bounded)
    })
    .before_cut(move |seq, at| synced::lower_eof(&lowered, log, seq, at))
    // what a dead writer left unfinished is cut by `Odb::settle`
    .deferring()
    .with_boot_run(boot_run);
    let tail = match (log, synced) {
        (Log::Pack, Some(synced)) => tail.synced_to(synced.seq, synced.pack),
        _ => tail,
    };
    if read_only { tail.read_only() } else { tail }
}

/// The append files as opened: the last value says whether `map.alt` was
/// created.
pub(super) type Files = (BlobStore, OnceLock<Tier1Map>, ObjectMap, bool);

fn open_files(
    alt_dir: &Path,
    opts: BlobOptions,
    head: &Head,
    late: bool,
    tail: impl Fn(Log) -> Tail,
    confirmed: Option<(Lens, Option<Left>)>,
) -> Result<Files, OdbError> {
    let tails = [tail(Log::Pack), tail(Log::Blobmap)];
    let mut blobs =
        BlobStore::open_deferred(alt_dir.join("store"), opts, Some(&head.store), tails)?;
    if let Some((noted, left)) = confirmed {
        let durable = alt_dir.join("odb.durable");
        super::confirmed::confirm_blob_map(&mut blobs, noted, &durable, left);
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
        let mut noted = Head::read_with_synced(&lock);
        if noted.3.removal && !mode.read_only {
            // a removal of lost records was interrupted: a handle that
            // writes finishes it before it reads the store
            let _ = unlock(&lock);
            crate::repair::finish(&alt_dir)?;
            lock_shared(&lock)?;
            noted = Head::read_with_synced(&lock);
        }
        let (head, synced, left, notes) = noted;
        // an alt that keeps no record boundaries may have cut the files
        // back since, and does not take the note back
        let synced = synced.filter(|_| head.bounded);
        let noted: Noted = Arc::new(left.into());
        let held = || boot_runs::held_at_open(&durable_path, left, notes.boot_runs);
        // a check reads every record itself
        let boot_runs = mode.index.then(held).flatten();
        let tail = |log: Log| {
            let boot_run = boot_runs.and_then(|(runs, boot)| runs.of(log, boot));
            tail(&durable_path, mode.read_only, log, synced, &noted, boot_run)
        };
        // without a note every file is longer than what is known synced
        let confirmed = mode.index.then(|| synced.unwrap_or(Lens::NONE));
        let passing = confirmed.map(|noted| (noted, left));
        let opened = open_files(&alt_dir, opts, &head, mode.late, tail, passing);
        let _ = unlock(&lock);
        let mut files = opened?;
        if mode.index && runs::wanted(&files, &durable_path) {
            let read = || open_files(&alt_dir, opts, &head, mode.late, tail, passing);
            if let Some(indexed) = runs::index_synced(&lock, &durable_path, read)? {
                files = indexed;
            }
        }
        let tier1_tail = tail(Log::Tier1);
        let mut this = Self::holding(alt_dir, lock, head, files, tier1_tail, mode.deferred)?;
        this.opened = (opts, mode.late);
        (this.generation, this.noted) = (notes.generation, notes.generation);
        this.left = noted;
        this.confirmed = confirmed;
        this.confirm()?;
        Ok(this)
    }

    /// A handle on the files as opened; a store just created gets its
    /// marker.
    fn holding(
        alt_dir: PathBuf,
        lock: File,
        head: Head,
        files: Files,
        tier1_tail: Tail,
        deferred: Deferred,
    ) -> Result<Self, OdbError> {
        let (mut blobs, tier1, map, created) = files;
        blobs.guard_opens(alt_dir.join("odb.lock"));
        let durable_path = alt_dir.join("odb.durable");
        if created {
            // a new store: each file is synced as far as its header
            synced::update(&durable_path, |marker| {
                (*marker == Marker::Absent).then_some(State::NEW)
            })?;
        }
        Ok(Self {
            blobs,
            map,
            tier1,
            registry: default_registry(),
            lock,
            held: false,
            head,
            sync_lock_path: alt_dir.join("odb.sync.lock"),
            tier1_tail,
            durable_path,
            defer: false,
            write_count: 0,
            checked: None,
            discarded: Vec::new(),
            deferred,
            confirmed: None,
            left: Noted::default(),
            generation: None,
            noted: None,
            opened: (BlobOptions::default(), false),
            alt_dir,
        })
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

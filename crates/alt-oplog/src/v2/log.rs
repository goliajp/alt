//! The handle on a version 2 log.

use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::lock::{lock_shared, unlock};
use crate::v2::block::{FILE_HEADER_LEN, FileHeader, Layout};
use crate::v2::cuts::Cuts;
use crate::v2::mark::{Mark, header_of};
use crate::v2::open::{Loaded, Source, load};
use crate::v2::owed::Shared;
use crate::v2::state::State;
use crate::{Anchor, Discarded, Op, OpId, OpLogError, SyncNamed};

pub(crate) struct Log {
    pub(super) file: File,
    pub(super) dir: PathBuf,
    pub(super) layout: Layout,
    /// whether this build may append to the log
    pub(super) writable: bool,
    /// the boot this handle runs in, as far as it is told one
    pub(super) boot: Option<[u8; 16]>,
    /// the log through the last frame this handle read or wrote
    pub(super) state: State,
    /// the four bytes of the file before where that frame ends
    pub(super) closes: [u8; 4],
    /// set when the log was opened at an anchor that holds
    pub(super) base: Option<Anchor>,
    /// The ops of the frames from `began` on, in chain order; before
    /// them, on a log opened at an anchor that lies before `began`, the
    /// ref transactions between the two.
    pub(super) ops: Vec<Op>,
    /// where in `ops` the ones after `base` begin
    pub(super) recent_from: usize,
    pub(super) began: u64,
    /// the last ref transaction before `began`
    pub(super) began_linked: u64,
    /// the whole chain, read on first use when `ops` is not all of it
    pub(super) full: OnceLock<Vec<Op>>,
    /// the cuts the log tells of, read on first use for the log as it
    /// was then cut
    pub(super) cuts: RefCell<Cuts>,
    /// up to where this handle knows the log synced
    pub(super) synced: u64,
    /// An op whose frame starts before this needs no vouching for: it is
    /// within what is synced, or was appended or read through in the
    /// running boot.
    pub(super) judged: Cell<u64>,
    /// the length of the file when this handle last read it and `tail`
    /// said that what follows the frames held is no part of the log
    pub(super) hidden: Option<u64>,
    /// how long the file was when this handle last read it as an open does
    pub(super) seen: u64,
    /// Up to where the file held frames then that are still in the log:
    /// a frame from here on was appended in the running boot, after that
    /// read or after a cut made since.
    pub(super) left_to: u64,
    /// The latest generation this handle knew the log in the file to
    /// be of: a cut it finds the log to be of a later one by was made
    /// since, in the running boot.
    pub(super) generation_known: u32,
    /// this handle noted that ops are left to be looked at
    pub(super) left_noted: bool,
    /// where the log ended with the checkpoint of a cut when this handle
    /// fsynced it or found it synced, zero before
    pub(super) cut_on_disk: u64,
    pub(super) defer: bool,
    pub(super) write_count: u64,
    pub(super) unsynced: bool,
    /// the frames this handle wrote that no sync is recorded over, and
    /// what it left for its sink, which shares it
    pub(super) account: Shared,
    pub(super) clamped: bool,
    pub(super) passed: bool,
    pub(super) reads: u64,
    pub(super) named: Option<SyncNamed>,
    pub(super) discarded: Vec<Discarded>,
    /// how many bytes of the file this handle read, and in how many reads
    pub(super) read: Cell<(u64, u64)>,
}

impl Log {
    pub fn open_at(dir: &Path, anchor: Option<Anchor>) -> Result<Self, OpLogError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("log"))?;
        Self::opened(dir, file, anchor)
    }

    pub fn open_read_only(dir: &Path, anchor: Option<Anchor>) -> Result<Self, OpLogError> {
        Self::opened(dir, File::open(dir.join("log"))?, anchor)
    }

    fn opened(dir: &Path, file: File, anchor: Option<Anchor>) -> Result<Self, OpLogError> {
        // only the read needs the lock: writers append under the exclusive
        // one, so a frame cut short here is a crashed writer's. The header
        // is read under it too: a cut writes there
        lock_shared(&file)?;
        let (header, told) = match header_of(&file) {
            Ok(read) => read,
            Err(unread) => {
                let _ = unlock(&file);
                return Err(unread);
            }
        };
        let mut log = Self::unread(dir, file, &header);
        let read = log.read_in(false, Some(told));
        let read = read.and_then(|()| log.begin_after(anchor));
        let _ = unlock(&log.file);
        read.map(|()| log)
    }

    /// A handle on the log `file` of `dir` that has read nothing of it
    /// but its header.
    fn unread(dir: &Path, file: File, header: &FileHeader) -> Log {
        Log {
            file,
            dir: dir.to_owned(),
            layout: header.layout(),
            writable: header.writable(),
            boot: crate::v2::running_boot(),
            state: State::empty(),
            closes: [0; 4],
            base: None,
            ops: Vec::new(),
            recent_from: 0,
            began: FILE_HEADER_LEN as u64,
            began_linked: 0,
            full: OnceLock::new(),
            cuts: RefCell::default(),
            synced: 0,
            judged: Cell::new(0),
            hidden: None,
            seen: 0,
            left_to: 0,
            generation_known: 0,
            left_noted: false,
            cut_on_disk: 0,
            defer: false,
            write_count: 0,
            unsynced: false,
            account: Shared::default(),
            clamped: false,
            passed: false,
            reads: 0,
            named: None,
            discarded: Vec::new(),
            read: Cell::new((FILE_HEADER_LEN as u64, 1)),
        }
    }

    pub(super) fn source(&self) -> Source<'_> {
        Source {
            file: &self.file,
            read: &self.read,
        }
    }

    /// Reads the log as an open does, from its first frame when `all`,
    /// and holds what that read found. The caller holds a lock on it,
    /// and gives what the header tells of a cut where it has read that
    /// under the lock.
    pub(super) fn read_in(&mut self, all: bool, told: Option<Mark>) -> Result<(), OpLogError> {
        let told = match told {
            Some(told) => told,
            None => self.told()?,
        };
        let loaded = load(&self.source(), &self.beside(), &self.layout, told, all)?;
        let Loaded {
            state,
            ops,
            closes,
            began,
            began_linked,
            synced,
            judged,
            hidden,
            seen,
            of_file,
            clamped,
            passed,
        } = loaded;
        (self.state, self.ops, self.closes) = (state, ops, closes);
        (self.began, self.began_linked) = (began, began_linked);
        (self.base, self.recent_from, self.full) = (None, 0, OnceLock::new());
        self.synced = self.synced.max(synced);
        self.judged.set(judged);
        // past the frames held are frames of the log only where the log
        // ends short of them for this handle
        self.left_to = match hidden {
            Some(_) => seen,
            None => self.state.end,
        };
        // read before, it finds the log cut since: what follows that cut
        // was appended in the running boot
        let (generation, cut) = of_file;
        if self.seen != 0 && generation > self.generation_known {
            self.left_to = self.left_to.min(cut);
        }
        self.generation_known = self.generation_known.max(generation);
        (self.hidden, self.seen) = (hidden, seen);
        (self.clamped, self.passed) = (clamped, passed);
        Ok(())
    }

    /// Whether `ops` holds every op of the log.
    pub(super) fn holds_all(&self) -> bool {
        self.began == FILE_HEADER_LEN as u64 && self.base.is_none()
    }

    pub fn head(&self) -> Option<OpId> {
        (self.state.seq != 0).then_some(self.state.head)
    }

    pub fn anchor(&self) -> Option<Anchor> {
        Some(Anchor {
            op: self.head()?,
            start: self.state.head_at,
            end: self.state.head_end,
            count: self.state.seq,
        })
    }

    pub fn anchored(&self) -> bool {
        self.base.is_some()
    }

    pub fn recent(&self) -> &[Op] {
        &self.ops[self.recent_from..]
    }

    pub fn len(&self) -> usize {
        self.state.seq as usize
    }

    pub fn appended_len(&self) -> u64 {
        self.state.end
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn bytes_read(&self) -> u64 {
        self.read.get().0
    }

    pub fn file_reads(&self) -> u64 {
        self.read.get().1
    }

    pub fn clamped(&self) -> bool {
        self.clamped
    }

    pub fn reads(&self) -> u64 {
        self.reads
    }

    pub fn passed_over(&self) -> bool {
        self.passed
    }

    pub fn take_discarded(&mut self) -> Vec<Discarded> {
        std::mem::take(&mut self.discarded)
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        // best-effort durability; an explicit sync is the checked path
        if self.unsynced && !crate::relaxed_durability() {
            let _ = crate::v2::fsync(&self.file);
        }
    }
}

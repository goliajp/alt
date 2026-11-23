//! Handles on one version 2 log, driven step by step and held after each
//! step to what the file and the driver know (`v2_model`, `v2_ledger`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alt_oplog::{Anchor, Meta, OpLog, OpLogSink, SyncNamed};

use super::v2_ledger::{Entry, Ledger};
use super::v2_log::{boot, file_len};
use super::v2_meanwhile::Meanwhile;
use super::v2_model::{Model, Reference, Rng, reference};
use super::v2_sequences::{BY_BLOCK, Counts};

/// What each call gave to whoever syncs what ops name, by payload, and
/// whether that one did.
type Calls = Arc<Mutex<Vec<(Vec<Vec<u8>>, bool)>>>;

pub struct Handle {
    pub log: OpLog,
    pub read_only: bool,
    pub sink: Option<OpLogSink>,
    /// how long the file was each time the handle read it as an open does
    pub seen: Vec<u64>,
}

pub struct Run {
    dir: alt_os::temp::TempDir,
    pub rng: Rng,
    pub model: Model,
    pub ledger: Ledger,
    pub handles: Vec<Handle>,
    /// the size of a block of the log
    pub block: u64,
    calls: Calls,
    /// whoever syncs what ops name fails while this is set
    pub refuses: Arc<AtomicBool>,
    pub meanwhile: Meanwhile,
    appended: u64,
    /// the request the next ops and the next receipt are of
    pub request: u8,
    /// a cut of this step left its checkpoint synced
    pub cut_synced: bool,
    /// what was done, for a run that fails
    pub trace: Vec<String>,
    pub counts: &'static Counts,
}

/// The name an op goes by in a trace: its payload before the padding.
pub fn name(payload: &[u8]) -> String {
    let text = String::from_utf8_lossy(payload);
    text.trim_end_matches('.').to_owned()
}

/// Whoever syncs what ops name: it notes what it is given in `calls` and
/// does as `refuses` says. With `meanwhile`, another handle on the log in
/// the directory given does what is planned there while it is at it.
fn seer(
    calls: Calls,
    refuses: Arc<AtomicBool>,
    meanwhile: Option<(Meanwhile, PathBuf)>,
) -> SyncNamed {
    let alone = meanwhile
        .is_some()
        .then(|| seer(calls.clone(), refuses.clone(), None));
    Arc::new(move |ops| {
        let given = ops.iter().map(|op| op.payload.clone()).collect();
        let does = !refuses.load(Ordering::Relaxed);
        calls.lock().unwrap().push((given, does));
        if let (Some((meanwhile, dir)), Some(alone)) = (&meanwhile, &alone) {
            meanwhile.run(dir, alone);
        }
        match does {
            true => Ok(()),
            false => Err(std::io::Error::other("what the ops name is not there")),
        }
    })
}

impl Run {
    pub fn new(seed: u64, block_exp: u8) -> Run {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        alt_oplog::v2::create_with(dir.path(), block_exp).unwrap();
        Run {
            dir,
            rng: Rng(seed),
            model: Model {
                boot: 1,
                ..Model::default()
            },
            ledger: Ledger::default(),
            handles: Vec::new(),
            block: 1 << block_exp,
            calls: Calls::default(),
            refuses: Arc::default(),
            meanwhile: Meanwhile::default(),
            appended: 0,
            request: 1,
            cut_synced: false,
            trace: Vec::new(),
            counts: &BY_BLOCK[(block_exp as usize - 10) / 2],
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn log(&self) -> Reference {
        reference(self.path())
    }

    pub fn file_len(&self) -> u64 {
        file_len(self.path())
    }

    /// Takes a handle opened now among those driven, in place of one of
    /// four where as many are open.
    pub fn adopt(&mut self, mut log: OpLog, read_only: bool) -> usize {
        if self.handles.len() == 4 {
            let gone = self.rng.below(4) as usize;
            self.handles.remove(gone);
        }
        let meanwhile = (self.meanwhile.clone(), self.path().to_owned());
        log.sync_named_with(seer(
            self.calls.clone(),
            self.refuses.clone(),
            Some(meanwhile),
        ));
        let deferred = !read_only && self.rng.one_in(4);
        log.set_defer_durability(deferred);
        let sink = deferred.then(|| log.sync_handle().unwrap());
        self.trace
            .push(format!("open (read only: {read_only}, sink: {deferred})"));
        self.handles.push(Handle {
            log,
            read_only,
            sink,
            seen: vec![self.file_len()],
        });
        self.handles.len() - 1
    }

    pub fn open(&mut self, read_only: bool) -> usize {
        let opened = match read_only {
            true => OpLog::open_read_only(self.path(), None),
            false => OpLog::open(self.path()),
        };
        self.adopt(opened.expect("the log opens"), read_only)
    }

    /// A handle that may write, opened where there is none.
    pub fn writer(&mut self) -> usize {
        let writers: Vec<usize> = (0..self.handles.len())
            .filter(|index| !self.handles[*index].read_only)
            .collect();
        match writers.len() {
            0 => self.open(false),
            some => writers[self.rng.below(some as u64) as usize],
        }
    }

    /// The calls made since the last time to whoever syncs what ops
    /// name; what those that did were given is seen to.
    pub fn calls(&mut self) -> usize {
        let calls: Vec<_> = self.calls.lock().unwrap().drain(..).collect();
        for (given, did) in &calls {
            let names: Vec<String> = given.iter().map(|payload| name(payload)).collect();
            self.trace
                .push(format!("  given {names:?}, sees to it: {did}"));
            if *did {
                self.model.seen_to.extend(given.iter().cloned());
            }
        }
        calls.len()
    }

    /// A new op to append: what its frame says of it, and its payload.
    pub fn op(&mut self) -> (Meta, Vec<u8>) {
        let workspace = match self.rng.below(4) {
            0 => None,
            n => Some([n as u8; 8]),
        };
        let staged = workspace.filter(|_| !self.rng.one_in(3));
        let meta = Meta {
            workspace,
            linked: self.rng.one_in(3),
            undoable: self.rng.one_in(2),
            index_after: staged.map(|workspace| [workspace[0]; 20]),
            retired: workspace.is_some() && self.rng.one_in(30),
            request: self.rng.one_in(3).then_some([self.request; 16]),
            ..Meta::default()
        };
        let padding = match self.rng.below(20) {
            // longer than a block
            0 => self.block + self.block / 2 + self.rng.below(2 * self.block),
            _ => self.rng.below(200),
        };
        (meta, self.payload(padding))
    }

    /// How long the name of the next op is.
    pub fn next_name(&self) -> u64 {
        format!("op {}", self.appended + 1).len() as u64
    }

    /// The payload of the next op, with `padding` bytes after its name.
    pub fn payload(&mut self, padding: u64) -> Vec<u8> {
        self.appended += 1;
        let mut payload = format!("op {}", self.appended).into_bytes();
        payload.resize(payload.len() + padding as usize, b'.');
        payload
    }

    /// Takes in an op a handle appended and holds as its last, at `head`.
    pub fn appended(&mut self, payload: Vec<u8>, meta: &Meta, head: Anchor) {
        self.ledger.appended(&payload, meta, head);
        self.model.written.insert(payload, self.model.boot);
        // it read the log to its end under the lock: a cut that was
        // begun and never came to the log is forgotten
        self.ledger.begun = None;
    }

    /// An answer for whoever is asked whether what an op names is stored:
    /// yes to all, or to all but one op past where the log is synced;
    /// with `surely`, to all but one wherever there is one.
    pub fn refusal(&mut self, surely: bool) -> Option<Vec<u8>> {
        let synced = self.model.synced;
        let past: Vec<&Entry> = self
            .ledger
            .ops
            .iter()
            .filter(|op| op.at >= synced)
            .collect();
        match past.len() {
            0 => None,
            _ if !surely && self.rng.one_in(2) => None,
            some => Some(past[self.rng.below(some as u64) as usize].payload.clone()),
        }
    }
}

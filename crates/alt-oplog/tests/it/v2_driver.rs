//! Handles on one version 2 log, driven step by step and held after each
//! step to what the file and the driver know (`v2_model`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alt_oplog::{Meta, Op, OpLog, OpLogError, OpLogSink};

use super::v2_log::boot;
use super::v2_model::{Laid, Model, Reference, Rng, reference};

/// What each call gave to whoever syncs what ops name, by payload, and
/// whether that one did.
type Calls = Arc<Mutex<Vec<(Vec<Vec<u8>>, bool)>>>;

pub struct Handle {
    pub log: OpLog,
    pub read_only: bool,
    pub sink: Option<OpLogSink>,
}

pub struct Run {
    dir: alt_os::temp::TempDir,
    pub rng: Rng,
    pub model: Model,
    pub handles: Vec<Handle>,
    calls: Calls,
    /// whoever syncs what ops name fails while this is set
    pub refuses: Arc<AtomicBool>,
    appended: u64,
    cut: (u32, u64),
    /// the cut of this step died before its checkpoint was synced
    pub unsynced_cut: bool,
    /// what was done, for a run that fails
    pub trace: Vec<String>,
}

/// The name an op goes by in a trace: its payload before the padding.
pub fn name(payload: &[u8]) -> String {
    let text = String::from_utf8_lossy(payload);
    text.trim_end_matches('.').to_owned()
}

/// Whether the log still holds the last op `handle` does, where it does.
pub fn holds(handle: &OpLog, log: &Reference) -> bool {
    let Some(head) = handle.anchor() else {
        return true;
    };
    let is_head = |op: &Laid| (op.at, op.end, op.id) == (head.start, head.end, head.op);
    log.ops.iter().any(is_head)
}

/// Holds `handle` to the model: it takes the log for synced no further,
/// and has ops to look at while one it holds was left by another boot
/// and never vouched for.
fn held_to(handle: &OpLog, unsettled: &[&Laid], model: &Model, who: &str) {
    let bounded = handle.bounded_to().unwrap_or(0);
    assert!(
        bounded <= model.synced,
        "{who} takes the log for synced up to {bounded}: it is up to {}",
        model.synced
    );
    let end = handle.appended_len();
    if let Some(op) = unsettled.iter().find(|op| op.at < end) {
        assert!(
            handle.unsettled(),
            "{who} has nothing to look at: {} at {} was left by another boot \
             and nobody vouched for it",
            name(&op.payload),
            op.at
        );
    }
}

impl Run {
    pub fn new(seed: u64) -> Run {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        alt_oplog::v2::create_with(dir.path(), 10).unwrap();
        Run {
            dir,
            rng: Rng(seed),
            model: Model {
                boot: 1,
                ..Model::default()
            },
            handles: Vec::new(),
            calls: Calls::default(),
            refuses: Arc::default(),
            appended: 0,
            cut: (0, 0),
            unsynced_cut: false,
            trace: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn log(&self) -> Reference {
        reference(self.path())
    }

    /// Opens a handle, in place of one of four where as many are open.
    pub fn open(&mut self, read_only: bool) -> usize {
        if self.handles.len() == 4 {
            let gone = self.rng.below(4) as usize;
            self.handles.remove(gone);
        }
        let opened = match read_only {
            true => OpLog::open_read_only(self.path(), None),
            false => OpLog::open(self.path()),
        };
        let mut log = opened.expect("the log opens");
        let (calls, refuses) = (self.calls.clone(), self.refuses.clone());
        log.sync_named_with(Arc::new(move |ops| {
            let given = ops.iter().map(|op| op.payload.clone()).collect();
            let does = !refuses.load(Ordering::Relaxed);
            calls.lock().unwrap().push((given, does));
            match does {
                true => Ok(()),
                false => Err(std::io::Error::other("what the ops name is not there")),
            }
        }));
        let deferred = !read_only && self.rng.one_in(4);
        log.set_defer_durability(deferred);
        let sink = deferred.then(|| log.sync_handle().unwrap());
        self.trace
            .push(format!("open (read only: {read_only}, sink: {deferred})"));
        self.handles.push(Handle {
            log,
            read_only,
            sink,
        });
        self.handles.len() - 1
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

    /// What a call on handle `index` came to: nothing where it was told
    /// its ops were cut (it then reads anew) or failed as `may_fail` allows.
    pub fn took<T>(
        &mut self,
        index: usize,
        did: Result<T, OpLogError>,
        may_fail: bool,
    ) -> Option<T> {
        match did {
            Ok(value) => Some(value),
            Err(OpLogError::CutBack) => {
                self.trace.push("  it is told its ops were cut".into());
                self.handles[index]
                    .log
                    .reopen()
                    .expect("the log reads anew");
                None
            }
            Err(OpLogError::Io(_)) if may_fail => {
                self.trace.push("  it fails as it was made to".into());
                None
            }
            Err(other) => panic!("{other:?}"),
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
            ..Meta::default()
        };
        self.appended += 1;
        let mut payload = format!("op {}", self.appended).into_bytes();
        let padding = match self.rng.below(20) {
            // longer than a block
            0 => 1500 + self.rng.below(2000),
            _ => self.rng.below(200),
        };
        payload.resize(payload.len() + padding as usize, b'.');
        (meta, payload)
    }

    /// An answer for whoever is asked whether what an op names is stored:
    /// yes to all, or to all but one op past where the log is synced.
    pub fn refusal(&mut self, log: &Reference) -> Option<Vec<u8>> {
        let past: Vec<&Laid> = log
            .ops
            .iter()
            .filter(|op| op.at >= self.model.synced)
            .collect();
        match past.len() {
            0 => None,
            _ if self.rng.one_in(2) => None,
            some => Some(past[self.rng.below(some as u64) as usize].payload.clone()),
        }
    }

    /// Takes in what a look came to: the ops `asked` about, with the
    /// answers, up to the op at `stopped` where it found one not vouched
    /// for. Every op of `unsettled` before that has to be among them.
    pub fn looked(&mut self, unsettled: &[Laid], asked: &[(Vec<u8>, bool)], stopped: Option<u64>) {
        let names: Vec<String> = asked.iter().map(|(payload, _)| name(payload)).collect();
        self.trace
            .push(format!("  asked of {names:?}, stopped at {stopped:?}"));
        let vouched = asked.iter().filter(|(_, held)| *held);
        self.model
            .vouched
            .extend(vouched.map(|(payload, _)| payload.clone()));
        for op in unsettled
            .iter()
            .filter(|op| stopped.is_none_or(|at| op.at < at))
        {
            assert!(
                asked.iter().any(|(payload, _)| *payload == op.payload),
                "the look passed over {} at {}, which another boot left and nobody vouched for",
                name(&op.payload),
                op.at
            );
        }
    }

    /// Holds the note beside the log, every handle and two opened now to
    /// the model, after a step.
    pub fn held_to_the_model(&mut self) {
        self.calls();
        let log = self.log();
        // a cut syncs the checkpoint it leaves, whatever the boundary is
        if log.cut != self.cut && !self.unsynced_cut {
            self.model.fsynced = self.model.fsynced.max(log.cut.1);
        }
        (self.cut, self.unsynced_cut) = (log.cut, false);
        let claimed = alt_oplog::bounded_in(self.path()).unwrap_or(0);
        self.model.synced_to(&log, claimed);
        assert!(
            log.said_synced <= self.model.synced,
            "a frame says the log was synced up to {}: it is up to {}",
            log.said_synced,
            self.model.synced
        );
        let unsettled = self.model.unsettled(&log);
        for (index, handle) in self.handles.iter().enumerate() {
            if holds(&handle.log, &log) {
                held_to(
                    &handle.log,
                    &unsettled,
                    &self.model,
                    &format!("handle {index}"),
                );
            }
        }
        for read_only in [true, false] {
            let opened = match read_only {
                true => OpLog::open_read_only(self.path(), None),
                false => OpLog::open(self.path()),
            };
            let fresh = opened.expect("the log opens");
            held_to(&fresh, &unsettled, &self.model, "a handle opened now");
            let ops = fresh.ops().expect("the log reads from its first frame");
            let same = |(held, laid): (&Op, &Laid)| held.id == laid.id;
            assert!(ops.len() <= log.ops.len() && ops.iter().zip(&log.ops).all(same));
            assert!(ops.len() == log.ops.len() || fresh.passed_over());
            fresh.verify().expect("the log holds to the full check");
        }
    }
}

//! Sequences of what handles do to a version 2 log, drawn from a seed:
//! appends that are synced and that are not, with a look before them and
//! without, syncs, looks that vouch and that cut, appends that were never
//! finished, cuts that die at each of their steps, readers that open and
//! read on at any time, and the machine going down with a part of what
//! was not synced lost. After every step each handle is held to what the
//! file and the driver know: it takes for looked at, and for synced, no
//! more than they do.

use std::fs;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::Ordering;

use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_driver::{Run, holds, name};
use super::v2_log::boot;
use super::v2_model::Laid;

/// Vouches for every op but the one of the payload `refused`, and notes
/// in `asked` each op it is asked about with its answer.
fn says<'a>(
    refused: &'a Option<Vec<u8>>,
    asked: &'a mut Vec<(Vec<u8>, bool)>,
) -> impl FnMut(&Op) -> Result<bool, OpLogError> + 'a {
    move |op| {
        let held = refused.as_ref() != Some(&op.payload);
        asked.push((op.payload.clone(), held));
        Ok(held)
    }
}

impl Run {
    fn unsettled_before(&self, end: u64) -> Vec<Laid> {
        let log = self.log();
        let left = self.model.unsettled(&log);
        left.into_iter().filter(|op| op.at < end).cloned().collect()
    }

    /// A writer looks at what was left, as it does before it appends, and
    /// cuts what is not vouched for. With `dies`, a cut ends after that
    /// step of it as if the process had died: the handle is gone then.
    fn settle(&mut self, index: usize, dies: Option<u8>) -> bool {
        let unsettled = self.unsettled_before(u64::MAX);
        let log = self.log();
        let refused = self.refusal(&log);
        let refuses = refused.as_deref().map(name);
        self.trace.push(format!(
            "handle {index} settles, refusing {refuses:?}, dies: {dies:?}"
        ));
        let mut asked = Vec::new();
        alt_oplog::v2::cut_dies_after(dies);
        let cut = self.handles[index].log.settle(says(&refused, &mut asked));
        alt_oplog::v2::cut_dies_after(None);
        // what it was told before it died it was told
        let died = dies.is_some() && matches!(cut, Err(OpLogError::Io(_)));
        let cut = self.took(index, cut, died);
        if died {
            self.unsynced_cut = dies.is_some_and(|step| step < 3);
            let vouched = asked.iter().filter(|(_, held)| *held);
            self.model
                .vouched
                .extend(vouched.map(|(payload, _)| payload.clone()));
            self.handles.remove(index);
            return false;
        }
        match cut {
            Some(cut) => self.looked(&unsettled, &asked, cut.map(|cut| cut.at)),
            None => return false,
        }
        true
    }

    fn append(&mut self) {
        let index = self.writer();
        if self.rng.one_in(3) && !self.settle(index, None) {
            return;
        }
        let unsettled = self.unsettled_before(u64::MAX);
        let (meta, payload) = self.op();
        let unsynced = self.rng.one_in(2);
        let fails = !unsynced && self.rng.one_in(8);
        self.trace.push(format!(
            "handle {index} appends {} (workspace {:?}, linked: {}, staged: {}, retired: {}), \
             unsynced: {unsynced}",
            name(&payload),
            meta.workspace.map(|workspace| workspace[0]),
            meta.linked,
            meta.index_after.is_some(),
            meta.retired
        ));
        self.calls();
        self.refuses.store(fails, Ordering::Relaxed);
        let left = unsynced.then(|| alt_os::durability::leave_unsynced(true));
        let appended = self.handles[index]
            .log
            .append_with("driver", 1, &payload, &meta);
        drop(left);
        self.refuses.store(false, Ordering::Relaxed);
        let seen_to = self.calls() != 0;
        if self.took(index, appended, fails).is_none() {
            return;
        }
        self.model.written.insert(payload, self.model.boot);
        // a writer that syncs appends after nothing that is not seen to
        if let Some(op) = unsettled.first().filter(|_| !unsynced && !seen_to) {
            panic!(
                "appended after {} at {}, which another boot left and nobody vouched for",
                name(&op.payload),
                op.at
            );
        }
        if !unsynced && self.rng.one_in(2) {
            self.sync(index, 0);
        }
    }

    /// Has the handle sync what it holds: itself, through its sink, or
    /// as a writer does that sets a boundary over all the log holds.
    fn sync(&mut self, index: usize, how: u64) {
        let fails = self.rng.one_in(8);
        self.trace
            .push(format!("handle {index} syncs (way {how}), fails: {fails}"));
        self.refuses.store(fails, Ordering::Relaxed);
        let handle = &mut self.handles[index];
        let mut synced = match how {
            0 => handle.log.sync(),
            _ => handle.log.bound_now(),
        };
        if let (Ok(()), Some(sink)) = (&synced, &handle.sink) {
            synced = sink.fsync();
        }
        self.refuses.store(false, Ordering::Relaxed);
        self.took(index, synced, fails);
    }

    /// Any handle looks at what was left, as a reader does.
    fn look(&mut self) {
        let index = self.rng.below(self.handles.len() as u64) as usize;
        let log = self.log();
        if !holds(&self.handles[index].log, &log) {
            return;
        }
        let end = self.handles[index].log.appended_len();
        let unsettled = self.unsettled_before(end);
        let refused = self.refusal(&log);
        let refuses = refused.as_deref().map(name);
        self.trace
            .push(format!("handle {index} looks, refusing {refuses:?}"));
        let mut asked = Vec::new();
        let found = self.handles[index]
            .log
            .unconfirmed(says(&refused, &mut asked));
        if let Some(found) = self.took(index, found, false) {
            self.looked(&unsettled, &asked, found.map(|found| found.at));
        }
    }

    /// Any handle reads what was appended since it last read.
    fn read_on(&mut self) {
        let index = self.rng.below(self.handles.len() as u64) as usize;
        let cuts = !self.handles[index].read_only && self.rng.one_in(2);
        self.trace
            .push(format!("handle {index} reads on, as a writer: {cuts}"));
        let handle = &mut self.handles[index];
        let read = match cuts {
            true => handle.log.refresh(),
            false => handle.log.follow(),
        };
        self.took(index, read, false);
    }

    /// A writer dies in the middle of an append: it has read the log to
    /// its end under the lock, as every writer does, and leaves there any
    /// bytes, or the first bytes of a frame.
    fn tear(&mut self) {
        let mut writer = OpLog::open(self.path()).expect("the log opens");
        writer.refresh().expect("a writer reads the log to its end");
        drop(writer);
        let path = self.path().join("log");
        let whole = fs::read(&path).unwrap();
        let mut torn: Vec<u8> = (0..1 + self.rng.below(120))
            .map(|_| self.rng.next() as u8)
            .collect();
        if let Some(last) = self.log().ops.last().filter(|_| self.rng.one_in(2)) {
            let frame = &whole[last.at as usize..last.end as usize];
            torn = frame[..1 + self.rng.below(frame.len() as u64 - 1) as usize].to_vec();
        }
        self.trace.push(format!(
            "{} bytes of no frame at {}",
            torn.len(),
            whole.len()
        ));
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&torn).unwrap();
    }

    /// The machine goes down: the handles are gone, and of what lies past
    /// where the log is synced a part may be.
    fn reboot(&mut self) {
        self.handles.clear();
        let path = self.path().join("log");
        let len = fs::metadata(&path).unwrap().len();
        let from = self.model.fsynced.max(64);
        let lost = match self.rng.below(4) {
            0 if len > from => {
                let kept = from + self.rng.below(len - from + 1);
                let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.set_len(kept).unwrap();
                format!("the file ends at {kept}")
            }
            1 if len > from => {
                let start = from + self.rng.below(len - from);
                let end = start + 1 + self.rng.below(len - start);
                let mut bytes = fs::read(&path).unwrap();
                bytes[start as usize..end as usize].fill(0);
                fs::write(&path, bytes).unwrap();
                format!("{start}..{end} is zeros")
            }
            _ => "nothing is lost".to_owned(),
        };
        let log = self.log();
        self.model.rebooted(&log);
        boot(self.model.boot);
        self.trace
            .push(format!("boot {} of {len} bytes: {lost}", self.model.boot));
    }

    fn step(&mut self) {
        if self.handles.is_empty() {
            self.open(false);
        }
        match self.rng.below(100) {
            0..8 => {
                let read_only = self.rng.one_in(2);
                self.open(read_only);
            }
            8..12 => {
                let gone = self.rng.below(self.handles.len() as u64) as usize;
                self.trace.push(format!("handle {gone} is closed"));
                self.handles.remove(gone);
            }
            12..46 => self.append(),
            46..56 => {
                let (index, how) = (self.writer(), self.rng.below(2));
                self.sync(index, how);
            }
            56..64 => self.read_on(),
            64..74 => self.look(),
            74..80 => {
                let index = self.writer();
                self.settle(index, None);
            }
            80..86 => self.tear(),
            86..93 => self.reboot(),
            _ => {
                let (index, dies) = (self.writer(), 1 + self.rng.below(5) as u8);
                self.settle(index, Some(dies));
            }
        }
    }
}

/// Runs the sequence of `steps` steps that `seed` gives, and tells what
/// was done when a step of it fails.
fn run(seed: u64, steps: usize) {
    let mut run = Run::new(seed);
    let ran = catch_unwind(AssertUnwindSafe(|| {
        for _ in 0..steps {
            run.step();
            run.held_to_the_model();
        }
    }));
    alt_oplog::v2::cut_dies_after(None);
    if let Err(panic) = ran {
        eprintln!("the sequence of seed {seed}:\n{}", run.trace.join("\n"));
        resume_unwind(panic);
    }
}

#[test]
fn whatever_handles_do_none_takes_for_looked_at_or_synced_what_the_file_and_its_writers_do_not() {
    const SEQUENCES: u64 = 4000;
    const THREADS: u64 = 8;
    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            scope.spawn(move || {
                for seed in (thread..SEQUENCES).step_by(THREADS as usize) {
                    run(seed, 40);
                }
            });
        }
    });
}

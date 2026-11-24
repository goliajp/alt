//! What a loss of power cannot take from a driven version 2 log, kept by
//! the driver itself: the bytes of the file as they were when a handle
//! last asked for it to be fsynced, for as far as they are the same
//! since. A sequence plays the loss of power on what the kernel holds of
//! the file, so the fsyncs its handles ask for need not be made: they
//! are counted, and told to the driver. One sequence in sixteen has them
//! made as well.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::v2_driver::Run;

pub struct Power {
    /// the file as it was when it was last fsynced
    held: Arc<Mutex<Vec<u8>>>,
    /// a process is being taken away: what its handles would fsync as
    /// they are dropped is neither made nor held
    dying: Arc<AtomicBool>,
    /// the fsyncs of this sequence are made
    pub real: bool,
}

impl Power {
    /// Has the fsyncs of the logs of this thread told to what is given
    /// back, and left out unless the sequence of `seed` is one that has
    /// them made.
    pub fn of(seed: u64) -> Power {
        let power = Power {
            held: Arc::default(),
            dying: Arc::default(),
            real: seed.is_multiple_of(16),
        };
        let (held, dying) = (power.held.clone(), power.dying.clone());
        alt_oplog::v2::fsyncs_left_out(!power.real);
        alt_oplog::v2::at_fsync(Some(Box::new(move |file: &File| {
            if dying.load(Ordering::Relaxed) {
                return;
            }
            let mut bytes = vec![0; file.metadata().unwrap().len() as usize];
            file.read_exact_at(&mut bytes, 0).unwrap();
            *held.lock().unwrap() = bytes;
        })));
        power
    }

    /// Has the fsyncs of this thread made again, and told to nobody.
    pub fn off() {
        alt_oplog::v2::fsyncs_left_out(false);
        alt_oplog::v2::at_fsync(None);
    }

    /// Drops `gone`, the handles of a process that died: nothing they
    /// would fsync on their way out is.
    fn takes<T>(&self, gone: T) {
        self.dying.store(true, Ordering::Relaxed);
        alt_oplog::v2::fsyncs_left_out(true);
        drop(gone);
        alt_oplog::v2::fsyncs_left_out(!self.real);
        self.dying.store(false, Ordering::Relaxed);
    }
}

/// How many bytes `was` and `is` begin with that are the same.
pub fn same_to(was: &[u8], is: &[u8]) -> usize {
    let mut same = 0;
    for (was, is) in was.chunks(4096).zip(is.chunks(4096)) {
        if was != is {
            let differs = was.iter().zip(is).position(|(was, is)| was != is);
            return same + differs.unwrap_or(was.len().min(is.len()));
        }
        same += was.len();
    }
    same
}

impl Run {
    /// Up to where no loss of power takes a byte of the file: it was
    /// fsynced with these bytes, past its header, and has them still.
    pub fn durable_to(&self) -> u64 {
        let now = std::fs::read(self.path().join("log")).unwrap();
        let held = self.power.held.lock().unwrap();
        match held.len().min(now.len()) {
            0..64 => 64,
            _ => 64 + same_to(&held[64..], &now[64..]) as u64,
        }
    }

    /// The process that held handle `index` died.
    pub fn dies(&mut self, index: usize) {
        let gone = self.handles.remove(index);
        self.power.takes(gone);
    }

    /// The machine went down, and every handle with it.
    pub fn all_die(&mut self) {
        let gone = std::mem::take(&mut self.handles);
        self.power.takes(gone);
    }
}
